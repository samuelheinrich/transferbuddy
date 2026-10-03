//! Listener creation is the only privileged operation. Bind directly first.
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, OnceLock},
};
pub fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}
#[cfg(unix)]
pub trait PortBroker: Send + Sync {
    fn bind(&self, address: SocketAddr, udp: bool) -> io::Result<OwnedFd>;
}
#[cfg(unix)]
static BROKER: OnceLock<Arc<dyn PortBroker>> = OnceLock::new();
#[cfg(unix)]
pub fn install_broker(broker: Arc<dyn PortBroker>) {
    let _ = BROKER.set(broker);
}
pub async fn bind_tcp(address: SocketAddr) -> io::Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(address).await {
        Ok(l) => Ok(l),
        Err(error) => {
            #[cfg(unix)]
            if error.kind() == io::ErrorKind::PermissionDenied {
                if let Some(broker) = BROKER.get() {
                    let broker = broker.clone();
                    let fd = tokio::task::spawn_blocking(move || broker.bind(address, false))
                        .await
                        .map_err(io::Error::other)??;
                    let listener = std::net::TcpListener::from(fd);
                    listener.set_nonblocking(true)?;
                    return tokio::net::TcpListener::from_std(listener);
                }
            }
            Err(error)
        }
    }
}
pub async fn bind_udp(address: SocketAddr) -> io::Result<tokio::net::UdpSocket> {
    match tokio::net::UdpSocket::bind(address).await {
        Ok(l) => Ok(l),
        Err(error) => {
            #[cfg(unix)]
            if error.kind() == io::ErrorKind::PermissionDenied {
                if let Some(broker) = BROKER.get() {
                    let broker = broker.clone();
                    let fd = tokio::task::spawn_blocking(move || broker.bind(address, true))
                        .await
                        .map_err(io::Error::other)??;
                    let socket = std::net::UdpSocket::from(fd);
                    socket.set_nonblocking(true)?;
                    return tokio::net::UdpSocket::from_std(socket);
                }
            }
            Err(error)
        }
    }
}
pub fn broker_port_allowed(port: u16, udp: bool) -> bool {
    if udp {
        port == 69
    } else {
        matches!(port, 21 | 22 | 80 | 443)
    }
}
#[cfg(target_os = "macos")]
pub const BROKER_SOCKET: &str = "/var/run/transferbuddy-port-helper.sock";
#[cfg(target_os = "macos")]
pub struct MacBroker;
#[cfg(target_os = "macos")]
impl PortBroker for MacBroker {
    fn bind(&self, address: SocketAddr, udp: bool) -> io::Result<OwnedFd> {
        use std::{io::Write, os::unix::net::UnixStream, time::Duration};
        if !broker_port_allowed(address.port(), udp) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Helper supports only TCP 21/22/80/443 and UDP 69",
            ));
        }
        let mut stream=UnixStream::connect(BROKER_SOCKET).map_err(|e|io::Error::new(e.kind(),format!("Standard-port helper unavailable: {e}. Enable the helper in Desktop Settings or use a higher port.")))?;
        let mut uid = 0;
        let mut gid = 0;
        if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 || uid != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Helper peer is not root",
            ));
        }
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(
            format!(
                "{} {} {}\n",
                if udp { "udp" } else { "tcp" },
                address.port(),
                address.ip()
            )
            .as_bytes(),
        )?;
        receive_fd(&stream)
    }
}
#[cfg(target_os = "macos")]
pub fn receive_fd(stream: &std::os::unix::net::UnixStream) -> io::Result<OwnedFd> {
    unsafe {
        let mut byte = [0u8];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 8];
        let mut message: libc::msghdr = std::mem::zeroed();
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control) as _;
        let n = libc::recvmsg(stream.as_raw_fd(), &mut message, 0);
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut received = Vec::new();
        let mut cmsg = libc::CMSG_FIRSTHDR(&message);
        while !cmsg.is_null() {
            let c = &*cmsg;
            if c.cmsg_level == libc::SOL_SOCKET && c.cmsg_type == libc::SCM_RIGHTS {
                let bytes = (c.cmsg_len as usize).saturating_sub(libc::CMSG_LEN(0) as usize);
                for i in 0..bytes / std::mem::size_of::<i32>() {
                    received.push(OwnedFd::from_raw_fd(
                        *(libc::CMSG_DATA(cmsg).cast::<i32>().add(i)),
                    ))
                }
            }
            cmsg = libc::CMSG_NXTHDR(&message, cmsg);
        }
        if n != 1
            || byte[0] != 0
            || received.len() != 1
            || message.msg_flags & libc::MSG_CTRUNC != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Helper rejected listener request; signed app and approved helper are required",
            ));
        }
        let fd = received.pop().unwrap();
        libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
        Ok(fd)
    }
}
#[cfg(target_os = "macos")]
pub fn send_fd(stream: &std::os::unix::net::UnixStream, fd: &impl AsRawFd) -> io::Result<()> {
    unsafe {
        let mut byte = [0u8];
        let mut iov = libc::iovec {
            iov_base: byte.as_mut_ptr().cast(),
            iov_len: 1,
        };
        let mut control = [0usize; 8];
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<i32>() as _) as _;
        let c = libc::CMSG_FIRSTHDR(&msg);
        (*c).cmsg_level = libc::SOL_SOCKET;
        (*c).cmsg_type = libc::SCM_RIGHTS;
        (*c).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<i32>() as _) as _;
        *libc::CMSG_DATA(c).cast::<i32>() = fd.as_raw_fd();
        if libc::sendmsg(stream.as_raw_fd(), &msg, 0) != 1 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_port_scope() {
        for port in [21, 22, 80, 443] {
            assert!(broker_port_allowed(port, false));
            assert!(!broker_port_allowed(port, true));
        }
        assert!(broker_port_allowed(69, true));
        assert!(!broker_port_allowed(69, false));
        assert!(!broker_port_allowed(1023, false));
    }
    #[tokio::test]
    async fn direct_bind_works() {
        let tcp = bind_tcp("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let udp = bind_udp("127.0.0.1:0".parse().unwrap()).await.unwrap();
        assert!(tcp.local_addr().unwrap().port() > 0);
        assert!(udp.local_addr().unwrap().port() > 0);
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn descriptor_round_trip() {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        send_fd(&a, &l).unwrap();
        let fd = receive_fd(&b).unwrap();
        let copy = std::net::TcpListener::from(fd);
        assert_eq!(copy.local_addr().unwrap(), l.local_addr().unwrap());
    }
}
