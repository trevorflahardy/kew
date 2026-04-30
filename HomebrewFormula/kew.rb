class Kew < Formula
  desc "Real local agent orchestration"
  homepage "https://github.com/trevorflahardy/kew"
  license "MIT"
  version "1.0.1"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-aarch64-apple-darwin.tar.gz"
      sha256 "0515da19fd9146294d938b33bce2d7d3a1a2e7cf9aec86fe237a21d92575a2d0"
    else
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-x86_64-apple-darwin.tar.gz"
      sha256 "da7795cd62db37ba0430aeedf590869921744f6c7b11f2cd7319b90c8f032be1"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "e23d85213f2bc06651ec683112e019094659d3328b153967444af36f604bf0cc"
    else
      url "https://github.com/trevorflahardy/kew/releases/download/v#{version}/kew-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "e4762684e9430bfca931c6dcee060ae8e26472866b070e51fea4305e89381ed2"
    end
  end

  def install
    bin.install "kew"
  end

  test do
    assert_match "kew", shell_output("#{bin}/kew --version")
  end
end
