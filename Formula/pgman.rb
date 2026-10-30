# Homebrew formula for pgman — k9s-style terminal UI for PostgreSQL.
#
# This in-repo copy is for `brew install --formula ./Formula/pgman.rb`
# (local-checkout install). The canonical user-facing install path will
# be the tap at https://github.com/tombaldwin/homebrew-tap:
#   brew tap tombaldwin/tap
#   brew install pgman
#
# After each release: run `scripts/update-formula.sh v<X.Y.Z>` — it
# computes SHA-256s from the GitHub Release tarballs and writes both this
# file and the tap's Formula/pgman.rb in one go, creating the tap entry if
# it doesn't exist yet.
class Pgman < Formula
  desc "k9s-style terminal UI for PostgreSQL"
  homepage "https://github.com/tombaldwin/pgman"
  version "0.4.2"
  license "MIT OR Apache-2.0"

  # pgman dump and restore run pg_dump and pg_restore, from libpq. It is
  # keg-only (nothing on PATH), so the wrapper below tells pgman where.
  depends_on "libpq"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "abdf7ef0f7830fead3ae77db1ab5c964a370688311d9cfdc360485c6d6be2b5b"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "63b457322a28009abcb820d9a24a4b25843c13b932c8ed572a972f65006b91ec"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "feb77abd21f48c97258017e1f0688479fa1e57e5fda6c11464665a4f3dcfbda7"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "5ef6d4a9dfafa4456087e0f1791c5d381d8febd0d02b5a62d46a08084b846706"
    end
  end

  def install
    libexec.install "pgman"
    # opt_bin, not the versioned keg: it follows libpq's upgrades. A
    # PGMAN_PG_BIN of the user's own still wins (the wrapper's value is
    # double-quoted, so bash expands it as it runs).
    (bin/"pgman").write_env_script libexec/"pgman",
                                   PGMAN_PG_BIN: "${PGMAN_PG_BIN:-#{formula_opt_bin("libpq")}}"
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  test do
    # pgman prints "pgman <version> · beta" — a substring match, since the
    # trailing beta tag isn't part of the version this formula tracks.
    assert_match "pgman #{version}", shell_output("#{bin}/pgman --version")
    # The tools dump and restore run are where the wrapper says.
    assert_path_exists formula_opt_bin("libpq")/"pg_dump"
    assert_path_exists formula_opt_bin("libpq")/"pg_restore"
    assert_match "PGMAN_PG_BIN", (bin/"pgman").read
  end
end
