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

// Encode reserved URL characters in transfer credentials, including @ and :.
fn encode_userinfo(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The source URL a device uses to fetch `rel_path` from transferbuddy.
/// `needs_port` reports whether the port had to be spelled out.
fn source_url(proto: Protocol, cfg: &Config, ip: &IpAddr, rel_path: &str) -> (String, bool) {
    let sc = cfg.service(service_of(proto));
    // A service bound to one address can only be reached there, whatever the
    // caller suggests.
    let host = match sc.bind.parse::<IpAddr>() {
        Ok(bound) if !bound.is_unspecified() => fmt_ip(&bound),
        _ => fmt_ip(ip),
    };
    let user = encode_userinfo(&cfg.auth.username);
    let pass = encode_userinfo(&cfg.auth.password);
    let rel = rel_path.trim_start_matches('/');
    let needs_port = sc.port != cisco_default_port(proto);
    let port_part = if needs_port { format!(":{}", sc.port) } else { String::new() };
    let url = match proto {
        Protocol::Http => format!("http://{host}{port_part}/{rel}"),
        Protocol::Https => format!("https://{host}{port_part}/{rel}"),
        Protocol::Ftp => format!("ftp://{user}:{pass}@{host}{port_part}/{rel}"),
        Protocol::Scp => format!("scp://{user}:{pass}@{host}{port_part}/{rel}"),
        Protocol::Sftp => format!("sftp://{user}:{pass}@{host}{port_part}/{rel}"),
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

/// Free and total bytes of a device filesystem, from the footer `dir` prints:
/// `1956839424 bytes total (234979328 bytes free)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashUsage {
    pub total: u64,
    pub free: u64,
}

impl FlashUsage {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    /// 0.0 – 1.0; 0 when the device reported no size at all.
    pub fn used_fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.used() as f64 / self.total as f64
        }
    }
    /// Does an image of `size` bytes still fit, with a little headroom?
    pub fn fits(&self, size: u64) -> bool {
        self.free > size
    }
}

/// All plain decimal numbers in a line, in order.
fn numbers_in(line: &str) -> Vec<u64> {
    line.split(|c: char| !c.is_ascii_digit())
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect()
}

/// Pull the totals out of a `dir` listing. The footer is the last line that
/// mentions both totals, so trailing prompt lines do not matter.
pub fn parse_dir_totals(output: &str) -> Option<FlashUsage> {
    output
        .lines()
        .rev()
        .find(|l| l.contains("bytes total") && l.contains("bytes free"))
        .and_then(|line| match numbers_in(line)[..] {
            [total, free, ..] => Some(FlashUsage { total, free }),
            _ => None,
        })
}

/// One member of a stack, from the `Switch Ports Model SW Version` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackMember {
    pub number: u16,
    pub model: String,
    pub version: String,
    pub image: String,
    pub mode: String,
    /// The `*` in the table: the active switch.
    pub active: bool,
}

/// What `show version` tells us about a device.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VersionInfo {
    /// e.g. `17.15.03`
    pub version: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
    pub uptime: Option<String>,
    /// e.g. `flash:packages.conf`
    pub image: Option<String>,
    pub last_reload: Option<String>,
    /// Empty on a standalone switch.
    pub members: Vec<StackMember>,
}

impl VersionInfo {
    /// True when every stack member runs `version`. A stack that came up
    /// half-upgraded is exactly what this is here to catch.
    pub fn all_members_run(&self, version: &str) -> bool {
        !self.members.is_empty() && self.members.iter().all(|m| m.version == version)
    }
    /// Versions that differ from the reported system version.
    pub fn members_off_version(&self) -> Vec<&StackMember> {
        match &self.version {
            Some(v) => self.members.iter().filter(|m| &m.version != v).collect(),
            None => Vec::new(),
        }
    }
}

/// Value of a `Label : value` row, if the line is one.
fn labelled(line: &str, label: &str) -> Option<String> {
    let (head, value) = line.split_once(':')?;
    if head.trim().eq_ignore_ascii_case(label) {
        let v = value.trim();
        if v.is_empty() {
            return None;
        }
        return Some(v.to_string());
    }
    None
}

/// Parse `show version`. Every field is optional — IOS releases word their
/// output differently and a missing field must never lose the rest.
pub fn parse_show_version(output: &str) -> VersionInfo {
    let mut info = VersionInfo::default();
    let mut in_stack_table = false;

    for line in output.lines() {
        let t = line.trim();

        if info.version.is_none() {
            // "Cisco IOS XE Software, Version 17.15.03"
            if let Some(rest) = t.strip_prefix("Cisco IOS XE Software, Version ") {
                info.version = Some(rest.trim().trim_end_matches(',').to_string());
            }
        }
        if info.uptime.is_none() {
            if let Some((_, rest)) = t.split_once(" uptime is ") {
                info.uptime = Some(rest.trim().to_string());
            }
        }
        if info.image.is_none() {
            if let Some(rest) = t.strip_prefix("System image file is ") {
                info.image = Some(rest.trim().trim_matches('"').to_string());
            }
        }
        if info.last_reload.is_none() {
            if let Some(v) = labelled(t, "Last reload reason") {
                info.last_reload = Some(v);
            }
        }
        if info.model.is_none() {
            if let Some(v) = labelled(t, "Model Number") {
                info.model = Some(v);
            } else if let Some(rest) = t.strip_prefix("cisco ") {
                // "cisco C9200L-48P-4X (ARM64) processor with ..."
                if t.contains(" processor") {
                    if let Some(model) = rest.split_whitespace().next() {
                        info.model = Some(model.to_string());
                    }
                }
            }
        }
        if info.serial.is_none() {
            if let Some(v) = labelled(t, "System Serial Number") {
                info.serial = Some(v);
            }
        }

        // The stack table: header, a rule of dashes, then one row per member.
        if t.starts_with("Switch ") && t.contains("Ports") && t.contains("SW Version") {
            in_stack_table = true;
            continue;
        }
        if in_stack_table {
            if t.is_empty() {
                in_stack_table = false;
                continue;
            }
            if t.starts_with("---") {
                continue;
            }
            match parse_stack_row(t) {
                Some(member) => info.members.push(member),
                // A line that is not a member row ends the table.
                None => in_stack_table = false,
            }
        }
    }
    info
}

/// `*    2 52    C9200L-48P-4X      17.15.03          CAT9K_LITE_IOSXE      INSTALL`
fn parse_stack_row(line: &str) -> Option<StackMember> {
    let (active, rest) = match line.strip_prefix('*') {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let [number, _ports, model, version, image, mode] = fields[..] else {
        return None;
    };
    Some(StackMember {
        number: number.parse().ok()?,
        model: model.to_string(),
        version: version.to_string(),
        image: image.to_string(),
        mode: mode.to_string(),
        active,
    })
}

/// The device prompt without its trailing `#`/`>`, i.e. the hostname.
pub fn hostname_from_prompt(prompt: &str) -> Option<String> {
    let name = prompt.trim().trim_end_matches(['#', '>']).trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        None
    } else {
        Some(name.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServiceId;

    const SHOW_VERSION: &str = include_str!("../testdata/show_version_c9200l.txt");
    const DIR_FLASH: &str = include_str!("../testdata/dir_flash.txt");

    #[test]
    fn reads_flash_totals_from_dir() {
        let usage = parse_dir_totals(DIR_FLASH).expect("dir footer");
        assert_eq!(usage.total, 1956839424);
        assert_eq!(usage.free, 234979328);
        assert_eq!(usage.used(), 1721860096);
        assert!((usage.used_fraction() - 0.88).abs() < 0.01);
        // The image in the listing would not fit a second time.
        assert!(!usage.fits(504057659));
        assert!(usage.fits(100 * 1000 * 1000));
    }

    #[test]
    fn dir_without_a_footer_is_not_invented() {
        assert_eq!(parse_dir_totals("Directory of flash:/\nSG#"), None);
        assert_eq!(parse_dir_totals(""), None);
        // A truncated footer must not be read as totals.
        assert_eq!(parse_dir_totals("1956839424 bytes total"), None);
    }

    #[test]
    fn reads_the_c9200l_show_version() {
        let info = parse_show_version(SHOW_VERSION);
        assert_eq!(info.version.as_deref(), Some("17.15.03"));
        assert_eq!(info.model.as_deref(), Some("C9200L-48P-4X"));
        assert_eq!(info.serial.as_deref(), Some("FOC24252FHH"));
        assert_eq!(info.image.as_deref(), Some("flash:packages.conf"));
        assert_eq!(info.last_reload.as_deref(), Some("Image Install"));
        assert_eq!(
            info.uptime.as_deref(),
            Some("47 weeks, 6 days, 23 hours, 56 minutes")
        );
    }

    #[test]
    fn reads_every_stack_member() {
        let info = parse_show_version(SHOW_VERSION);
        assert_eq!(info.members.len(), 3, "members: {:?}", info.members);
        assert_eq!(info.members[0].number, 1);
        assert!(!info.members[0].active);
        assert!(info.members[1].active, "switch 2 is the active one");
        assert_eq!(info.members[2].model, "C9200L-48P-4X");
        for m in &info.members {
            assert_eq!(m.version, "17.15.03");
            assert_eq!(m.image, "CAT9K_LITE_IOSXE");
            assert_eq!(m.mode, "INSTALL");
        }
        // This is the switch whose upgrade did not take: everything still
        // runs the old release, and nothing is off-version relative to it.
        assert!(info.all_members_run("17.15.03"));
        assert!(!info.all_members_run("17.15.06"));
        assert!(info.members_off_version().is_empty());
    }

    #[test]
    fn detects_a_half_upgraded_stack() {
        let mut info = parse_show_version(SHOW_VERSION);
        info.members[2].version = "17.15.06".into();
        let odd = info.members_off_version();
        assert_eq!(odd.len(), 1);
        assert_eq!(odd[0].number, 3);
        assert!(!info.all_members_run("17.15.03"));
    }

    #[test]
    fn show_version_of_a_standalone_switch() {
        let text = "Cisco IOS XE Software, Version 17.09.04a\n\
                    sw1 uptime is 3 days, 2 hours\n\
                    System image file is \"flash:packages.conf\"\n\
                    Model Number                       : C9300-24P\n\
                    System Serial Number               : FOC1234ABCD\n";
        let info = parse_show_version(text);
        assert_eq!(info.version.as_deref(), Some("17.09.04a"));
        assert_eq!(info.model.as_deref(), Some("C9300-24P"));
        assert!(info.members.is_empty());
        assert!(!info.all_members_run("17.09.04a"), "no members, no claim");
    }

    #[test]
    fn empty_output_yields_nothing() {
        let info = parse_show_version("");
        assert_eq!(info, VersionInfo::default());
    }

    #[test]
    fn hostname_comes_from_the_prompt() {
        assert_eq!(hostname_from_prompt("SG-AS-OG5-01#"), Some("SG-AS-OG5-01".into()));
        assert_eq!(hostname_from_prompt("Switch>"), Some("Switch".into()));
        assert_eq!(hostname_from_prompt("#"), None);
        assert_eq!(hostname_from_prompt("a b#"), None);
    }

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
            "copy scp://cisco:secret@192.168.1.10:2222/img.bin flash:"
        );
        assert_eq!(
            copy_command(Protocol::Sftp, &c, &ip(), "img.bin"),
            "copy sftp://cisco:secret@192.168.1.10:2222/img.bin flash:"
        );
        // TFTP cannot carry a port in IOS; the command warns instead.
        assert!(copy_command(Protocol::Tftp, &c, &ip(), "img.bin").contains("port 6969"));
    }

    #[test]
    fn passwords_are_included_and_reserved_characters_are_encoded() {
        let mut c = cfg(false);
        c.auth.username = "net@admin".into();
        c.auth.password = "p:a@ss;word".into();
        for (proto, scheme) in [
            (Protocol::Ftp, "ftp"),
            (Protocol::Scp, "scp"),
            (Protocol::Sftp, "sftp"),
        ] {
            let command = deploy_command(proto, &c, &ip(), "img.bin", "flash:").unwrap();
            assert!(
                command.contains(&format!("{scheme}://net%40admin:p%3Aa%40ss%3Bword@")),
                "{command}"
            );
        }
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
    fn a_bound_service_advertises_its_own_address() {
        let mut c = cfg(false);
        c.http.bind = "10.9.9.9".into();
        // The suggestion is ignored: nothing listens on it for this service.
        assert_eq!(
            copy_command(Protocol::Http, &c, &ip(), "img.bin"),
            "copy http://10.9.9.9:8080/img.bin flash:"
        );
        // A wildcard bind keeps using the advertised address.
        c.https.bind = "0.0.0.0".into();
        assert_eq!(
            copy_command(Protocol::Https, &c, &ip(), "img.bin"),
            "copy https://192.168.1.10:8443/img.bin flash:"
        );
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
