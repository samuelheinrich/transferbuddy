use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::cli::Cli;
use crate::logging::LogLevel;
use crate::services::ServiceId;

/// Persistent + runtime configuration. Secrets (passwords) are never stored
/// here — see `auth::Secrets`, which lives in a 0600 file under `state/`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Shared root directory. Everything below it is served, nothing above it.
    /// Never persisted: transferbuddy always serves the directory it was
    /// started in, unless `--root` says otherwise.
    #[serde(skip)]
    pub root: PathBuf,

    pub http: ServiceConfig,
    pub https: ServiceConfig,
    pub ftp: ServiceConfig,
    /// One SSH service covers both SFTP and SCP.
    pub ssh: ServiceConfig,
    pub tftp: ServiceConfig,

    pub auth: AuthConfig,
    pub uploads: UploadConfig,
    pub tls: TlsConfig,

    /// Which local address generated URLs use when a service is bound to
    /// `0.0.0.0`: an interface name (`en5`), a literal address, or `None` for
    /// the automatic choice. A laptop on cable *and* Wi-Fi is the reason this
    /// exists.
    pub advertise: Option<String>,

    pub log_level: LogLevel,
    pub log_to_file: bool,
    pub log_file: Option<PathBuf>,

    /// How long finished/failed sessions stay visible in the session view.
    pub session_history_secs: u64,
    /// Maximum parallel sessions per service.
    pub max_sessions: usize,
    /// Idle session timeout in seconds.
    pub idle_timeout_secs: u64,
    /// Show speeds in bits (true) or bytes (false).
    pub speed_in_bits: bool,
    /// Play short beeps when settings are toggled in the TUI.
    pub sound: bool,
    /// Show the animated intro screen on start.
    pub intro: bool,

    #[serde(skip)]
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ServiceConfig {
    pub enabled: bool,
    /// 0 = pick the default for the current privilege level at startup.
    pub port: u16,
    pub bind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AuthConfig {
    pub username: String,
    /// Always `cisco123` unless a password was set explicitly (TUI, CLI or the
    /// secrets file). Kept out of config.toml — it lives in state/secrets.toml.
    #[serde(skip)]
    pub password: String,
    /// Lock an IP out after this many failed logins ...
    pub max_login_failures: u32,
    /// ... for this many seconds.
    pub login_lockout_secs: u64,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            username: crate::auth::DEFAULT_USERNAME.into(),
            password: crate::auth::DEFAULT_PASSWORD.into(),
            max_login_failures: 5,
            login_lockout_secs: 60,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UploadConfig {
    pub enabled: bool,
    /// Upload target directory, relative to root ("" = root itself).
    pub dir: String,
    /// Never overwrite existing files unless explicitly allowed.
    pub overwrite: bool,
    /// 0 = unlimited.
    pub max_upload_mib: u64,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self { enabled: false, dir: String::new(), overwrite: false, max_upload_mib: 0 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TlsConfig {
    /// Existing certificate/key to use instead of the generated ones.
    pub cert_path: Option<PathBuf>,
    pub key_path: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            root: PathBuf::from("."),
            http: ServiceConfig::default(),
            https: ServiceConfig::default(),
            ftp: ServiceConfig::default(),
            ssh: ServiceConfig::default(),
            tftp: ServiceConfig::default(),
            auth: AuthConfig::default(),
            uploads: UploadConfig::default(),
            tls: TlsConfig::default(),
            advertise: None,
            log_level: LogLevel::Info,
            log_to_file: true,
            log_file: None,
            session_history_secs: 600,
            max_sessions: 32,
            idle_timeout_secs: 300,
            speed_in_bits: true,
            sound: true,
            intro: true,
            config_dir: PathBuf::new(),
        }
    }
}

impl Config {
    /// `~/Library/Application Support/transferbuddy` on macOS.
    pub fn default_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("transferbuddy")
    }

    pub fn load(explicit_path: Option<&Path>) -> Result<Self> {
        let (dir, file) = match explicit_path {
            Some(p) => (
                p.parent().map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from(".")),
                p.to_path_buf(),
            ),
            None => {
                let dir = Self::default_dir();
                let file = dir.join("config.toml");
                (dir, file)
            }
        };
        let mut cfg: Config = if file.is_file() {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))?
        } else {
            Config::default()
        };
        cfg.config_dir = dir;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_dir)
            .with_context(|| format!("creating {}", self.config_dir.display()))?;
        let text = toml::to_string_pretty(self)?;
        let path = self.config_dir.join("config.toml");
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn service(&self, id: ServiceId) -> &ServiceConfig {
        match id {
            ServiceId::Ftp => &self.ftp,
            ServiceId::Http => &self.http,
            ServiceId::Https => &self.https,
            ServiceId::Ssh => &self.ssh,
            ServiceId::Tftp => &self.tftp,
        }
    }

    pub fn service_mut(&mut self, id: ServiceId) -> &mut ServiceConfig {
        match id {
            ServiceId::Ftp => &mut self.ftp,
            ServiceId::Http => &mut self.http,
            ServiceId::Https => &mut self.https,
            ServiceId::Ssh => &mut self.ssh,
            ServiceId::Tftp => &mut self.tftp,
        }
    }

    /// Merge CLI flags over the loaded config and resolve "auto" values.
    pub fn apply_cli(&mut self, cli: &Cli, privileged: bool) -> Result<()> {
        // Root directory: CLI > cwd. The root is never read from config.toml,
        // so starting transferbuddy somewhere else always serves that place.
        if let Some(root) = &cli.root {
            self.root = root.clone();
        } else if self.root.as_os_str().is_empty() || self.root == Path::new(".") {
            self.root = std::env::current_dir().context("determining current directory")?;
        }
        self.root = self
            .root
            .canonicalize()
            .with_context(|| format!("root directory {} does not exist", self.root.display()))?;
        if !self.root.is_dir() {
            bail!("root path {} is not a directory", self.root.display());
        }

        if cli.all {
            for id in ServiceId::ALL {
                self.service_mut(id).enabled = true;
            }
        }
        if cli.http {
            self.http.enabled = true;
        }
        if cli.https {
            self.https.enabled = true;
        }
        if cli.ftp {
            self.ftp.enabled = true;
        }
        if cli.sftp || cli.scp {
            self.ssh.enabled = true;
        }
        if cli.tftp {
            self.tftp.enabled = true;
        }

        if let Some(p) = cli.port_http {
            self.http.port = p;
        }
        if let Some(p) = cli.port_https {
            self.https.port = p;
        }
        if let Some(p) = cli.port_ftp {
            self.ftp.port = p;
        }
        if let Some(p) = cli.port_sftp {
            self.ssh.port = p;
        }
        if let Some(p) = cli.port_tftp {
            self.tftp.port = p;
        }

        if let Some(iface) = &cli.interface {
            self.advertise = Some(iface.clone());
        }
        if let Some(bind) = &cli.bind {
            bind.parse::<std::net::IpAddr>()
                .map_err(|_| anyhow::anyhow!("invalid bind address: {bind}"))?;
            for id in ServiceId::ALL {
                self.service_mut(id).bind = bind.clone();
            }
        }

        for id in ServiceId::ALL {
            let sc = self.service_mut(id);
            if sc.port == 0 {
                sc.port = id.default_port(privileged);
            }
            if sc.bind.is_empty() {
                sc.bind = "0.0.0.0".into();
            }
        }

        if let Some(user) = &cli.username {
            self.auth.username = user.clone();
        }
        if let Some(pass) = &cli.password {
            self.auth.password = pass.clone();
        }
        if cli.uploads {
            self.uploads.enabled = true;
        }
        if let Some(dir) = &cli.upload_dir {
            self.uploads.dir = dir.clone();
        }
        if let Some(m) = cli.max_upload_mib {
            self.uploads.max_upload_mib = m;
        }
        if let Some(level) = &cli.log_level {
            self.log_level = level.parse().map_err(|e: String| anyhow::anyhow!(e))?;
        }
        if let Some(f) = &cli.log_file {
            self.log_file = Some(f.clone());
            self.log_to_file = true;
        }
        if let Some(n) = cli.max_sessions {
            self.max_sessions = n.max(1);
        }
        if cli.no_sound {
            self.sound = false;
        }
        if cli.no_intro {
            self.intro = false;
        }

        // Credentials: CLI password > stored secret > cisco123. Nothing is
        // ever generated, so an unattended start is always cisco / cisco123.
        let secrets_path = self.config_dir.join("state").join("secrets.toml");
        if cli.password.is_none() {
            match crate::auth::load_password(&secrets_path) {
                Ok(pass) => self.auth.password = pass,
                Err(e) => bail!("could not prepare credentials: {e:#}"),
            }
        }
        Ok(())
    }

    pub fn validate(&self, privileged: bool) -> Result<()> {
        for id in ServiceId::ALL {
            let sc = self.service(id);
            if sc.enabled && sc.port < 1024 && !privileged {
                bail!(
                    "{} is configured for privileged port {} but transferbuddy was not started \
                     with root privileges.\n  Either run: sudo transferbuddy ...\n  or use an \
                     unprivileged port, e.g. --port-{} {}",
                    id.display_name(),
                    sc.port,
                    id.flag_name(),
                    id.default_port(false)
                );
            }
            sc.bind.parse::<std::net::IpAddr>().map_err(|_| {
                anyhow::anyhow!(
                    "invalid bind address for {}: {}",
                    id.display_name(),
                    sc.bind
                )
            })?;
        }
        if self.uploads.enabled {
            let dir = self.upload_dir_abs();
            if let Ok(meta) = std::fs::metadata(&dir) {
                if !meta.is_dir() {
                    bail!("upload directory {} is not a directory", dir.display());
                }
            }
        }
        Ok(())
    }

    /// Address a device should be told to fetch from, for a service bound to
    /// `bind`. `peer` is the device's own address when it is known.
    pub fn advertised_ip(&self, bind: &str, peer: Option<&std::net::IpAddr>) -> Option<std::net::IpAddr> {
        crate::netif::advertised_ip(self.advertise.as_deref(), bind, peer)
    }

    pub fn upload_dir_abs(&self) -> PathBuf {
        if self.uploads.dir.is_empty() {
            self.root.clone()
        } else {
            self.root.join(&self.uploads.dir)
        }
    }

    pub fn log_file_path(&self) -> Option<PathBuf> {
        if !self.log_to_file {
            return None;
        }
        Some(
            self.log_file
                .clone()
                .unwrap_or_else(|| self.config_dir.join("logs").join("transferbuddy.log")),
        )
    }

    pub fn certificates_dir(&self) -> PathBuf {
        self.config_dir.join("certificates")
    }

    pub fn ssh_dir(&self) -> PathBuf {
        self.config_dir.join("ssh")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ServiceId;
    use clap::Parser;

    #[test]
    fn default_ports_by_privilege() {
        assert_eq!(ServiceId::Http.default_port(true), 80);
        assert_eq!(ServiceId::Http.default_port(false), 8080);
        assert_eq!(ServiceId::Https.default_port(true), 443);
        assert_eq!(ServiceId::Https.default_port(false), 8443);
        assert_eq!(ServiceId::Ftp.default_port(true), 21);
        assert_eq!(ServiceId::Ftp.default_port(false), 2121);
        assert_eq!(ServiceId::Ssh.default_port(true), 22);
        assert_eq!(ServiceId::Ssh.default_port(false), 2222);
        assert_eq!(ServiceId::Tftp.default_port(true), 69);
        assert_eq!(ServiceId::Tftp.default_port(false), 6969);
    }

    #[test]
    fn privileged_port_rejected_without_root() {
        let mut c = Config::default();
        c.http.enabled = true;
        c.http.port = 80;
        c.http.bind = "0.0.0.0".into();
        for id in ServiceId::ALL {
            if c.service(id).bind.is_empty() {
                c.service_mut(id).port = id.default_port(false);
                c.service_mut(id).bind = "0.0.0.0".into();
            }
        }
        let err = c.validate(false).unwrap_err().to_string();
        assert!(err.contains("sudo"), "unexpected message: {err}");
        assert!(c.validate(true).is_ok());
    }

    #[test]
    fn missing_advertise_interface_does_not_block_startup() {
        let mut c = Config::default();
        for id in ServiceId::ALL { c.service_mut(id).bind = "0.0.0.0".into(); }
        c.advertise = Some("utun-transferbuddy-nonexistent".into());
        c.validate(false).unwrap();
        assert_eq!(
            c.advertised_ip("127.0.0.1", None),
            Some("127.0.0.1".parse().unwrap())
        );
    }

    #[test]
    fn invalid_bind_rejected() {
        let mut c = Config::default();
        for id in ServiceId::ALL {
            c.service_mut(id).port = id.default_port(false);
            c.service_mut(id).bind = "0.0.0.0".into();
        }
        c.http.enabled = true;
        c.http.bind = "not-an-ip".into();
        assert!(c.validate(false).is_err());
    }

    #[test]
    fn stale_root_in_config_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "root = \"/old/pinned/path\"\nintro = false\n").unwrap();
        let mut cfg = Config::load(Some(&file)).unwrap();
        // Container-level `#[serde(default)]` fills the skipped field from
        // `Config::default()`, i.e. "." — which apply_cli resolves to the cwd.
        assert_eq!(cfg.root, PathBuf::from("."), "root must not come from config.toml");
        assert!(!cfg.intro, "other settings must still load");

        let cli = Cli::parse_from(["transferbuddy"]);
        cfg.apply_cli(&cli, false).unwrap();
        assert_eq!(cfg.root, std::env::current_dir().unwrap().canonicalize().unwrap());
    }

    #[test]
    fn config_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = Config::default();
        c.config_dir = dir.path().to_path_buf();
        c.http.enabled = true;
        c.http.port = 9999;
        c.auth.username = "svc".into();
        c.auth.password = "must-not-persist".into();
        c.root = PathBuf::from("/somewhere/else");
        c.save().unwrap();
        let text = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(!text.contains("must-not-persist"), "password leaked into config.toml");
        assert!(!text.contains("/somewhere/else"), "root leaked into config.toml");
        let loaded = Config::load(Some(&dir.path().join("config.toml"))).unwrap();
        assert!(loaded.http.enabled);
        assert_eq!(loaded.http.port, 9999);
        assert_eq!(loaded.auth.username, "svc");
        // The password never comes from config.toml — it falls back to the
        // fixed default until the secrets file or --password says otherwise.
        assert_eq!(loaded.auth.password, crate::auth::DEFAULT_PASSWORD);
    }
}
