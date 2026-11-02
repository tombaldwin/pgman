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
  version "0.5.0"
  license "MIT OR Apache-2.0"

  # pgman dump and restore run pg_dump and pg_restore, from libpq. It is
  # keg-only (nothing on PATH), so the wrapper below tells pgman where.
  depends_on "libpq"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "0af5a3c44959fc4b1afada67c3b1409919804f69b3a8cdbcf40cee831e89afba"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "3869f8b6c83cb830a43e238c44b1f4db50ddfe500112c519354670e4a409a360"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "81ea8c0f1f0ac4c6a355a1dada91f7e039587ef5e7e088c5fbec09b46f8380c8"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "6bc3034ff352d5c8287be654fdbf51c52d18f3a124e7d3ce8f52b2bbddc933a9"
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
