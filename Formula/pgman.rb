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
  version "0.5.4"
  license "MIT OR Apache-2.0"

  # pgman dump and restore run pg_dump and pg_restore, from libpq. It is
  # keg-only (nothing on PATH), so the wrapper below tells pgman where.
  depends_on "libpq"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "96cad31aef88ecbe5b2103d2a8f2415b5d42d3042c70da26d4c8e07f8fdbdcdd"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "9002c1d76f7cee176b11fba833da007b3d2c3e701c0053f9c39a0f58d67f476a"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "b7c104e64c6baa29ca69eb5090c282051caa6895f7752304664b893d8ab5cb81"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "d6d898711994e633c4b4cdf87d243adf437d6bdbea147a9b68c5882638cf7737"
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
