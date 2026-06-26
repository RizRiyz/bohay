# Homebrew formula for bohay. Build-from-source (pure Rust, no system libs).
#
# Quickest path (works right now, no release needed):
#   brew install --HEAD RizRiyz/bohay/bohay
#
# Stable releases: after you `git tag v0.1.0 && git push --tags`, set `sha256`
# below to the tag tarball's checksum:
#   curl -sL https://github.com/RizRiyz/bohay/archive/refs/tags/v0.1.0.tar.gz | shasum -a 256
class Bohay < Formula
  desc "Terminal Workspace Manager for Next-Gen Agents — single-binary terminal multiplexer"
  homepage "https://github.com/RizRiyz/bohay"
  url "https://github.com/RizRiyz/bohay/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/RizRiyz/bohay.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/bohay --version")
  end
end
