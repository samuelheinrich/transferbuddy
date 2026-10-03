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

#[derive(Debug, Clone, PartialEq, Eq)]
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
            out.push(NetInterface {
                name: ifa.name,
                ip,
                kind,
            });
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

/// Interfaces a device could realistically be told to connect to: IPv4, not
/// loopback. This is the list the address selector cycles through.
pub fn candidates() -> Vec<NetInterface> {
    interfaces()
        .into_iter()
        .filter(|i| i.ip.is_ipv4() && i.kind != IfKind::Loopback)
        .collect()
}

/// Best local IPv4 to put into generated Cisco copy commands: prefer
/// physical interfaces, never loopback/tunnel/VM unless nothing else exists.
///
/// On a laptop on Wi-Fi *and* cable both are `Physical`, so the tie is broken
/// by the interface order — which is why the address can be pinned, see
/// [`resolve_advertise`].
pub fn suggest_ip() -> Option<IpAddr> {
    candidates()
        .into_iter()
        .min_by_key(|i| i.kind.rank())
        .map(|i| i.ip)
}

/// Resolve a configured preference — an interface name (`en5`) or a literal
/// address — against the interfaces that exist right now. `None` when the
/// interface is gone or the address is no longer configured, so a stale
/// selection must be corrected explicitly rather than silently changing routes.
pub fn resolve_advertise(setting: &str) -> Option<IpAddr> {
    let setting = setting.trim();
    if setting.is_empty() {
        return None;
    }
    let all = interfaces();
    if let Ok(ip) = setting.parse::<IpAddr>() {
        return all.iter().find(|i| i.ip == ip).map(|i| i.ip);
    }
    // Prefer IPv4 of that interface; fall back to whatever it has.
    all.iter()
        .find(|i| i.name == setting && i.ip.is_ipv4())
        .or_else(|| all.iter().find(|i| i.name == setting))
        .map(|i| i.ip)
}

/// The interface an address belongs to, for display.
pub fn interface_of(ip: &IpAddr) -> Option<String> {
    interfaces()
        .into_iter()
        .find(|i| &i.ip == ip)
        .map(|i| i.name)
}

/// The local address the kernel would use to reach `peer`, asked by opening
/// an unconnected UDP socket — no packet is sent. This is what a device on
/// another subnet must be told to fetch from, and it is more reliable than
/// guessing from the interface list on a multi-homed machine.
pub fn source_ip_for(peer: &IpAddr) -> Option<IpAddr> {
    let bind = if peer.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    };
    let sock = std::net::UdpSocket::bind(bind).ok()?;
    // Port 9 (discard); connect() on UDP only sets the default peer.
    sock.connect(std::net::SocketAddr::new(*peer, 9)).ok()?;
    let ip = sock.local_addr().ok()?.ip();
    if ip.is_unspecified() {
        return None;
    }
    Some(ip)
}

/// Address a device at `peer` should use for a service bound to `bind`.
///
/// A service bound to one address can only be reached there, so that wins.
/// After it comes the address the user pinned, then the route towards the
/// device, and only then the generic suggestion.
pub fn advertised_ip(advertise: Option<&str>, bind: &str, peer: Option<&IpAddr>) -> Option<IpAddr> {
    if let Ok(ip) = bind.parse::<IpAddr>() {
        if !ip.is_unspecified() {
            return Some(ip);
        }
    }
    match advertise {
        Some(selection) => resolve_advertise(selection),
        None => match peer {
            Some(peer) => source_ip_for(peer),
            None => suggest_ip(),
        },
    }
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
    fn advertised_ip_prefers_an_explicit_bind() {
        let peer = v4("10.1.2.3");
        assert_eq!(
            advertised_ip(None, "192.168.1.10", Some(&peer)),
            Some(v4("192.168.1.10"))
        );
        // A bound address wins even over a pinned one — only there is anyone
        // actually listening.
        assert_eq!(
            advertised_ip(Some("en0"), "192.168.1.10", Some(&peer)),
            Some(v4("192.168.1.10"))
        );
        // A wildcard bind falls through.
        assert_ne!(
            advertised_ip(None, "0.0.0.0", Some(&peer)),
            Some(v4("0.0.0.0"))
        );
    }

    #[test]
    fn a_pinned_interface_wins_over_the_automatic_choice() {
        let Some(pick) = candidates().into_iter().next() else {
            return; // no network on this machine
        };
        let peer = v4("10.1.2.3");
        assert_eq!(
            advertised_ip(Some(&pick.name), "0.0.0.0", Some(&peer)),
            Some(pick.ip)
        );
        // Pinning the literal address works the same way.
        assert_eq!(
            advertised_ip(Some(&pick.ip.to_string()), "0.0.0.0", Some(&peer)),
            Some(pick.ip)
        );
        assert_eq!(interface_of(&pick.ip).as_deref(), Some(pick.name.as_str()));
    }

    #[test]
    fn a_stale_pin_requires_an_explicit_new_selection() {
        assert_eq!(resolve_advertise("en-does-not-exist"), None);
        assert_eq!(resolve_advertise("203.0.113.99"), None);
        assert_eq!(resolve_advertise(""), None);
        // Never silently switch to a different laptop interface.
        assert_eq!(
            advertised_ip(Some("en-does-not-exist"), "0.0.0.0", None),
            None
        );
    }

    #[test]
    fn candidates_are_never_loopback() {
        for c in candidates() {
            assert!(c.ip.is_ipv4() && !c.ip.is_loopback(), "{c:?}");
        }
    }

    #[test]
    fn source_ip_towards_loopback_is_loopback() {
        assert_eq!(source_ip_for(&v4("127.0.0.1")), Some(v4("127.0.0.1")));
    }
}
