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
  version "0.1.0"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "dd7b475552549cab697730085425be2a57afe7bdd6073ebbc0cced9aea609576"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "774e4437c52e4865a2dbc3345c07b089243306260605a6bcb24362741f18c781"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "8b64717a06dbb16e7a745fee53bfa0280be6b75571c0497de8827d73961a401a"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b221858217ce4c66ba2e48491cb4afc1774df08c39614aaca78c2163d82103c0"
    end
  end

  def install
    bin.install "pgman"
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  test do
    # pgman prints "pgman 0.1.0 · beta" — a substring match, since the
    # trailing beta tag isn't part of the version this formula tracks.
    assert_match "pgman #{version}", shell_output("#{bin}/pgman --version")
  end
end
