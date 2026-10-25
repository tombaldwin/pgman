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
  version "0.2.1"
  license "MIT OR Apache-2.0"

  if OS.mac?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "794ed75e9fb99e412a628c3124c66a3f037fc576a0baa0018eb096cc118c0fc7"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "d91108e7ce4df57ae0919bfe7c91960114539e394ad7a1b177645f319f8d1c37"
    end
  elsif OS.linux?
    if Hardware::CPU.arm?
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "936beea68546ff27036d36ebc8e27715a9e2106accd83721acb9c973769a4ee0"
    else
      url "https://github.com/tombaldwin/pgman/releases/download/v#{version}/pgman-v#{version}-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "b266088f08315610f59ec4eaa21c2fe3d6b1d6996b740d5734800a951bda6095"
    end
  end

  def install
    bin.install "pgman"
    prefix.install "README.md", "LICENSE-MIT", "LICENSE-APACHE"
  end

  test do
    # pgman prints "pgman <version> · beta" — a substring match, since the
    # trailing beta tag isn't part of the version this formula tracks.
    assert_match "pgman #{version}", shell_output("#{bin}/pgman --version")
  end
end
