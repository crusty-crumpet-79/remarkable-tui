class RemarkableTui < Formula
  desc "TUI for reMarkable 2 tablet interactions over USB"
  homepage "https://github.com/crusty-crumpet-79/remarkable-tui"
  url "https://github.com/crusty-crumpet-79/remarkable-tui/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "7ff5fce6afee4d5a7586e148dd8941d7d598b8dd8024e8e6b09e70cdabacf7e7"
  license "MIT" # Assuming MIT based on common Rust practices, but verify if added.

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  test do
    assert_predicate bin/"remarkable", :exist?
  end
end
