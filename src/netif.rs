use std::net::IpAddr;

/// Rough classification of a local network interface, used to rank
/// candidates for the address suggested in generated Cisco commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IfKind {
    /// Physical-looking interface (Ethernet/Wi-Fi, `en*` on macOS).
    Physical,
    Loopback,
    /// VPN / tunnel (`utun*`, `tun*`, `tap*`, `ipsec*`, `ppp*`).
    Tunnel,
    /// Virtualisation or container bridges (`bridge*`, `vmnet*`, `docker*`, `vboxnet*`).
    Virtual,
    /// Apple Wireless Direct Link and friends (`awdl*`, `llw*`).
    AppleAux,
    Other,
}

impl IfKind {
    pub fn label(self) -> &'static str {
        match self {
            IfKind::Physical => "physical",
            IfKind::Loopback => "loopback",
            IfKind::Tunnel => "tunnel",
            IfKind::Virtual => "virtual",
            IfKind::AppleAux => "apple-aux",
            IfKind::Other => "other",
        }
    }
    /// Lower = preferred when suggesting an address.
    fn rank(self) -> u8 {
        match self {
            IfKind::Physical => 0,
            IfKind::Other => 1,
            IfKind::Virtual => 2,
            IfKind::Tunnel => 3,
            IfKind::AppleAux => 4,
            IfKind::Loopback => 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetInterface {
    pub name: String,
    pub ip: IpAddr,
    pub kind: IfKind,
}

pub fn classify(name: &str, ip: &IpAddr) -> IfKind {
    if ip.is_loopback() || name.starts_with("lo") {
        return IfKind::Loopback;
    }
    let n = name.to_ascii_lowercase();
    if n.starts_with("awdl") || n.starts_with("llw") {
        IfKind::AppleAux
    } else if n.starts_with("utun")
        || n.starts_with("tun")
        || n.starts_with("tap")
        || n.starts_with("ipsec")
        || n.starts_with("ppp")
        || n.starts_with("wg")
    {
        IfKind::Tunnel
    } else if n.starts_with("bridge")
        || n.starts_with("vmnet")
        || n.starts_with("docker")
        || n.starts_with("vboxnet")
        || n.starts_with("veth")
    {
        IfKind::Virtual
    } else if n.starts_with("en") || n.starts_with("eth") || n.starts_with("wl") {
        IfKind::Physical
    } else {
        IfKind::Other
    }
}

/// All local interfaces with their addresses (IPv4 and IPv6).
pub fn interfaces() -> Vec<NetInterface> {
    let mut out = Vec::new();
    if let Ok(list) = if_addrs::get_if_addrs() {
        for ifa in list {
            let ip = ifa.addr.ip();
            // Skip IPv6 link-local noise; keep everything else visible.
            if let IpAddr::V6(v6) = ip {
                if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                    continue;
                }
            }
            let kind = classify(&ifa.name, &ip);
            out.push(NetInterface { name: ifa.name, ip, kind });
        }
    }
    out.sort_by(|a, b| {
        a.kind
            .rank()
            .cmp(&b.kind.rank())
            .then_with(|| b.ip.is_ipv4().cmp(&a.ip.is_ipv4()))
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Best local IPv4 to put into generated Cisco copy commands: prefer
/// physical interfaces, never loopback/tunnel/VM unless nothing else exists.
pub fn suggest_ip() -> Option<IpAddr> {
    interfaces()
        .into_iter()
        .filter(|i| i.ip.is_ipv4() && i.kind != IfKind::Loopback)
        .min_by_key(|i| i.kind.rank())
        .map(|i| i.ip)
}

/// The address devices should use to reach a service bound to `bind`.
#[allow(dead_code)] // public helper, exercised in tests
pub fn effective_ip(bind: &str) -> Option<IpAddr> {
    match bind.parse::<IpAddr>() {
        Ok(ip) if ip.is_unspecified() => suggest_ip(),
        Ok(ip) => Some(ip),
        Err(_) => suggest_ip(),
    }
}

/// The local address the kernel would use to reach `peer`, asked by opening
/// an unconnected UDP socket — no packet is sent. This is what a device on
/// another subnet must be told to fetch from, and it is more reliable than
/// guessing from the interface list on a multi-homed machine.
pub fn source_ip_for(peer: &IpAddr) -> Option<IpAddr> {
    let bind = if peer.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = std::net::UdpSocket::bind(bind).ok()?;
    // Port 9 (discard); connect() on UDP only sets the default peer.
    sock.connect(std::net::SocketAddr::new(*peer, 9)).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_unspecified() {
        return None;
    }
    Some(ip)
}

/// Address a device at `peer` should use for a service bound to `bind`:
/// an explicit bind address wins, otherwise the route towards the device
/// decides, and only then the generic suggestion.
pub fn advertised_ip(bind: &str, peer: Option<&IpAddr>) -> Option<IpAddr> {
    if let Ok(ip) = bind.parse::<IpAddr>() {
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    peer.and_then(source_ip_for).or_else(suggest_ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn classification() {
        assert_eq!(classify("lo0", &v4("127.0.0.1")), IfKind::Loopback);
        assert_eq!(classify("en0", &v4("192.168.1.5")), IfKind::Physical);
        assert_eq!(classify("utun3", &v4("10.8.0.2")), IfKind::Tunnel);
        assert_eq!(classify("bridge100", &v4("192.168.64.1")), IfKind::Virtual);
        assert_eq!(classify("docker0", &v4("172.17.0.1")), IfKind::Virtual);
        assert_eq!(classify("awdl0", &v4("169.254.1.1")), IfKind::AppleAux);
    }

    #[test]
    fn suggestion_never_returns_loopback() {
        if let Some(ip) = suggest_ip() {
            assert!(!ip.is_loopback());
        }
    }

    #[test]
    fn effective_ip_uses_bind_when_concrete() {
        assert_eq!(effective_ip("192.168.1.10"), Some(v4("192.168.1.10")));
    }

    #[test]
    fn advertised_ip_prefers_an_explicit_bind() {
        let peer = v4("10.1.2.3");
        assert_eq!(advertised_ip("192.168.1.10", Some(&peer)), Some(v4("192.168.1.10")));
        // A wildcard bind falls through to the route towards the peer.
        assert_ne!(advertised_ip("0.0.0.0", Some(&peer)), Some(v4("0.0.0.0")));
    }

    #[test]
    fn source_ip_towards_loopback_is_loopback() {
        assert_eq!(source_ip_for(&v4("127.0.0.1")), Some(v4("127.0.0.1")));
    }
}
