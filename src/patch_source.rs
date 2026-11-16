//! Where a patch directory's files are read from: the working tree, or a
//! commit named by a git ref (a tag, a branch, a SHA).
//!
//! A checkout is whatever its owner last pulled or switched to: a stale
//! `main` hides patches a release brings, and a feature branch offers one
//! a week before the code that needs it. Read at a ref, the files are the
//! commit's, through `git ls-tree` and `git cat-file` — nothing checks
//! out, and the working tree and index are never touched. Read from the
//! working tree inside a repository, what makes it differ from its
//! commit (untracked or modified files, a branch other than the default,
//! commits behind its upstream) comes back as warnings.
//!
//! git runs through `util::git()`, and only commands that run no filters
//! or hooks: `rev-parse`, `symbolic-ref`, `rev-list`, `ls-tree`,
//! `cat-file --batch` (which never applies a filter). The working tree is
//! compared with HEAD byte for byte here, not with `git status`, which
//! may run a clean filter.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A patch file bigger than this is refused rather than read.
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

/// Where a [`Snapshot`]'s files came from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Source {
    #[default]
    WorkTree,
    /// `git_ref` as given, resolved once to the commit `sha` (full).
    Commit { git_ref: String, sha: String },
}

impl Source {
    /// `the working tree`, or `demo-12 (5a17c0de2)`. Pure.
    pub fn describe(&self) -> String {
        match self {
            Source::WorkTree => "the working tree".to_string(),
            Source::Commit { git_ref, sha } => {
                let short = short_sha(sha);
                if git_ref == sha || sha.starts_with(git_ref.as_str()) {
                    short.to_string()
                } else {
                    format!("{git_ref} ({short})")
                }
            }
        }
    }

    pub fn sha(&self) -> Option<&str> {
        match self {
            Source::WorkTree => None,
            Source::Commit { sha, .. } => Some(sha),
        }
    }

    pub fn git_ref(&self) -> Option<&str> {
        match self {
            Source::WorkTree => None,
            Source::Commit { git_ref, .. } => Some(git_ref),
        }
    }
}

/// A commit's first nine hex digits, for people. Pure.
pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(9)]
}

/// A patch directory's `*.sql` files, read once, from one place.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    pub source: Source,
    /// File name → text.
    pub texts: BTreeMap<String, String>,
    /// What the reader should know about where they came from.
    pub warnings: Vec<String>,
    /// The directory is inside a git repository, so a ref could have
    /// named a commit instead.
    pub in_git: bool,
    /// A working-tree read in a checkout: its branch and commit
    /// (`branch main at 5a17c0de2`), for the operator to see.
    pub checkout: Option<String>,
}

impl Snapshot {
    /// Each file parsed for its id, in name order.
    pub fn patch_files(
        &self,
        cfg: &crate::query::patches::PatchesConfig,
    ) -> Vec<crate::query::patches::PatchFile> {
        self.texts
            .iter()
            .map(|(name, text)| crate::query::patches::patch_file(name, text, cfg))
            .collect()
    }

    /// Read from the working tree of a repository, with no ref given.
    pub fn unpinned(&self) -> bool {
        self.in_git && self.source == Source::WorkTree
    }
}

/// Whether `r` is a ref this module passes to git: no option (`-…`), no
/// whitespace or control characters, no `:` (a path inside a commit),
/// nothing over 200 bytes. git decides whether it names a commit. Pure.
pub fn check_ref(r: &str) -> Result<(), String> {
    if r.is_empty() {
        return Err("ref is empty".to_string());
    }
    if r.len() > 200 {
        return Err("ref is longer than 200 characters".to_string());
    }
    if r.starts_with('-') {
        return Err(format!("ref {r:?} starts with '-'"));
    }
    if let Some(c) = r
        .chars()
        .find(|c| c.is_whitespace() || c.is_control() || *c == ':')
    {
        return Err(format!("ref {r:?} contains {c:?}"));
    }
    Ok(())
}

/// The `*.sql` files in `dir`: at `git_ref`'s commit when given, else
/// the working tree. Blocking: git runs.
pub fn read(dir: &Path, git_ref: Option<&str>) -> Result<Snapshot, String> {
    let repo = repo_of(dir);
    match git_ref {
        Some(r) => {
            check_ref(r)?;
            let (top, prefix) = repo?.ok_or_else(|| {
                format!(
                    "{} isn't inside a git repository, so there is no ref {r:?} to read it at",
                    dir.display()
                )
            })?;
            let sha = resolve_commit(&top, r)?;
            let entries = tree_entries(&top, &sha, &prefix)?;
            if entries.is_empty() {
                return Err(format!(
                    "{r} ({}) has no {} directory, or nothing in it",
                    short_sha(&sha),
                    display_prefix(&prefix)
                ));
            }
            let (texts, warnings) = read_entries(&top, entries, &format!("at {r}"))?;
            Ok(Snapshot {
                source: Source::Commit {
                    git_ref: r.to_string(),
                    sha,
                },
                texts,
                warnings,
                in_git: true,
                checkout: None,
            })
        }
        None => {
            let texts = read_work_tree(dir)?;
            let (warnings, checkout) = match &repo {
                Ok(Some((top, prefix))) => {
                    let (w, c) = work_tree_warnings(top, prefix, &texts);
                    (w, Some(c))
                }
                Ok(None) => (Vec::new(), None),
                Err(e) => (
                    vec![format!(
                        "the files are in a git checkout git couldn't read: {e}"
                    )],
                    Some("a checkout git couldn't read".to_string()),
                ),
            };
            Ok(Snapshot {
                source: Source::WorkTree,
                texts,
                warnings,
                checkout,
                // A checkout git can't read is still a checkout: the
                // rules for one hold rather than lapse.
                in_git: !matches!(repo, Ok(None)),
            })
        }
    }
}

fn display_prefix(prefix: &str) -> &str {
    match prefix.trim_end_matches('/') {
        "" => ".",
        p => p,
    }
}

/// Every `*.sql` file directly in `dir`, following symlinks.
fn read_work_tree(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
    let mut out = BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        if !is_sql(&name) || !path.is_file() {
            continue;
        }
        let bytes = read_capped(&path).map_err(|e| format!("cannot read {name}: {e}"))?;
        out.insert(name.clone(), text_of(&name, bytes)?);
    }
    Ok(out)
}

/// A file's bytes, but never more than one past [`MAX_FILE_BYTES`]: a
/// bigger one is refused by its size, without reading it all.
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn is_sql(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".sql")
}

/// A patch file's bytes as text: UTF-8, and not past [`MAX_FILE_BYTES`].
fn text_of(name: &str, bytes: Vec<u8>) -> Result<String, String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(too_big(name));
    }
    String::from_utf8(bytes).map_err(|_| format!("{name} isn't UTF-8 text"))
}

fn too_big(name: &str) -> String {
    format!("{name} is bigger than a patch file pgman reads ({MAX_FILE_BYTES} bytes)")
}

/// git in `dir`, its stdout on success, its stderr's first line on
/// failure.
fn git_out(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::util::git()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err
            .lines()
            .next()
            .unwrap_or("git failed")
            .trim()
            .to_string())
    }
}

/// The repository `dir` is in: its top level and `dir`'s path inside it
/// (`a/b/`, or empty at the top). `Ok(None)` when no directory from
/// `dir` up holds a `.git`; an error when one does and git can't read it
/// (git missing, a repository it calls of dubious ownership…), so the
/// caller doesn't take a checkout for a plain directory.
fn repo_of(dir: &Path) -> Result<Option<(PathBuf, String)>, String> {
    let abs = dir
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", dir.display()))?;
    if !abs.ancestors().any(|a| a.join(".git").exists()) {
        return Ok(None);
    }
    let out = git_out(&abs, &["rev-parse", "--show-toplevel", "--show-prefix"])?;
    let mut lines = out.lines();
    let top = PathBuf::from(lines.next().unwrap_or("").trim());
    if top.as_os_str().is_empty() {
        return Err("git named no top level".to_string());
    }
    let prefix = lines.next().unwrap_or("").trim().to_string();
    Ok(Some((top, prefix)))
}

/// `r`'s commit, as a full SHA.
fn resolve_commit(top: &Path, r: &str) -> Result<String, String> {
    let spec = format!("{r}^{{commit}}");
    match git_out(top, &["rev-parse", "--verify", "--quiet", &spec]) {
        Ok(out) if !out.trim().is_empty() => Ok(out.trim().to_string()),
        _ => Err(format!(
            "no commit {r:?} in {}: a tag or branch from elsewhere needs fetching first (`git fetch --tags`)",
            top.display()
        )),
    }
}

/// One entry of `git ls-tree -z`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    pub mode: String,
    pub kind: String,
    pub oid: String,
    /// Its name inside the directory listed.
    pub name: String,
}

/// `git ls-tree -z` output, names with `prefix` taken off. An entry
/// whose name isn't UTF-8 is left out, as the working tree's reader
/// leaves such a file out. Pure.
pub fn parse_ls_tree(out: &[u8], prefix: &str) -> Result<Vec<TreeEntry>, String> {
    let mut entries = Vec::new();
    for record in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let Ok(record) = std::str::from_utf8(record) else {
            continue;
        };
        let (meta, path) = record
            .split_once('\t')
            .ok_or_else(|| format!("unexpected ls-tree line {record:?}"))?;
        let mut parts = meta.split(' ');
        let (Some(mode), Some(kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(format!("unexpected ls-tree line {record:?}"));
        };
        let name = path.strip_prefix(prefix).unwrap_or(path);
        entries.push(TreeEntry {
            mode: mode.to_string(),
            kind: kind.to_string(),
            oid: oid.to_string(),
            name: name.to_string(),
        });
    }
    Ok(entries)
}

/// The entries directly in `prefix` at `commit`.
fn tree_entries(top: &Path, commit: &str, prefix: &str) -> Result<Vec<TreeEntry>, String> {
    let mut args = vec!["ls-tree", "-z", commit];
    if !prefix.is_empty() {
        args.extend(["--", prefix]);
    }
    let out = crate::util::git()
        .arg("-C")
        .arg(top)
        .args(&args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git ls-tree {commit}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    parse_ls_tree(&out.stdout, prefix)
}

/// The `*.sql` blobs among `entries`, read; a symlink or anything not a
/// plain file is skipped with a warning naming `at`.
fn read_entries(
    top: &Path,
    entries: Vec<TreeEntry>,
    at: &str,
) -> Result<(BTreeMap<String, String>, Vec<String>), String> {
    let mut warnings = Vec::new();
    let mut files = Vec::new();
    for e in entries.into_iter().filter(|e| is_sql(&e.name)) {
        if e.kind == "blob" && (e.mode == "100644" || e.mode == "100755") {
            files.push(e);
        } else {
            warnings.push(format!(
                "{} {at} is not a plain file (mode {}), so it was left out",
                e.name, e.mode
            ));
        }
    }
    let blobs = cat_blobs(top, &files)?;
    let mut texts = BTreeMap::new();
    for (e, bytes) in files.iter().zip(blobs) {
        texts.insert(e.name.clone(), text_of(&e.name, bytes)?);
    }
    Ok((texts, warnings))
}

/// Each of `files`' objects, by `git cat-file --batch` (no filters
/// run), read as it streams: one past [`MAX_FILE_BYTES`] stops it.
fn cat_blobs(top: &Path, files: &[TreeEntry]) -> Result<Vec<Vec<u8>>, String> {
    use std::io::Write;
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let mut child = crate::util::git()
        .arg("-C")
        .arg(top)
        .args(["cat-file", "--batch"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let input: String = files.iter().map(|e| format!("{}\n", e.oid)).collect();
    let mut stdin = child.stdin.take().ok_or("git cat-file has no stdin")?;
    // Written from its own thread: git answers as it reads, and a full
    // stdout pipe would otherwise stall both sides.
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let stdout = child.stdout.take().ok_or("git cat-file has no stdout")?;
    let names: Vec<&str> = files.iter().map(|e| e.name.as_str()).collect();
    let parsed = parse_cat_file_batch(std::io::BufReader::new(stdout), &names, MAX_FILE_BYTES);
    if parsed.is_err() {
        // Its output is no longer read: stop it rather than wait.
        let _ = child.kill();
    }
    let _ = writer.join();
    let status = child.wait().map_err(|e| format!("git cat-file: {e}"))?;
    let blobs = parsed?;
    if !status.success() {
        return Err("git cat-file failed".to_string());
    }
    Ok(blobs)
}

/// `git cat-file --batch` output, one object per name in `names`:
/// `<oid> <type> <size>\n<content>\n`, or `<oid> missing\n` for one it
/// lacks. Read as it streams, so an object over `max` bytes is refused
/// by its header, before its content is read. Pure but for `r`.
pub fn parse_cat_file_batch<R: std::io::BufRead>(
    mut r: R,
    names: &[&str],
    max: usize,
) -> Result<Vec<Vec<u8>>, String> {
    use std::io::{BufRead, Read};
    let ended = |e: std::io::Error| format!("git cat-file output ended early: {e}");
    let mut blobs = Vec::with_capacity(names.len());
    for name in names {
        let mut header = Vec::new();
        (&mut r)
            .take(512)
            .read_until(b'\n', &mut header)
            .map_err(ended)?;
        if header.pop() != Some(b'\n') {
            return Err("git cat-file output ended early".to_string());
        }
        let header = std::str::from_utf8(&header).map_err(|_| "bad git cat-file header")?;
        let fields: Vec<&str> = header.split(' ').collect();
        let [oid, kind, size] = fields[..] else {
            return Err(format!("{name}: git cat-file says {header:?}"));
        };
        if kind != "blob" {
            return Err(format!("{name} ({oid}) is a {kind}, not a file"));
        }
        let size: usize = size
            .parse()
            .map_err(|_| format!("bad git cat-file size in {header:?}"))?;
        if size > max {
            return Err(too_big(name));
        }
        let mut body = vec![0; size];
        r.read_exact(&mut body).map_err(ended)?;
        let mut nl = [0u8];
        r.read_exact(&mut nl).map_err(ended)?;
        if nl[0] != b'\n' {
            return Err("git cat-file output is out of step".to_string());
        }
        blobs.push(body);
    }
    Ok(blobs)
}

/// How a working-tree read of `prefix` differs from its commit: files
/// not in HEAD, changed since, or missing; a branch other than the
/// default; commits behind or ahead of its upstream (as of the last
/// fetch). Each check that git can't answer is left out.
fn work_tree_warnings(
    top: &Path,
    prefix: &str,
    texts: &BTreeMap<String, String>,
) -> (Vec<String>, String) {
    let mut warnings = Vec::new();
    let head = tree_entries(top, "HEAD", prefix).and_then(|entries| {
        let (texts, _) = read_entries(top, entries, "in HEAD")?;
        Ok(texts)
    });
    if let Ok(head) = head {
        warnings.extend(drift(texts, &head));
    }
    let branch = git_out(top, &["symbolic-ref", "--quiet", "--short", "HEAD"])
        .ok()
        .map(|b| b.trim().to_string());
    let default = git_out(top, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .ok()
        .map(|d| d.trim().trim_start_matches("origin/").to_string())
        .filter(|d| !d.is_empty() && d != "origin/HEAD" && d != "HEAD");
    let upstream = git_out(top, &["rev-parse", "--abbrev-ref", "@{upstream}"])
        .ok()
        .map(|u| u.trim().to_string());
    let counts = upstream.as_ref().and_then(|_| {
        let out = git_out(
            top,
            &["rev-list", "--left-right", "--count", "HEAD...@{upstream}"],
        )
        .ok()?;
        let mut n = out.split_whitespace().map(|s| s.parse::<u64>().ok());
        Some((n.next()??, n.next()??))
    });
    let head_short = git_out(top, &["rev-parse", "--short=9", "HEAD"])
        .ok()
        .map(|s| s.trim().to_string());
    warnings.extend(branch_warnings(
        branch.as_deref(),
        head_short.as_deref(),
        default.as_deref(),
        upstream.as_deref().zip(counts),
    ));
    (
        warnings,
        checkout_line(branch.as_deref(), head_short.as_deref()),
    )
}

/// The checkout's branch and commit, for people: `branch main at
/// 5a17c0de2`, `5a17c0de2 (no branch)`, `branch main, no commit yet`.
/// Pure.
pub fn checkout_line(branch: Option<&str>, head: Option<&str>) -> String {
    match (branch, head) {
        (Some(b), Some(h)) => format!("branch {b} at {h}"),
        (Some(b), None) => format!("branch {b}, no commit yet"),
        (None, Some(h)) => format!("{h} (no branch)"),
        (None, None) => "no commit".to_string(),
    }
}

/// The working tree's `*.sql` files against HEAD's: untracked, modified,
/// deleted. Pure.
pub fn drift(work: &BTreeMap<String, String>, head: &BTreeMap<String, String>) -> Vec<String> {
    let untracked: Vec<&str> = work
        .keys()
        .filter(|k| !head.contains_key(*k))
        .map(String::as_str)
        .collect();
    let modified: Vec<&str> = work
        .iter()
        .filter(|(k, v)| head.get(*k).is_some_and(|h| h != *v))
        .map(|(k, _)| k.as_str())
        .collect();
    let missing: Vec<&str> = head
        .keys()
        .filter(|k| !work.contains_key(*k))
        .map(String::as_str)
        .collect();
    let mut out = Vec::new();
    if !untracked.is_empty() {
        out.push(format!("not committed: {}", untracked.join(", ")));
    }
    if !modified.is_empty() {
        out.push(format!(
            "changed since the last commit: {}",
            modified.join(", ")
        ));
    }
    if !missing.is_empty() {
        out.push(format!(
            "committed but missing from the working tree: {}",
            missing.join(", ")
        ));
    }
    out
}

/// What the checkout's branch says: detached, not the default branch,
/// behind or ahead of its upstream (`(name, (ahead, behind))`). Pure.
pub fn branch_warnings(
    branch: Option<&str>,
    head: Option<&str>,
    default: Option<&str>,
    upstream: Option<(&str, (u64, u64))>,
) -> Vec<String> {
    let mut out = Vec::new();
    match branch {
        None => out.push(format!(
            "the checkout is not on a branch (HEAD is {})",
            head.unwrap_or("detached")
        )),
        Some(b) => {
            if let Some(d) = default.filter(|d| *d != b) {
                out.push(format!("the checkout is on branch {b}, not {d}"));
            }
        }
    }
    if let Some((up, (ahead, behind))) = upstream {
        let on = branch.unwrap_or("HEAD");
        if behind > 0 {
            out.push(format!(
                "{on} is {behind} commit{} behind {up} as of the last fetch",
                if behind == 1 { "" } else { "s" }
            ));
        }
        if ahead > 0 {
            out.push(format!(
                "{on} has {ahead} commit{} not on {up}",
                if ahead == 1 { "" } else { "s" }
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_ref_that_could_be_an_option_or_a_path_is_refused() {
        for ok in [
            "main",
            "origin/main",
            "v1.2.3",
            "release-12",
            "5a17c0de2",
            "HEAD~2",
            "a^",
        ] {
            assert!(check_ref(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-c",
            "--upload-pack=x",
            "HEAD:schema/1.sql",
            "a b",
            "a\nb",
            &"x".repeat(201),
        ] {
            assert!(check_ref(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ls_tree_entries_lose_their_directory() {
        let out = b"100644 blob aaa\tdb/patches/001-a.sql\x00120000 blob bbb\tdb/patches/002-link.sql\x00040000 tree ccc\tdb/patches/old\x00";
        let e = parse_ls_tree(out, "db/patches/").unwrap();
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].name, "001-a.sql");
        assert_eq!((e[1].mode.as_str(), e[1].oid.as_str()), ("120000", "bbb"));
        assert_eq!(e[2].kind, "tree");
        assert!(parse_ls_tree(b"garbage\0", "").is_err());
        // One name that isn't UTF-8 leaves out that entry, not the read.
        let mixed = b"100644 blob aaa\tp/\xff.sql\x00100644 blob bbb\tp/002.sql\x00";
        let e = parse_ls_tree(mixed, "p/").unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "002.sql");
        assert!(parse_ls_tree(b"", "x/").unwrap().is_empty());
    }

    #[test]
    fn cat_file_batch_splits_on_sizes_not_newlines() {
        let parse = |out: &[u8], names: &[&str]| parse_cat_file_batch(out, names, 100);
        let out = b"aaa blob 5\nab\ncd\nbbb blob 0\n\n";
        assert_eq!(
            parse(out, &["a.sql", "b.sql"]).unwrap(),
            vec![b"ab\ncd".to_vec(), Vec::new()]
        );
        assert!(parse(b"aaa missing\n", &["a.sql"])
            .unwrap_err()
            .contains("a.sql"));
        assert!(parse(b"aaa blob 9\nshort\n", &["a.sql"]).is_err());
        assert!(parse(b"aaa tree 3\nabc\n", &["a.sql"]).is_err());
        assert!(parse(b"aaa blob 1\nx\n", &["a.sql", "b.sql"]).is_err());
        assert!(parse(b"aaa blob 1\nxy", &["a.sql"]).is_err());
        // A size that would overflow, and one over the cap: refused by
        // the header, nothing allocated for the content.
        let huge = format!("aaa blob {}\n", usize::MAX);
        assert!(parse(huge.as_bytes(), &["a.sql"])
            .unwrap_err()
            .contains("bigger than"));
        assert!(parse(b"aaa blob 101\n", &["big.sql"])
            .unwrap_err()
            .starts_with("big.sql is bigger"));
    }

    #[test]
    fn a_working_tree_file_over_the_cap_is_refused_unread() {
        let dir =
            std::env::temp_dir().join(format!("pgman-patch-source-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = std::fs::File::create(dir.join("001-big.sql")).unwrap();
        // Sparse: big by its length, nothing written.
        f.set_len(MAX_FILE_BYTES as u64 * 64).unwrap();
        assert_eq!(
            read_capped(&dir.join("001-big.sql")).unwrap().len(),
            MAX_FILE_BYTES + 1
        );
        let e = read_work_tree(&dir).unwrap_err();
        assert!(e.starts_with("001-big.sql is bigger"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drift_names_untracked_changed_and_missing_files() {
        let work = map(&[("001.sql", "a"), ("002.sql", "b2"), ("004.sql", "new")]);
        let head = map(&[("001.sql", "a"), ("002.sql", "b"), ("003.sql", "c")]);
        assert_eq!(
            drift(&work, &head),
            vec![
                "not committed: 004.sql".to_string(),
                "changed since the last commit: 002.sql".to_string(),
                "committed but missing from the working tree: 003.sql".to_string(),
            ]
        );
        assert!(drift(&head, &head).is_empty());
    }

    #[test]
    fn branch_warnings_say_off_default_detached_behind_and_ahead() {
        assert!(branch_warnings(
            Some("main"),
            None,
            Some("main"),
            Some(("origin/main", (0, 0)))
        )
        .is_empty());
        assert_eq!(
            branch_warnings(Some("feature-x"), None, Some("main"), None),
            vec!["the checkout is on branch feature-x, not main"]
        );
        assert_eq!(
            branch_warnings(None, Some("abc123456"), Some("main"), None),
            vec!["the checkout is not on a branch (HEAD is abc123456)"]
        );
        assert_eq!(
            branch_warnings(Some("main"), None, None, Some(("origin/main", (1, 5)))),
            vec![
                "main is 5 commits behind origin/main as of the last fetch",
                "main has 1 commit not on origin/main"
            ]
        );
    }

    #[test]
    fn checkout_line_names_the_branch_and_commit() {
        assert_eq!(
            checkout_line(Some("main"), Some("5a17c0de2")),
            "branch main at 5a17c0de2"
        );
        assert_eq!(
            checkout_line(None, Some("5a17c0de2")),
            "5a17c0de2 (no branch)"
        );
        assert_eq!(
            checkout_line(Some("main"), None),
            "branch main, no commit yet"
        );
    }

    #[test]
    fn a_commit_source_describes_its_ref_and_short_sha() {
        let sha = "5a17c0de2aaaabbbbccccddddeeeeffff0000111";
        let c = Source::Commit {
            git_ref: "demo-12".into(),
            sha: sha.into(),
        };
        assert_eq!(c.describe(), "demo-12 (5a17c0de2)");
        let by_sha = Source::Commit {
            git_ref: "5a17c0d".into(),
            sha: sha.into(),
        };
        assert_eq!(by_sha.describe(), "5a17c0de2");
        assert_eq!(Source::WorkTree.describe(), "the working tree");
    }

    /// A scratch repository: `None` when git isn't installed.
    fn repo(tag: &str) -> Option<PathBuf> {
        let root =
            std::env::temp_dir().join(format!("pgman-patch-source-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("db/patches")).ok()?;
        let ok = git(&root, &["init", "-q", "-b", "main"]);
        ok.then_some(root)
    }

    /// Setup's git: plain, unlike `util::git()`, which refuses to clone.
    fn git(root: &Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    fn write(root: &Path, name: &str, text: &str) {
        std::fs::write(root.join("db/patches").join(name), text).unwrap();
    }

    #[test]
    fn a_ref_reads_its_commit_and_never_the_working_tree() {
        let Some(root) = repo("ref") else { return };
        write(&root, "001-a.sql", "one");
        write(&root, "002-b.sql", "two");
        assert!(git(&root, &["add", "."]));
        assert!(git(&root, &["commit", "-q", "-m", "first"]));
        assert!(git(&root, &["tag", "release-1"]));
        // After the tag: a committed patch, an uncommitted one, an edit.
        write(&root, "003-c.sql", "three");
        assert!(git(&root, &["add", "."]));
        assert!(git(&root, &["commit", "-q", "-m", "second"]));
        write(&root, "004-d.sql", "four");
        write(&root, "001-a.sql", "one, edited");
        let dir = root.join("db/patches");

        let at = read(&dir, Some("release-1")).unwrap();
        assert_eq!(at.texts, map(&[("001-a.sql", "one"), ("002-b.sql", "two")]));
        assert_eq!(at.source.git_ref(), Some("release-1"));
        assert_eq!(at.source.sha().map(str::len), Some(40));
        assert!(!at.unpinned());

        let head = read(&dir, Some("HEAD")).unwrap();
        assert_eq!(head.texts.len(), 3);
        assert_eq!(head.texts["001-a.sql"], "one");

        let work = read(&dir, None).unwrap();
        assert_eq!(work.texts.len(), 4);
        assert!(
            work.checkout
                .as_deref()
                .is_some_and(|c| c.starts_with("branch main at ")),
            "{:?}",
            work.checkout
        );
        assert_eq!(at.checkout, None);
        assert!(work.unpinned());
        assert!(
            work.warnings
                .contains(&"not committed: 004-d.sql".to_string()),
            "{:?}",
            work.warnings
        );
        assert!(work
            .warnings
            .contains(&"changed since the last commit: 001-a.sql".to_string()));

        assert!(read(&dir, Some("no-such-tag"))
            .unwrap_err()
            .contains("no commit"));
        assert!(read(&dir, Some("--output=/tmp/x")).is_err());
        // The working tree is as it was left.
        assert_eq!(
            std::fs::read_to_string(dir.join("001-a.sql")).unwrap(),
            "one, edited"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A replace ref (`git replace <release> <other>`) makes git read one
    /// commit's objects as another's; the files at a ref are its own.
    #[test]
    fn a_replace_ref_does_not_change_what_a_ref_reads() {
        let Some(root) = repo("replace") else { return };
        write(&root, "001-a.sql", "SELECT 1;");
        assert!(git(&root, &["add", "."]) && git(&root, &["commit", "-q", "-m", "release"]));
        assert!(git(&root, &["tag", "release-1"]));
        write(&root, "001-a.sql", "DROP TABLE customers;");
        assert!(git(&root, &["commit", "-q", "-am", "other"]));
        assert!(git(&root, &["replace", "release-1", "HEAD"]));
        let at = read(&root.join("db/patches"), Some("release-1")).unwrap();
        assert_eq!(at.texts["001-a.sql"], "SELECT 1;");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// In a partial clone, a blob not fetched yet is missing: reading it
    /// must not fetch it, which would run the remote's transport from the
    /// checkout's config (here an ssh command that leaves a mark).
    #[cfg(unix)]
    #[test]
    fn a_partial_clones_missing_file_is_not_fetched() {
        use std::os::unix::fs::PermissionsExt;
        let Some(origin) = repo("lazy-origin") else {
            return;
        };
        write(&origin, "001-a.sql", "SELECT 1;");
        assert!(git(&origin, &["add", "."]) && git(&origin, &["commit", "-q", "-m", "one"]));
        assert!(git(&origin, &["config", "uploadpack.allowFilter", "true"]));
        let clone = origin.with_file_name(format!(
            "pgman-patch-source-lazy-clone-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&clone);
        let url = format!("file://{}", origin.display());
        let cloned = git(
            &origin,
            &[
                "clone",
                "-q",
                "--no-checkout",
                "--filter=blob:none",
                &url,
                clone.to_str().unwrap(),
            ],
        );
        assert!(cloned, "a partial clone over file:// needs git 2.19+");
        std::fs::create_dir_all(clone.join("db/patches")).unwrap();
        let mark = clone.join("fetched");
        let ssh = clone.join("ssh.sh");
        std::fs::write(
            &ssh,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", mark.display()),
        )
        .unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(git(
            &clone,
            &["config", "remote.origin.url", "ssh://example.invalid/repo"]
        ));
        assert!(git(
            &clone,
            &["config", "core.sshCommand", ssh.to_str().unwrap()]
        ));

        let got = read(&clone.join("db/patches"), Some("HEAD"));
        assert!(
            !mark.exists(),
            "reading a ref fetched, running the checkout's ssh command"
        );
        assert!(got.is_err(), "a missing blob reads as missing: {got:?}");
        let _ = std::fs::remove_dir_all(&clone);
        let _ = std::fs::remove_dir_all(&origin);
    }

    #[test]
    fn a_directory_outside_git_reads_the_working_tree_and_refuses_a_ref() {
        let dir =
            std::env::temp_dir().join(format!("pgman-patch-source-nogit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("001.sql"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "y").unwrap();
        // GIT_CEILING_DIRECTORIES isn't ours to set here; a temp dir
        // inside some repository is unlikely, and skipped if so.
        if !matches!(repo_of(&dir), Ok(None)) {
            return;
        }
        let s = read(&dir, None).unwrap();
        assert_eq!(s.texts, map(&[("001.sql", "x")]));
        assert!(!s.in_git && s.warnings.is_empty());
        assert!(read(&dir, Some("main"))
            .unwrap_err()
            .contains("isn't inside a git repository"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_checkout_git_cannot_read_still_counts_as_one() {
        let dir =
            std::env::temp_dir().join(format!("pgman-patch-source-badgit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("db")).unwrap();
        // A `.git` file pointing nowhere: git refuses it.
        std::fs::write(dir.join(".git"), "gitdir: /nonexistent/pgman-test\n").unwrap();
        std::fs::write(dir.join("db/001.sql"), "x").unwrap();
        let s = read(&dir.join("db"), None).unwrap();
        assert!(s.unpinned(), "{s:?}");
        assert!(
            s.warnings[0].starts_with("the files are in a git checkout git couldn't read"),
            "{:?}",
            s.warnings
        );
        assert!(read(&dir.join("db"), Some("main")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
