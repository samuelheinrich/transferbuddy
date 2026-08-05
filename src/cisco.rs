use std::net::IpAddr;

use crate::config::Config;
use crate::services::ServiceId;
use crate::session::Protocol;

/// Cisco IOS/IOS-XE default ports for copy URLs; when the service runs on a
/// different port it must be part of the URL (TFTP being the exception).
fn cisco_default_port(proto: Protocol) -> u16 {
    match proto {
        Protocol::Ftp => 21,
        Protocol::Http => 80,
        Protocol::Https => 443,
        Protocol::Scp | Protocol::Sftp => 22,
        Protocol::Tftp => 69,
    }
}

fn fmt_ip(ip: &IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}

/// Build the `copy <url> flash:` command for one file and one protocol.
/// `rel_path` is the file path relative to the shared root, `/`-separated.
pub fn copy_command(proto: Protocol, cfg: &Config, ip: &IpAddr, rel_path: &str) -> String {
    let host = fmt_ip(ip);
    let user = &cfg.auth.username;
    let pass = &cfg.auth.password;
    let rel = rel_path.trim_start_matches('/');
    let (port, needs_port) = {
        let sc = cfg.service(match proto {
            Protocol::Ftp => ServiceId::Ftp,
            Protocol::Http => ServiceId::Http,
            Protocol::Https => ServiceId::Https,
            Protocol::Scp | Protocol::Sftp => ServiceId::Ssh,
            Protocol::Tftp => ServiceId::Tftp,
        });
        (sc.port, sc.port != cisco_default_port(proto))
    };
    let port_part = if needs_port { format!(":{port}") } else { String::new() };
    match proto {
        Protocol::Http => format!("copy http://{host}{port_part}/{rel} flash:"),
        Protocol::Https => format!("copy https://{host}{port_part}/{rel} flash:"),
        Protocol::Ftp => format!("copy ftp://{user}:{pass}@{host}{port_part}/{rel} flash:"),
        Protocol::Scp => format!("copy scp://{user}@{host}{port_part}/{rel} flash:"),
        Protocol::Sftp => format!("copy sftp://{user}@{host}{port_part}/{rel} flash:"),
        Protocol::Tftp => {
            // IOS `copy tftp://` does not accept a port; non-69 needs a hint.
            if needs_port {
                format!("copy tftp://{host}/{rel} flash:   ! note: TFTP runs on port {port}; IOS only supports port 69 — run with sudo for port 69")
            } else {
                format!("copy tftp://{host}/{rel} flash:")
            }
        }
    }
}

/// All copy commands for the currently enabled services.
pub fn commands_for_file(cfg: &Config, ip: &IpAddr, rel_path: &str) -> Vec<(Protocol, String)> {
    let mut out = Vec::new();
    if cfg.http.enabled {
        out.push((Protocol::Http, copy_command(Protocol::Http, cfg, ip, rel_path)));
    }
    if cfg.https.enabled {
        out.push((Protocol::Https, copy_command(Protocol::Https, cfg, ip, rel_path)));
    }
    if cfg.ftp.enabled {
        out.push((Protocol::Ftp, copy_command(Protocol::Ftp, cfg, ip, rel_path)));
    }
    if cfg.ssh.enabled {
        out.push((Protocol::Scp, copy_command(Protocol::Scp, cfg, ip, rel_path)));
        out.push((Protocol::Sftp, copy_command(Protocol::Sftp, cfg, ip, rel_path)));
    }
    if cfg.tftp.enabled {
        out.push((Protocol::Tftp, copy_command(Protocol::Tftp, cfg, ip, rel_path)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServiceId;

    fn cfg(privileged: bool) -> Config {
        let mut c = Config::default();
        for id in ServiceId::ALL {
            let sc = c.service_mut(id);
            sc.port = id.default_port(privileged);
            sc.enabled = true;
        }
        c.auth.username = "cisco".into();
        c.auth.password = "secret".into();
        c
    }

    fn ip() -> IpAddr {
        "192.168.1.10".parse().unwrap()
    }

    #[test]
    fn default_ports_are_omitted_when_privileged() {
        let c = cfg(true);
        assert_eq!(
            copy_command(Protocol::Http, &c, &ip(), "cat9k_iosxe.bin"),
            "copy http://192.168.1.10/cat9k_iosxe.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Tftp, &c, &ip(), "cat9k_iosxe.bin"),
            "copy tftp://192.168.1.10/cat9k_iosxe.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Ftp, &c, &ip(), "cat9k_iosxe.bin"),
            "copy ftp://cisco:secret@192.168.1.10/cat9k_iosxe.bin flash:"
        );
    }

    #[test]
    fn non_default_ports_appear_in_url() {
        let c = cfg(false);
        assert_eq!(
            copy_command(Protocol::Http, &c, &ip(), "cat9k_iosxe.bin"),
            "copy http://192.168.1.10:8080/cat9k_iosxe.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Https, &c, &ip(), "sub/img.bin"),
            "copy https://192.168.1.10:8443/sub/img.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Scp, &c, &ip(), "img.bin"),
            "copy scp://cisco@192.168.1.10:2222/img.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Sftp, &c, &ip(), "img.bin"),
            "copy sftp://cisco@192.168.1.10:2222/img.bin flash:"
        );
        // TFTP cannot carry a port in IOS; the command warns instead.
        assert!(copy_command(Protocol::Tftp, &c, &ip(), "img.bin").contains("port 6969"));
    }

    #[test]
    fn commands_only_for_enabled_services() {
        let mut c = cfg(false);
        c.https.enabled = false;
        c.ftp.enabled = false;
        c.ssh.enabled = false;
        c.tftp.enabled = false;
        let cmds = commands_for_file(&c, &ip(), "a.bin");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].0, Protocol::Http);
    }

    #[test]
    fn ipv6_addresses_are_bracketed() {
        let c = cfg(true);
        let v6: IpAddr = "fd00::1".parse().unwrap();
        assert_eq!(
            copy_command(Protocol::Http, &c, &v6, "a.bin"),
            "copy http://[fd00::1]/a.bin flash:"
        );
    }
}
