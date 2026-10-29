# Homebrew formula for pgman — k9s-style Postgres TUI for Java/AWS shops.
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
  desc "k9s-style Postgres TUI for Java/AWS shops"
  homepage "https://github.com/tombaldwin/pgman"
  version "0.4.0"
  license "MIT OR Apache-2.0"

  # pgman dump and restore run pg_dump and pg_restore, from libpq. It is
  # keg-only (nothing on PATH), so the wrapper below tells pgman where.
  depends_on "libpq"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "1d74ee50d042438c14fb7126f91ecedddce34080998d991e7aef7d05bbafab09"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "72e933d43caff354eac985af9260023c44a26658e7f3ba4f9954c6634b2f1e7d"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "485d8124a3e62fb22e11e2ccb04c772a23b624bf99091aa37fd8e35a5a7134b9"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "396878d9b6468b755bb3ea2d5e59e6f4741cb2a2022398e9bec66a840c458407"
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
