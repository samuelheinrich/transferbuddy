use std::ffi::{c_char, c_void};
use std::{
    io::{self, Read, Write},
    net::{IpAddr, SocketAddr, TcpListener, UdpSocket},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    time::Duration,
};
use transferbuddy_core::platform::{broker_port_allowed, send_fd, BROKER_SOCKET};
type Ref = *const c_void;
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(value: Ref);
    fn CFStringCreateWithCString(allocator: Ref, value: *const c_char, encoding: u32) -> Ref;
    fn CFStringGetCString(value: Ref, buffer: *mut c_char, size: isize, encoding: u32) -> bool;
    fn CFDataCreate(allocator: Ref, bytes: *const u8, len: isize) -> Ref;
    fn CFDictionaryCreate(
        allocator: Ref,
        keys: *const Ref,
        values: *const Ref,
        count: isize,
        key_callbacks: Ref,
        value_callbacks: Ref,
    ) -> Ref;
    fn CFDictionaryGetValue(dictionary: Ref, key: Ref) -> Ref;
}
#[link(name = "Security", kind = "framework")]
extern "C" {
    static kSecGuestAttributeAudit: Ref;
    static kSecCodeInfoTeamIdentifier: Ref;
    fn SecCodeCopySelf(flags: u32, code: *mut Ref) -> i32;
    fn SecCodeCopyStaticCode(code: Ref, flags: u32, static_code: *mut Ref) -> i32;
    fn SecCodeCopySigningInformation(code: Ref, flags: u32, information: *mut Ref) -> i32;
    fn SecCodeCopyGuestWithAttributes(
        host: Ref,
        attributes: Ref,
        flags: u32,
        code: *mut Ref,
    ) -> i32;
    fn SecRequirementCreateWithString(text: Ref, flags: u32, requirement: *mut Ref) -> i32;
    fn SecCodeCheckValidity(code: Ref, flags: u32, requirement: Ref) -> i32;
}
struct Owned(Ref);
impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) }
        }
    }
}
fn failure(s: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, s)
}
fn checked(result: i32, value: Ref) -> io::Result<Owned> {
    if result != 0 || value.is_null() {
        Err(failure("Code-signing authentication failed"))
    } else {
        Ok(Owned(value))
    }
}
fn requirement() -> io::Result<Owned> {
    unsafe {
        let mut value = std::ptr::null();
        let own = checked(SecCodeCopySelf(0, &mut value), value)?;
        let mut value = std::ptr::null();
        let static_code = checked(SecCodeCopyStaticCode(own.0, 0, &mut value), value)?;
        let mut value = std::ptr::null();
        let info = checked(
            SecCodeCopySigningInformation(static_code.0, 1 << 1, &mut value),
            value,
        )?;
        let team = CFDictionaryGetValue(info.0, kSecCodeInfoTeamIdentifier);
        if team.is_null() {
            return Err(failure(
                "Helper must be Developer ID signed, with the same Team ID as the desktop",
            ));
        }
        let mut buffer = [0i8; 128];
        if !CFStringGetCString(team, buffer.as_mut_ptr(), 128, 0x08000100) {
            return Err(failure("Invalid signing Team ID"));
        }
        let team = std::ffi::CStr::from_ptr(buffer.as_ptr())
            .to_str()
            .map_err(|_| failure("Invalid signing Team ID"))?;
        if team.is_empty() || !team.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(failure("Invalid signing Team ID"));
        }
        let text=std::ffi::CString::new(format!("anchor apple generic and identifier \"com.transferbuddy.desktop\" and certificate leaf[subject.OU] = \"{team}\"")).unwrap();
        let text = Owned(CFStringCreateWithCString(
            std::ptr::null(),
            text.as_ptr(),
            0x08000100,
        ));
        let mut req = std::ptr::null();
        checked(SecRequirementCreateWithString(text.0, 0, &mut req), req)
    }
}
fn authenticate(stream: &UnixStream, requirement: &Owned) -> io::Result<()> {
    unsafe {
        // LOCAL_PEERTOKEN identifies the process that opened this connection, without PID reuse.
        let mut token = [0u32; 8];
        let mut length = std::mem::size_of_val(&token) as libc::socklen_t;
        if libc::getsockopt(
            stream.as_raw_fd(),
            0,
            0x006,
            token.as_mut_ptr().cast(),
            &mut length,
        ) != 0
            || length as usize != std::mem::size_of_val(&token)
        {
            return Err(failure("Cannot identify client audit token"));
        }
        let data = Owned(CFDataCreate(
            std::ptr::null(),
            token.as_ptr().cast(),
            std::mem::size_of_val(&token) as isize,
        ));
        let keys = [kSecGuestAttributeAudit];
        let values = [data.0];
        let attributes = Owned(CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
        ));
        let mut guest = std::ptr::null();
        let guest = checked(
            SecCodeCopyGuestWithAttributes(std::ptr::null(), attributes.0, 0, &mut guest),
            guest,
        )?;
        if SecCodeCheckValidity(guest.0, 0, requirement.0) != 0 {
            return Err(failure("Client is not the signed TransferBuddy desktop"));
        }
        Ok(())
    }
}
fn parse_request(line: &str) -> io::Result<(SocketAddr, bool)> {
    let mut words = line.split_whitespace();
    let udp = match words.next() {
        Some("tcp") => false,
        Some("udp") => true,
        _ => return Err(failure("Unknown listener type")),
    };
    let port = words
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| failure("Invalid port"))?;
    let ip = words
        .next()
        .and_then(|s| s.parse::<IpAddr>().ok())
        .ok_or_else(|| failure("Literal IP address required"))?;
    if words.next().is_some() || !broker_port_allowed(port, udp) {
        return Err(failure("Listener request outside helper scope"));
    }
    Ok((SocketAddr::new(ip, port), udp))
}
fn handle(mut stream: UnixStream, requirement: &Owned) -> io::Result<()> {
    authenticate(&stream, requirement)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut request = Vec::new();
    loop {
        let mut byte = [0];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if request.len() >= 128 {
            return Err(failure("Request too large"));
        }
        request.push(byte[0]);
    }
    let (addr, udp) =
        parse_request(std::str::from_utf8(&request).map_err(|_| failure("Invalid request"))?)?;
    if udp {
        send_fd(&stream, &UdpSocket::bind(addr)?)
    } else {
        send_fd(&stream, &TcpListener::bind(addr)?)
    }
}
pub fn run() -> io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(failure("Launch this helper through SMAppService"));
    }
    let requirement = requirement()?;
    if let Ok(meta) = std::fs::symlink_metadata(BROKER_SOCKET) {
        if meta.uid() != 0 || !meta.file_type().is_socket() {
            return Err(failure("Unsafe existing helper socket"));
        }
        if UnixStream::connect(BROKER_SOCKET).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                "Helper already running",
            ));
        }
        std::fs::remove_file(BROKER_SOCKET)?;
    }
    let listener = UnixListener::bind(BROKER_SOCKET)?;
    std::fs::set_permissions(BROKER_SOCKET, std::fs::Permissions::from_mode(0o666))?;
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let result = stream
                    .try_clone()
                    .and_then(|peer| handle(peer, &requirement));
                if let Err(e) = result {
                    eprintln!("Listener request denied: {e}");
                    let _ = stream.write_all(&[1]);
                }
            }
            Err(e) => eprintln!("Helper socket: {e}"),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_scope() {
        assert!(parse_request("tcp 443 127.0.0.1").is_ok());
        assert!(parse_request("udp 69 ::").is_ok());
        for r in [
            "tcp 69 127.0.0.1",
            "udp 443 ::",
            "tcp 80 hostname",
            "tcp 80 :: extra",
            "tcp 0 ::",
            "tcp 123 ::",
            "/bin/sh 80 ::",
        ] {
            assert!(parse_request(r).is_err(), "{r}")
        }
    }
    #[test]
    fn unsigned_client_is_rejected() {
        let (a, _) = UnixStream::pair().unwrap();
        let result = requirement().and_then(|req| authenticate(&a, &req));
        assert!(result.is_err());
    }
}
