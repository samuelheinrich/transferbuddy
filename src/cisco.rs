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

/// The service that carries one protocol.
pub fn service_of(proto: Protocol) -> ServiceId {
    match proto {
        Protocol::Ftp => ServiceId::Ftp,
        Protocol::Http => ServiceId::Http,
        Protocol::Https => ServiceId::Https,
        Protocol::Scp | Protocol::Sftp => ServiceId::Ssh,
        Protocol::Tftp => ServiceId::Tftp,
    }
}

/// The source URL a device uses to fetch `rel_path` from transferbuddy.
/// `needs_port` reports whether the port had to be spelled out.
fn source_url(proto: Protocol, cfg: &Config, ip: &IpAddr, rel_path: &str) -> (String, bool) {
    let host = fmt_ip(ip);
    let user = &cfg.auth.username;
    let pass = &cfg.auth.password;
    let rel = rel_path.trim_start_matches('/');
    let sc = cfg.service(service_of(proto));
    let needs_port = sc.port != cisco_default_port(proto);
    let port_part = if needs_port { format!(":{}", sc.port) } else { String::new() };
    let url = match proto {
        Protocol::Http => format!("http://{host}{port_part}/{rel}"),
        Protocol::Https => format!("https://{host}{port_part}/{rel}"),
        Protocol::Ftp => format!("ftp://{user}:{pass}@{host}{port_part}/{rel}"),
        Protocol::Scp => format!("scp://{user}@{host}{port_part}/{rel}"),
        Protocol::Sftp => format!("sftp://{user}@{host}{port_part}/{rel}"),
        // IOS `copy tftp://` does not accept a port at all.
        Protocol::Tftp => format!("tftp://{host}/{rel}"),
    };
    (url, needs_port)
}

/// Build the `copy <url> flash:` command for one file and one protocol.
/// `rel_path` is the file path relative to the shared root, `/`-separated.
pub fn copy_command(proto: Protocol, cfg: &Config, ip: &IpAddr, rel_path: &str) -> String {
    let (url, needs_port) = source_url(proto, cfg, ip, rel_path);
    if proto == Protocol::Tftp && needs_port {
        // IOS only ever talks to port 69, so the command alone is not enough.
        let port = cfg.service(ServiceId::Tftp).port;
        return format!(
            "copy {url} flash:   ! note: TFTP runs on port {port}; IOS only supports port 69 — run with sudo for port 69"
        );
    }
    format!("copy {url} flash:")
}

/// The command a deploy types on the device: no explanatory suffix, and an
/// explicit destination. `Err` when the device could not reach the service.
pub fn deploy_command(
    proto: Protocol,
    cfg: &Config,
    ip: &IpAddr,
    rel_path: &str,
    dest: &str,
) -> Result<String, String> {
    let dest = dest.trim();
    if dest.is_empty() {
        return Err("destination is empty — use e.g. flash:".into());
    }
    let (url, needs_port) = source_url(proto, cfg, ip, rel_path);
    if proto == Protocol::Tftp && needs_port {
        return Err(format!(
            "IOS only supports TFTP on port 69, transferbuddy listens on {} — \
             start transferbuddy with sudo or deploy over HTTP",
            cfg.service(ServiceId::Tftp).port
        ));
    }
    let cmd = format!("copy {url} {dest}");
    crate::deploy::check_command(&cmd)?;
    Ok(cmd)
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
    fn deploy_command_is_a_bare_copy() {
        let c = cfg(false);
        assert_eq!(
            deploy_command(Protocol::Http, &c, &ip(), "img.bin", "flash:").unwrap(),
            "copy http://192.168.1.10:8080/img.bin flash:"
        );
        assert_eq!(
            deploy_command(Protocol::Http, &c, &ip(), "sub/img.bin", "bootflash:new.bin").unwrap(),
            "copy http://192.168.1.10:8080/sub/img.bin bootflash:new.bin"
        );
        // No trailing "! note:" comment — this is typed on the device.
        assert!(!deploy_command(Protocol::Http, &c, &ip(), "img.bin", "flash:")
            .unwrap()
            .contains('!'));
    }

    #[test]
    fn deploy_command_rejects_what_the_device_cannot_do() {
        let c = cfg(false);
        // TFTP on a high port is unreachable for IOS.
        assert!(deploy_command(Protocol::Tftp, &c, &ip(), "img.bin", "flash:").is_err());
        assert!(deploy_command(Protocol::Tftp, &cfg(true), &ip(), "img.bin", "flash:").is_ok());
        // And the whitelist still applies to the destination.
        assert!(deploy_command(Protocol::Http, &c, &ip(), "img.bin", "running-config").is_err());
        assert!(deploy_command(Protocol::Http, &c, &ip(), "img.bin", "").is_err());
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
