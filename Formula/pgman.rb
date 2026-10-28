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
  version "0.3.0"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "1c39140902a737eec254896565cc8a3af1f939e4e933081be6fd27c84384acf3"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "7bb287bfee867af1987203a198092b24039d1401076259b08b6d1973e3c9c2ef"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "c070b44e61e4f177acd91a45ec4169df84a63cd474bd3bc1e98fb5399ac70b53"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "12e62f13746078ccdbe740059103287b295ab7e5770de3383eb25833c53d49df"
    end
  end

  def install
    bin.install "pgman"
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  def caveats
    <<~EOS
      pgman dump and pgman restore run your own pg_dump and pg_restore.
      If you don't have them yet: brew install libpq
    EOS
  end

  test do
    # pgman prints "pgman <version> · beta" — a substring match, since the
    # trailing beta tag isn't part of the version this formula tracks.
    assert_match "pgman #{version}", shell_output("#{bin}/pgman --version")
  end
end
