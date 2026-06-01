class GitAi < Formula
  desc "AI-powered git attribution and authorship tracking"
  homepage "https://github.com/git-ai-project/git-ai"
  version "__VERSION__"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/__REPO__/releases/download/v__VERSION__/git-ai-macos-arm64"
      sha256 "__SHA256_MACOS_ARM64__"
    end
    on_intel do
      url "https://github.com/__REPO__/releases/download/v__VERSION__/git-ai-macos-x64"
      sha256 "__SHA256_MACOS_X64__"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/__REPO__/releases/download/v__VERSION__/git-ai-linux-arm64"
      sha256 "__SHA256_LINUX_ARM64__"
    end
    on_intel do
      url "https://github.com/__REPO__/releases/download/v__VERSION__/git-ai-linux-x64"
      sha256 "__SHA256_LINUX_X64__"
    end
  end

  def install
    binary_name = stable.url.split("/").last
    bin.install binary_name => "git-ai"
    # Install git shim that routes through git-ai via argv[0] dispatch
    bin.install binary_name => "git"
  end

  def caveats
    <<~EOS
      git-ai has been installed with a `git` shim.
      The shim takes PATH precedence so all git commands route through git-ai.
      Run `git-ai install-hooks` to set up IDE/agent hooks.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/git-ai --version")
  end
end
