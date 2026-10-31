//! Encrypting a dump: an environment's `encrypt = "…"`, the operator's age
//! keys, and the KMS envelope. See `docs/design/masked-dumps.md`, section
//! 5.
//!
//! Every file of a dump but its plain header is encrypted with the stock
//! `age` crate to one X25519 recipient:
//!
//! - `age:age1…` — the operator's own key, made by `pgman key new` and
//!   kept in pgman's config directory (`keys/`, 0600). Restoring needs the
//!   matching identity there, or in `PGMAN_AGE_IDENTITY_FILE`.
//! - `kms:<key>` — a fresh identity for each dump, wrapped by AWS KMS with
//!   the header's fields as the encryption context and kept, wrapped, in
//!   the header. Restoring is a `kms:Decrypt`: an IAM decision and a
//!   CloudTrail line, gone with a leaver's access.
//! - `none` — the operator's explicit opt-out.
//!
//! The encryption key is never the masking key, nor derived from it.
//! Secrets never reach `tracing` or an error message.

use age::secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// `encrypt = "…"` on an environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encrypt {
    None,
    /// An age recipient, `age1…`.
    Age(String),
    /// A KMS key: an id, an `alias/…`, or an ARN.
    Kms(String),
}

/// `none`, `age:age1…` or `kms:<key>`, checked. Pure.
pub fn parse(s: &str) -> Result<Encrypt, String> {
    let s = s.trim();
    if s == "none" {
        return Ok(Encrypt::None);
    }
    if let Some(r) = s.strip_prefix("age:") {
        return r
            .parse::<age::x25519::Recipient>()
            .map(|_| Encrypt::Age(r.to_string()))
            .map_err(|_| {
                format!("encrypt {s:?}: not an age public key (age1…); `pgman key new` makes one")
            });
    }
    if let Some(k) = s.strip_prefix("kms:") {
        if crate::aws_cli::plain_arg(k, ":/_-.", 2048) {
            return Ok(Encrypt::Kms(k.to_string()));
        }
        return Err(format!("encrypt {s:?}: not a KMS key id, alias/… or ARN"));
    }
    Err(format!(
        "encrypt {s:?}: give kms:<key id or alias/…>, age:<public key>, or none"
    ))
}

/// How a dump was encrypted, in its plain header. Holds no secret: a KMS
/// identity is there only wrapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seal {
    /// `age` or `kms`.
    pub kind: String,
    /// The age recipient every file is encrypted to.
    pub recipient: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kms_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kms_region: Option<String>,
    /// The identity, wrapped by KMS, base64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped: Option<String>,
}

impl Seal {
    /// One line for a summary: what can open it. Pure.
    pub fn describe(&self) -> String {
        match (self.kind.as_str(), &self.kms_key) {
            ("kms", Some(k)) => format!("encrypted with KMS key {k}"),
            _ => format!("encrypted to age key {}", short(&self.recipient)),
        }
    }
}

/// `age1abcd…wxyz`. Pure.
pub fn short(recipient: &str) -> String {
    if recipient.len() <= 16 {
        return recipient.to_string();
    }
    format!("{}…{}", &recipient[..9], &recipient[recipient.len() - 4..])
}

/// Runs `aws` with arguments and stdin; stdout back. [`crate::aws_cli`]'s
/// in pgman, a fake in tests.
pub type AwsRun<'a> = &'a dyn Fn(&[String], &[u8]) -> Result<String, String>;

/// The KMS encryption context, as the CLI takes it: JSON. Pure.
fn context_json(context: &BTreeMap<String, String>) -> String {
    serde_json::to_string(context).unwrap_or_default()
}

/// `aws kms encrypt …`, the plaintext on stdin. Pure.
pub fn kms_encrypt_args(
    key: &str,
    scope: &crate::aws_cli::AwsScope,
    context: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "kms",
        "encrypt",
        "--key-id",
        key,
        "--plaintext",
        "fileb:///dev/stdin",
        "--encryption-context",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(context_json(context));
    args.extend(["--query".to_string(), "CiphertextBlob".to_string()]);
    args.extend(scope.common_args("text"));
    args
}

/// `aws kms decrypt …`, the ciphertext on stdin. Pure.
pub fn kms_decrypt_args(
    key: &str,
    scope: &crate::aws_cli::AwsScope,
    context: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "kms",
        "decrypt",
        "--key-id",
        key,
        "--ciphertext-blob",
        "fileb:///dev/stdin",
        "--encryption-context",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.push(context_json(context));
    args.extend(["--query".to_string(), "Plaintext".to_string()]);
    args.extend(scope.common_args("text"));
    args
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

/// A new dump's seal under `enc`, and the recipient to encrypt to: `None`
/// for `none`. `context` is the header's fields, bound into a KMS wrap.
pub fn seal(
    enc: &Encrypt,
    scope: &crate::aws_cli::AwsScope,
    context: &BTreeMap<String, String>,
    aws: AwsRun,
) -> Result<Option<(Seal, age::x25519::Recipient)>, String> {
    use base64::Engine;
    match enc {
        Encrypt::None => Ok(None),
        Encrypt::Age(r) => {
            let to = r
                .parse::<age::x25519::Recipient>()
                .map_err(|_| format!("encrypt: {r:?} is not an age public key"))?;
            Ok(Some((
                Seal {
                    kind: "age".into(),
                    recipient: r.clone(),
                    kms_key: None,
                    kms_region: None,
                    wrapped: None,
                },
                to,
            )))
        }
        Encrypt::Kms(key) => {
            scope.validate()?;
            let identity = age::x25519::Identity::generate();
            let to = identity.to_public();
            let secret = identity.to_string();
            let out = aws(
                &kms_encrypt_args(key, scope, context),
                secret.expose_secret().as_bytes(),
            )
            .map_err(|e| format!("encrypting the dump's key with KMS: {e}"))?;
            let wrapped = out.trim().to_string();
            if wrapped.is_empty() || b64().decode(&wrapped).is_err() {
                return Err("KMS returned no ciphertext for the dump's key".into());
            }
            Ok(Some((
                Seal {
                    kind: "kms".into(),
                    recipient: to.to_string(),
                    kms_key: Some(key.clone()),
                    kms_region: scope.region.clone(),
                    wrapped: Some(wrapped),
                },
                to,
            )))
        }
    }
}

/// The identity that opens `seal`: the operator's key, or KMS's unwrap of
/// the dump's own, checked against the recipient the files were
/// encrypted to.
pub fn open(
    seal: &Seal,
    scope: &crate::aws_cli::AwsScope,
    context: &BTreeMap<String, String>,
    aws: AwsRun,
) -> Result<age::x25519::Identity, String> {
    use base64::Engine;
    let identity = match seal.kind.as_str() {
        "age" => return find_identity(&seal.recipient),
        "kms" => {
            let key = seal.kms_key.as_deref().ok_or("the dump names no KMS key")?;
            let wrapped = seal
                .wrapped
                .as_deref()
                .ok_or("the dump holds no wrapped key")?;
            let blob = b64()
                .decode(wrapped)
                .map_err(|_| "the dump's wrapped key isn't base64".to_string())?;
            let mut scope = scope.clone();
            if scope.region.is_none() {
                scope.region = seal.kms_region.clone();
            }
            scope.validate()?;
            let out = aws(&kms_decrypt_args(key, &scope, context), &blob).map_err(|e| {
                format!(
                    "KMS wouldn't decrypt the dump's key (its header may have been edited): {e}"
                )
            })?;
            let plain = b64()
                .decode(out.trim())
                .map_err(|_| "KMS returned a key pgman can't read".to_string())?;
            let text = String::from_utf8(plain)
                .map_err(|_| "KMS returned a key pgman can't read".to_string())?;
            text.trim()
                .parse::<age::x25519::Identity>()
                .map_err(|_| "KMS returned a key pgman can't read".to_string())?
        }
        other => {
            return Err(format!(
                "the dump is encrypted as {other:?}, which this pgman doesn't read"
            ))
        }
    };
    if identity.to_public().to_string() != seal.recipient {
        return Err("the key KMS returned doesn't open this dump".into());
    }
    Ok(identity)
}

/// Where the operator's age keys live.
pub fn keys_dir() -> PathBuf {
    crate::util::config_dir().join("keys")
}

/// Make a new age key named `name` in [`keys_dir`]: its identity file
/// (0600, the format `age -d -i` reads) and its public key, which goes in
/// `encrypt = "age:…"`.
pub fn new_key(name: &str) -> Result<(PathBuf, String), String> {
    if !crate::aws_cli::plain_arg(name, "_-.", 64) {
        return Err(format!(
            "key name {name:?}: letters, digits, '_', '-' and '.' only"
        ));
    }
    let dir = keys_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(format!("{name}.txt"));
    let identity = age::x25519::Identity::generate();
    let public = identity.to_public().to_string();
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => {
            format!("{} exists: pgman doesn't replace a key", path.display())
        }
        _ => format!("{}: {e}", path.display()),
    })?;
    let secret = identity.to_string();
    writeln!(f, "# pgman dump key {name}\n# public key: {public}")
        .and_then(|_| writeln!(f, "{}", secret.expose_secret()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((path, public))
}

/// The identities in an age identity file's text: one per line, `#`
/// comments and blank lines skipped. Pure.
fn identities_in(text: &str) -> Vec<age::x25519::Identity> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.parse().ok())
        .collect()
}

/// The operator's identity for `recipient`: from `PGMAN_AGE_IDENTITY_FILE`,
/// then each file in [`keys_dir`].
pub fn find_identity(recipient: &str) -> Result<age::x25519::Identity, String> {
    let mut files: Vec<PathBuf> = std::env::var_os("PGMAN_AGE_IDENTITY_FILE")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    if let Ok(entries) = std::fs::read_dir(keys_dir()) {
        let mut found: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        found.sort();
        files.extend(found);
    }
    for f in files {
        let Ok(text) = std::fs::read_to_string(&f) else {
            continue;
        };
        if let Some(id) = identities_in(&text)
            .into_iter()
            .find(|id| id.to_public().to_string() == recipient)
        {
            return Ok(id);
        }
    }
    Err(format!(
        "no key here opens this dump: it is encrypted to {}, and neither {} nor \
         PGMAN_AGE_IDENTITY_FILE holds its identity",
        short(recipient),
        keys_dir().display()
    ))
}

/// A file being written, encrypted when there's a recipient.
pub enum Sink {
    Plain(std::io::BufWriter<std::fs::File>),
    Sealed(age::stream::StreamWriter<std::io::BufWriter<std::fs::File>>),
}

impl Sink {
    /// Write to `file`, encrypted to `to` when given.
    pub fn new(file: std::fs::File, to: Option<&age::x25519::Recipient>) -> std::io::Result<Sink> {
        let w = std::io::BufWriter::new(file);
        let Some(to) = to else {
            return Ok(Sink::Plain(w));
        };
        let enc = age::Encryptor::with_recipients(std::iter::once(to as &dyn age::Recipient))
            .map_err(std::io::Error::other)?;
        Ok(Sink::Sealed(enc.wrap_output(w)?))
    }

    /// Flush and close: an encrypted file isn't whole until this returns.
    pub fn finish(self) -> std::io::Result<()> {
        match self {
            Sink::Plain(mut w) => w.flush(),
            Sink::Sealed(w) => w.finish()?.flush(),
        }
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Sink::Plain(w) => w.write(buf),
            Sink::Sealed(w) => w.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Sink::Plain(w) => w.flush(),
            Sink::Sealed(w) => w.flush(),
        }
    }
}

/// Read `file`, decrypted with `key` when given.
pub fn source(
    file: std::fs::File,
    key: Option<&age::x25519::Identity>,
) -> std::io::Result<Box<dyn Read + Send>> {
    let r = std::io::BufReader::new(file);
    let Some(key) = key else {
        return Ok(Box::new(r));
    };
    let dec = age::Decryptor::new(r).map_err(std::io::Error::other)?;
    let read = dec
        .decrypt(std::iter::once(key as &dyn age::Identity))
        .map_err(std::io::Error::other)?;
    Ok(Box::new(read))
}

/// The whole of `path`, decrypted with `key` when given.
pub fn read_all(path: &Path, key: Option<&age::x25519::Identity>) -> Result<Vec<u8>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut out = Vec::new();
    source(f, key)
        .and_then(|mut r| r.read_to_end(&mut out))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_settings_parse_and_refuse_what_isnt_a_key() {
        let id = age::x25519::Identity::generate();
        let r = id.to_public().to_string();
        assert_eq!(parse(&format!("age:{r}")), Ok(Encrypt::Age(r.clone())));
        assert_eq!(parse("none"), Ok(Encrypt::None));
        assert_eq!(
            parse("kms:alias/pgman-dumps"),
            Ok(Encrypt::Kms("alias/pgman-dumps".into()))
        );
        assert!(parse("kms:arn:aws:kms:eu-west-2:123456789012:key/abcd-1234").is_ok());
        for bad in [
            "age:age1nope",
            "kms:--debug",
            "kms:",
            "aes:x",
            "",
            "kms:a b",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn what_is_written_sealed_reads_back_with_its_key_only() {
        let dir = std::env::temp_dir().join(format!("pgman-crypt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let id = age::x25519::Identity::generate();
        let path = dir.join("f");
        let mut s =
            Sink::new(std::fs::File::create(&path).unwrap(), Some(&id.to_public())).unwrap();
        s.write_all(b"rows of a copy").unwrap();
        s.finish().unwrap();
        let raw = std::fs::read(&path).unwrap();
        assert!(raw.starts_with(b"age-encryption.org/v1"), "an age file");
        assert!(!raw.windows(4).any(|w| w == b"rows"), "no plaintext");
        assert_eq!(read_all(&path, Some(&id)).unwrap(), b"rows of a copy");
        let other = age::x25519::Identity::generate();
        assert!(read_all(&path, Some(&other)).is_err());
        // Without a recipient it is the plain bytes.
        let plain = dir.join("p");
        let mut s = Sink::new(std::fs::File::create(&plain).unwrap(), None).unwrap();
        s.write_all(b"schema").unwrap();
        s.finish().unwrap();
        assert_eq!(read_all(&plain, None).unwrap(), b"schema");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn ctx() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("copy".to_string(), "c-1".to_string()),
            ("database".to_string(), "ledger".to_string()),
        ])
    }

    /// A KMS that "wraps" by reversing the bytes, and refuses a different
    /// encryption context.
    fn fake_kms(args: &[String], input: &[u8]) -> Result<String, String> {
        use base64::Engine;
        let at = args
            .iter()
            .position(|a| a == "--encryption-context")
            .unwrap();
        let context = &args[at + 1];
        let want = context_json(&ctx());
        match args[1].as_str() {
            "encrypt" => {
                let mut v = input.to_vec();
                v.reverse();
                v.extend_from_slice(context.as_bytes());
                Ok(b64().encode(v))
            }
            "decrypt" => {
                let mut v = input.to_vec();
                if !v.ends_with(want.as_bytes()) || context != &want {
                    return Err("InvalidCiphertextException".into());
                }
                v.truncate(v.len() - want.len());
                v.reverse();
                Ok(b64().encode(v))
            }
            _ => Err("unexpected".into()),
        }
    }

    #[test]
    fn a_kms_seal_wraps_a_fresh_key_and_opens_only_with_its_context() {
        let scope = crate::aws_cli::AwsScope {
            region: Some("eu-west-2".into()),
            ..Default::default()
        };
        let (seal, to) = seal(
            &Encrypt::Kms("alias/pgman-dumps".into()),
            &scope,
            &ctx(),
            &fake_kms,
        )
        .unwrap()
        .unwrap();
        assert_eq!(seal.kind, "kms");
        assert_eq!(seal.recipient, to.to_string());
        let header = serde_json::to_string(&seal).unwrap();
        assert!(
            !header.contains("AGE-SECRET-KEY"),
            "no secret in the header"
        );
        let id = open(
            &seal,
            &crate::aws_cli::AwsScope::default(),
            &ctx(),
            &fake_kms,
        )
        .unwrap();
        assert_eq!(id.to_public().to_string(), seal.recipient);
        // An edited header changes the context: KMS refuses.
        let mut edited = ctx();
        edited.insert("database".into(), "other".into());
        assert!(open(&seal, &scope, &edited, &fake_kms).is_err());
        // A wrapped key that opens to another identity is refused.
        let mut swapped = seal.clone();
        swapped.recipient = age::x25519::Identity::generate().to_public().to_string();
        assert!(open(&swapped, &scope, &ctx(), &fake_kms).is_err());
    }

    #[test]
    fn kms_arguments_put_the_secret_on_stdin_never_the_command_line() {
        let scope = crate::aws_cli::AwsScope::default();
        let a = kms_encrypt_args("alias/k", &scope, &ctx());
        assert_eq!(&a[..4], ["kms", "encrypt", "--key-id", "alias/k"]);
        assert!(a.contains(&"fileb:///dev/stdin".to_string()));
        assert!(a.contains(&r#"{"copy":"c-1","database":"ledger"}"#.to_string()));
        let d = kms_decrypt_args("alias/k", &scope, &ctx());
        assert_eq!(&d[..2], ["kms", "decrypt"]);
        assert!(d.contains(&"--ciphertext-blob".to_string()));
    }

    #[test]
    fn identities_are_found_by_their_public_key() {
        let a = age::x25519::Identity::generate();
        let b = age::x25519::Identity::generate();
        let text = format!(
            "# a comment\n\n{}\n{}\nnot a key\n",
            a.to_string().expose_secret(),
            b.to_string().expose_secret()
        );
        let found = identities_in(&text);
        assert_eq!(found.len(), 2);
        assert_eq!(found[1].to_public().to_string(), b.to_public().to_string());
    }

    #[test]
    fn short_keeps_the_ends_of_a_public_key() {
        assert_eq!(short("age1abcdefghijklmnopqrstuvwxyz"), "age1abcde…wxyz");
        assert_eq!(short("age1short"), "age1short");
    }
}
