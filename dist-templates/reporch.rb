# Homebrew formula for reporch.
#
# AUTHORED BUT NEVER EXECUTED — no tap exists and nothing has been published. cargo-dist
# regenerates this from the release artifacts and opens a PR against sezeryavuz/homebrew-tap;
# this copy is committed so the formula is reviewable before any of that happens.
#
# The SHA256 values are placeholders. `dist` fills them in from the real archives.
class Reporch < Formula
  desc "One task, every repo, one console — orchestrate Claude Code across a multi-repo project"
  homepage "https://github.com/sezeryavuz/reporch"
  version "0.1.0"
  license "MIT"

  # No Windows bottle: cross-session messaging does not exist on native Windows (§4.5).
  # WSL 2 users install the Linux build inside their distribution.
  on_macos do
    on_arm do
      url "https://github.com/sezeryavuz/reporch/releases/download/v0.1.0/reporch-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/sezeryavuz/reporch/releases/download/v0.1.0/reporch-x86_64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/sezeryavuz/reporch/releases/download/v0.1.0/reporch-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
    on_intel do
      url "https://github.com/sezeryavuz/reporch/releases/download/v0.1.0/reporch-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  # reporch shells out to the real git binary so the user's config, hooks and credential
  # helpers apply (§13.2).
  depends_on "git"

  def install
    bin.install "reporch"
    # `rp` is the short alias, installed by every channel (§14).
    bin.install "rp" if File.exist?("rp")
    bin.install_symlink bin/"reporch" => "rp" unless File.exist?(bin/"rp")
  end

  def caveats
    <<~EOS
      reporch drives Claude Code, which it does not install. You need:

        Claude Code >= 2.1.224, on a first-party Anthropic account
        macOS or Linux — cross-session messaging does not exist on native Windows

      Check everything at once:

        reporch doctor
    EOS
  end

  test do
    assert_match "reporch", shell_output("#{bin}/reporch --version")
    assert_match "reporch", shell_output("#{bin}/rp --version")
    # doctor exits non-zero when Claude Code is absent, which it will be in a CI sandbox —
    # so assert on what it says rather than on the exit code.
    assert_match "Claude Code", shell_output("#{bin}/reporch doctor", 1)
  end
end
