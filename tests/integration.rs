//! Integration tests: spawn the real binary (`--no-tui`) and talk to it over
//! real sockets, the way curl / Cisco devices would.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_transferbuddy")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

struct Server {
    child: Child,
    _dir: tempfile::TempDir,
    http_port: u16,
    tftp_port: u16,
}

impl Server {
    fn start(uploads: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("image.bin"), vec![0xAB; 300_000]).unwrap();
        std::fs::write(root.join("sub/nested.cfg"), b"hostname sw1\n").unwrap();
        std::fs::create_dir_all(root.join("up")).unwrap();

        let http_port = free_port();
        let tftp_port = free_port();
        let mut cmd = Command::new(bin());
        cmd.args([
            "--no-tui",
            "--http",
            "--tftp",
            "--bind",
            "127.0.0.1",
            "--root",
            root.to_str().unwrap(),
            "--port-http",
            &http_port.to_string(),
            "--port-tftp",
            &tftp_port.to_string(),
            "--config",
            dir.path().join("cfg/config.toml").to_str().unwrap(),
            "--upload-dir",
            "up",
            "--max-upload-mib",
            "1",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        if uploads {
            cmd.arg("--uploads");
        }
        let child = cmd.spawn().expect("spawning transferbuddy");
        let server = Server { child, _dir: dir, http_port, tftp_port };
        server.wait_ready();
        server
    }

    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if TcpStream::connect(("127.0.0.1", self.http_port)).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("server did not become ready");
    }

    fn http(&self, request: &str) -> (u16, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", self.http_port)).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        // Read the declared HTTP response, rather than waiting for socket EOF.
        // macOS can reset a rejected upload with an unread request body after
        // sending the complete response; its Content-Length still frames it.
        while !response.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            response.push(byte[0]);
            assert!(response.len() < 16 * 1024, "HTTP headers too large");
        }
        let body_start = response.len();
        let content_length = String::from_utf8_lossy(&response).lines()
            .find_map(|line| line.strip_prefix("Content-Length: "))
            .unwrap().parse::<usize>().unwrap();
        response.resize(body_start + content_length, 0);
        stream.read_exact(&mut response[body_start..]).unwrap();
        let head = String::from_utf8_lossy(&response);
        let code: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body_start = response
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|p| p + 4)
            .unwrap_or(response.len());
        (code, response[body_start..].to_vec())
    }

    fn get(&self, path: &str) -> (u16, Vec<u8>) {
        self.http(&format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn http_download_and_errors() {
    let s = Server::start(false);
    let (code, body) = s.get("/image.bin");
    assert_eq!(code, 200);
    assert_eq!(body.len(), 300_000);
    assert!(body.iter().all(|b| *b == 0xAB));

    let (code, body) = s.get("/sub/nested.cfg");
    assert_eq!(code, 200);
    assert_eq!(body, b"hostname sw1\n");

    let (code, _) = s.get("/missing.bin");
    assert_eq!(code, 404);

    // Directory listing works.
    let (code, body) = s.get("/");
    assert_eq!(code, 200);
    assert!(String::from_utf8_lossy(&body).contains("image.bin"));
}

#[test]
fn http_blocks_path_traversal() {
    let s = Server::start(false);
    for evil in ["/../etc/passwd", "/../../etc/passwd", "/sub/../../etc/passwd", "/%2e%2e/etc/passwd"] {
        let (code, _) = s.get(evil);
        assert!(code == 403 || code == 404, "{evil} returned {code}");
    }
}

#[test]
fn http_upload_disabled_by_default() {
    let s = Server::start(false);
    let (code, _) = s.http(
        "PUT /up.bin HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
    );
    assert_eq!(code, 403);
}

#[test]
fn http_upload_when_enabled() {
    let s = Server::start(true);
    let (code, _) = s.http(
        "PUT /up.bin HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
    );
    assert_eq!(code, 201);
    // No overwrite of an existing upload.
    let (code, _) = s.http(
        "PUT /up.bin HTTP/1.1\r\nHost: x\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
    );
    assert_eq!(code, 409);
    // Upload size limit (1 MiB) enforced.
    let (code, _) = s.http(&format!(
        "PUT /big.bin HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        2 * 1024 * 1024
    ));
    assert_eq!(code, 409);
}

#[test]
fn parallel_http_downloads() {
    let s = Server::start(false);
    let port = s.http_port;
    let mut threads = Vec::new();
    for _ in 0..8 {
        threads.push(std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /image.bin HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
                .unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).unwrap();
            response.len()
        }));
    }
    for t in threads {
        let len = t.join().unwrap();
        assert!(len > 300_000, "short response: {len}");
    }
}

#[test]
fn tftp_download_rrq() {
    let s = Server::start(false);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    // RRQ image.bin octet
    let mut rrq = vec![0, 1];
    rrq.extend_from_slice(b"sub/nested.cfg\0octet\0");
    socket.send_to(&rrq, ("127.0.0.1", s.tftp_port)).unwrap();

    let mut buf = [0u8; 1024];
    let mut collected = Vec::new();
    let mut expected_block = 1u16;
    loop {
        let (n, from) = socket.recv_from(&mut buf).unwrap();
        assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 3, "expected DATA");
        let block = u16::from_be_bytes([buf[2], buf[3]]);
        assert_eq!(block, expected_block);
        collected.extend_from_slice(&buf[4..n]);
        let ack = [0u8, 4, buf[2], buf[3]];
        socket.send_to(&ack, from).unwrap();
        if n - 4 < 512 {
            break;
        }
        expected_block += 1;
    }
    assert_eq!(collected, b"hostname sw1\n");
}

#[test]
fn tftp_blocks_traversal() {
    let s = Server::start(false);
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut rrq = vec![0, 1];
    rrq.extend_from_slice(b"../../etc/passwd\0octet\0");
    socket.send_to(&rrq, ("127.0.0.1", s.tftp_port)).unwrap();
    let mut buf = [0u8; 1024];
    let (n, _) = socket.recv_from(&mut buf).unwrap();
    assert!(n >= 4);
    assert_eq!(u16::from_be_bytes([buf[0], buf[1]]), 5, "expected ERROR packet");
}

#[test]
fn occupied_port_fails_with_clean_exit() {
    let blocker = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = blocker.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(bin())
        .args([
            "--no-tui",
            "--http",
            "--bind",
            "127.0.0.1",
            "--root",
            dir.path().to_str().unwrap(),
            "--port-http",
            &port.to_string(),
            "--config",
            dir.path().join("cfg/config.toml").to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn privileged_port_rejected_without_root() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(bin())
        .args([
            "--no-tui",
            "--http",
            "--port-http",
            "80",
            "--root",
            dir.path().to_str().unwrap(),
            "--config",
            dir.path().join("cfg/config.toml").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("sudo"), "stderr was: {err}");
}

#[test]
fn graceful_shutdown_on_sigint() {
    let s = Server::start(false);
    let pid = s.child.id() as i32;
    unsafe { libc::kill(pid, libc::SIGINT) };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut server = s;
    loop {
        if let Ok(Some(status)) = server.child.try_wait() {
            assert!(status.success(), "exit status: {status:?}");
            break;
        }
        assert!(Instant::now() < deadline, "no clean shutdown within 5 s");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Port is released again.
    std::thread::sleep(Duration::from_millis(100));
    assert!(TcpListener::bind(("127.0.0.1", server.http_port)).is_ok());
}

#[test]
fn client_abort_does_not_kill_server() {
    let s = Server::start(false);
    // Open a download and drop the socket mid-transfer.
    {
        let mut stream = TcpStream::connect(("127.0.0.1", s.http_port)).unwrap();
        stream
            .write_all(b"GET /image.bin HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut small = [0u8; 512];
        let _ = stream.read(&mut small);
        drop(stream);
    }
    std::thread::sleep(Duration::from_millis(200));
    // Server still answers.
    let (code, _) = s.get("/sub/nested.cfg");
    assert_eq!(code, 200);
}
