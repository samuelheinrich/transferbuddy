use std::path::PathBuf;

use clap::Parser;

/// transferbuddy — serve a local directory to Cisco network devices over
/// FTP / HTTP / HTTPS / SCP / SFTP / TFTP, with a keyboard-driven TUI.
#[derive(Parser, Debug, Clone)]
#[command(name = "transferbuddy", version = crate::VERSION, about)]
pub struct Cli {
    /// Directory to share (defaults to the current working directory)
    #[arg(long, value_name = "DIR")]
    pub root: Option<PathBuf>,

    /// Local interface or address to put into generated URLs (e.g. en5),
    /// for a machine with more than one network
    #[arg(long, value_name = "NAME|IP")]
    pub interface: Option<String>,

    /// Enable the HTTP service
    #[arg(long)]
    pub http: bool,
    /// Enable the HTTPS service
    #[arg(long)]
    pub https: bool,
    /// Enable the FTP service
    #[arg(long)]
    pub ftp: bool,
    /// Enable the SFTP service (shares the SSH service with SCP)
    #[arg(long)]
    pub sftp: bool,
    /// Enable the SCP service (shares the SSH service with SFTP)
    #[arg(long)]
    pub scp: bool,
    /// Enable the TFTP service
    #[arg(long)]
    pub tftp: bool,
    /// Enable all services
    #[arg(long)]
    pub all: bool,

    /// HTTP port (default: 80 with root privileges, 8080 without)
    #[arg(long, value_name = "PORT")]
    pub port_http: Option<u16>,
    /// HTTPS port (default: 443 / 8443)
    #[arg(long, value_name = "PORT")]
    pub port_https: Option<u16>,
    /// FTP control port (default: 21 / 2121)
    #[arg(long, value_name = "PORT")]
    pub port_ftp: Option<u16>,
    /// SFTP/SCP (SSH) port (default: 22 / 2222)
    #[arg(long, value_name = "PORT", alias = "sftp-port", alias = "scp-port")]
    pub port_sftp: Option<u16>,
    /// TFTP port (default: 69 / 6969)
    #[arg(long, value_name = "PORT")]
    pub port_tftp: Option<u16>,

    /// Bind address for all services (e.g. 0.0.0.0, 127.0.0.1 or a specific local IP)
    #[arg(long, value_name = "ADDR")]
    pub bind: Option<String>,

    /// Username for FTP/SFTP/SCP authentication
    #[arg(long, value_name = "USER")]
    pub username: Option<String>,
    /// Password for FTP/SFTP/SCP authentication (default: cisco123)
    #[arg(long, value_name = "PASS")]
    pub password: Option<String>,

    /// Allow uploads (write access). Off by default.
    #[arg(long)]
    pub uploads: bool,
    /// Upload target directory (relative to root; created if missing)
    #[arg(long, value_name = "DIR")]
    pub upload_dir: Option<String>,
    /// Maximum upload size in MiB (0 = unlimited)
    #[arg(long, value_name = "MIB")]
    pub max_upload_mib: Option<u64>,

    /// Run without the TUI and log to stdout
    #[arg(long)]
    pub no_tui: bool,
    /// Skip the animated intro screen
    #[arg(long)]
    pub no_intro: bool,
    /// Mute the TUI beeps
    #[arg(long)]
    pub no_sound: bool,

    /// Log level (debug, info, warning, error)
    #[arg(long, value_name = "LEVEL")]
    pub log_level: Option<String>,
    /// Also write logs to this file
    #[arg(long, value_name = "FILE")]
    pub log_file: Option<PathBuf>,

    /// Use an alternative config file
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Maximum parallel sessions per service
    #[arg(long, value_name = "N")]
    pub max_sessions: Option<usize>,
}

impl From<&Cli> for transferbuddy_core::StartupOptions {
    fn from(cli: &Cli) -> Self {
        Self {
            root: cli.root.clone(),
            interface: cli.interface.clone(),
            http: cli.http,
            https: cli.https,
            ftp: cli.ftp,
            sftp: cli.sftp,
            scp: cli.scp,
            tftp: cli.tftp,
            all: cli.all,
            port_http: cli.port_http,
            port_https: cli.port_https,
            port_ftp: cli.port_ftp,
            port_sftp: cli.port_sftp,
            port_tftp: cli.port_tftp,
            bind: cli.bind.clone(),
            username: cli.username.clone(),
            password: cli.password.clone(),
            uploads: cli.uploads,
            upload_dir: cli.upload_dir.clone(),
            max_upload_mib: cli.max_upload_mib,
            no_tui: cli.no_tui,
            no_intro: cli.no_intro,
            no_sound: cli.no_sound,
            log_level: cli.log_level.clone(),
            log_file: cli.log_file.clone(),
            config: cli.config.clone(),
            max_sessions: cli.max_sessions,
        }
    }
}
