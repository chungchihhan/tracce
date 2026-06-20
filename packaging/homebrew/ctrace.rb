class Ctrace < Formula
  desc "macOS kernel-event tracer for Claude Code sessions"
  homepage "https://github.com/chungchihhan/ctrace"
  url "https://github.com/chungchihhan/ctrace/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "MIT"
  head "https://github.com/chungchihhan/ctrace.git", branch: "main"

  depends_on "rust" => :build
  depends_on :macos

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      ctrace reads kernel events via `sudo /usr/bin/eslogger`, so it prompts for
      your password on start. eslogger ships with macOS 13+. Your terminal app
      may also need Full Disk Access (System Settings > Privacy & Security).
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/ctrace --version")
  end
end
