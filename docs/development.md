# Development

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

That's the gate CLAUDE.md requires before any change is considered done — run
it in this order. `cargo test` covers unit, render, and subprocess tests plus
doctests; it does not need a live Postgres.

For changes that touch live-database behaviour, also run the integration
suite against a real Postgres:

```bash
docker compose -f docker-compose.test.yml up -d
cargo test --features integration
docker compose -f docker-compose.test.yml down
```

The size sweep (`tests/sizes.rs`) renders every screen at four terminal
sizes and is part of the default `cargo test` run — no separate command
needed, but `cargo test --test sizes` runs it alone if you're only chasing a
layout regression.

CI (`.github/workflows/ci.yml`) additionally runs `cargo-deny` (advisories,
licences), `cargo-machete` (unused dependencies), an MSRV build/test on Rust
1.94.1, and the [candor](https://github.com/tombaldwin/candor) DB-boundary
check (`ci/candor-check.sh`) enforcing that direct database access stays in
`src/conn.rs` and `src/query/`. See `CONTRIBUTING.md` for the full pre-PR
checklist and `CLAUDE.md` for the AI-assisted-contributor rules.

## Distribution

- **Cargo**: `cargo install pgman` from crates.io, or `cargo install --path .`
  from a checkout. Not published yet — the first release is v0.1.0, which has
  not happened yet.
- **GitHub Releases**: tagging `v<X.Y.Z>` triggers
  `.github/workflows/release.yml`, which builds release binaries for
  `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
  `aarch64-apple-darwin`, and `x86_64-apple-darwin`, and attaches tarballs +
  SHA-256 checksums to a draft release. Publishing to crates.io happens once
  the maintainer flips that draft to published.
- **Homebrew**: tap lives at
  [`tombaldwin/homebrew-tap`](https://github.com/tombaldwin/homebrew-tap).
  Per-release, the formula there is bumped by `scripts/update-formula.sh`
  (added separately from this workflow) rather than by hand.
