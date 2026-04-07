class Kew < Formula
  desc "Real local agent orchestration"
  homepage "https://github.com/trevorflahardy/kew"
  license "MIT"
  version "VERSION_PLACEHOLDER"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_AARCH64_DARWIN"
    else
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_X86_64_DARWIN"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "SHA256_AARCH64_LINUX"
    else
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "SHA256_X86_64_LINUX"
    end
  end

  def install
    bin.install "kew"
  end

  test do
    assert_match "kew", shell_output("#{bin}/kew --version")
  end
end
