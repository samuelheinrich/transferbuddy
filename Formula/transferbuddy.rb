# Homebrew formula for transferbuddy.
#
# For a published release, replace `url`/`sha256` with the release tarball:
#   url "https://github.com/<owner>/transferbuddy/archive/refs/tags/v0.2.tar.gz"
#   sha256 "<sha256 of the tarball>"
# and drop the `head`-style local install below.
class Transferbuddy < Formula
  desc "Multi-protocol file transfer server (FTP/HTTP/HTTPS/SCP/SFTP/TFTP) with a TUI, built for provisioning Cisco devices"
  homepage "https://github.com/samuel-heinrich/transferbuddy"
  url "https://github.com/samuel-heinrich/transferbuddy/archive/refs/tags/v0.2.tar.gz"
  sha256 :no_check # replace with the real tarball sha256 when tagging a release
  license "MIT"
  head "https://github.com/samuel-heinrich/transferbuddy.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      transferbuddy uses unprivileged default ports (8080/8443/2121/2222/6969).
      For the standard ports (80/443/21/22/69) run it with sudo:
        sudo transferbuddy --all
      Note: Cisco IOS `copy tftp:` only supports port 69 (requires sudo).

      Configuration and generated keys/certificates live in:
        ~/Library/Application Support/transferbuddy/
    EOS
  end

  test do
    # Version banner works.
    assert_match version.to_s, shell_output("#{bin}/transferbuddy --version")

    # Serve a file over HTTP and fetch it back.
    (testpath/"share/hello.txt").write "hello from transferbuddy"
    port = free_port
    pid = spawn bin/"transferbuddy", "--no-tui", "--http",
                "--bind", "127.0.0.1",
                "--port-http", port.to_s,
                "--root", testpath/"share",
                "--config", testpath/"cfg/config.toml"
    sleep 2
    begin
      output = shell_output("curl -s http://127.0.0.1:#{port}/hello.txt")
      assert_equal "hello from transferbuddy", output
    ensure
      Process.kill "INT", pid
      Process.wait pid
    end
  end
end
