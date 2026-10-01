mod auth;
mod certs;
mod cisco;
mod cli;
mod config;
mod deploy;
mod fsroot;
mod logging;
mod netif;
mod services;
mod session;
mod sound;
mod sshkeys;
mod switch;
mod tui;

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;

use crate::cli::Cli;
use crate::config::Config;
use crate::logging::{LogLevel, Logger};
use crate::services::{ServiceId, ServiceManager};
use crate::session::SessionManager;
use crate::switch::SwitchManager;

/// One full version for the TUI, CLI and release builds.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Everything the services, the TUI and the headless runner share.
pub struct App {
    pub config: std::sync::RwLock<Config>,
    pub logger: Arc<Logger>,
    pub sessions: Arc<SessionManager>,
    pub services: ServiceManager,
    /// Open SSH sessions to network devices.
    pub switches: SwitchManager,
    pub privileged: bool,
    /// Handle of the async runtime, so the (synchronous) TUI can spawn work
    /// such as a deploy session.
    pub runtime: tokio::runtime::Handle,
}

pub type SharedApp = Arc<App>;

fn main() -> ExitCode {
    let cli = Cli::parse();

    let privileged = unsafe { libc::geteuid() } == 0;

    let mut config = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: failed to load configuration: {e:#}");
            return ExitCode::from(2);
        }
    };
    if let Err(e) = config.apply_cli(&cli, privileged) {
        eprintln!("error: {e:#}");
        return ExitCode::from(2);
    }
    if let Err(e) = config.validate(privileged) {
        eprintln!("error: invalid configuration: {e:#}");
        return ExitCode::from(2);
    }

    let logger = Arc::new(Logger::new(
        config.log_level,
        config.log_file_path(),
        cli.no_tui,
    ));
    if let Some(pin) = config.advertise.as_deref() {
        if netif::resolve_advertise(pin).is_none() {
            logger.log_simple(
                LogLevel::Warning,
                "core",
                format!("interface {pin} is unavailable — using automatic address selection"),
            );
        }
    }
    let sessions = Arc::new(SessionManager::new(config.session_history_secs));

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };

    let services = ServiceManager::new(runtime.handle().clone(), logger.clone(), sessions.clone());
    let switches = SwitchManager::new(runtime.handle().clone(), logger.clone());
    let app: SharedApp = Arc::new(App {
        config: std::sync::RwLock::new(config),
        logger: logger.clone(),
        sessions: sessions.clone(),
        services,
        switches,
        privileged,
        runtime: runtime.handle().clone(),
    });
    app.services.attach_app(&app);
    sessions.start_metrics_task(runtime.handle());

    logger.log_simple(
        LogLevel::Info,
        "core",
        format!(
            "transferbuddy starting (root: {}, privileged: {})",
            app.config.read().unwrap().root.display(),
            privileged
        ),
    );

    // The TUI always comes up with every service stopped — nothing is exposed
    // to the network until you say so (Space/s in the services view, or S for
    // all enabled ones). Headless mode has no such control, so it starts the
    // enabled services itself.
    if cli.no_tui {
        for id in ServiceId::ALL {
            if app.config.read().unwrap().service(id).enabled {
                app.services.start(id);
            }
        }
    }

    let code = if cli.no_tui {
        run_headless(&runtime, &app)
    } else {
        match tui::run(app.clone()) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: TUI failed: {e:#}");
                1
            }
        }
    };

    // Graceful shutdown of all listeners and device sessions.
    app.services.stop_all();
    app.switches.disconnect_all();
    runtime.shutdown_timeout(std::time::Duration::from_secs(3));
    ExitCode::from(code)
}

/// `--no-tui`: print structured status + logs to the terminal until Ctrl-C.
fn run_headless(runtime: &tokio::runtime::Runtime, app: &SharedApp) -> u8 {
    let mut rx = app.logger.subscribe();
    // Register the SIGINT handler immediately so an early Ctrl-C still
    // shuts down cleanly instead of killing the process.
    let (sig_tx, mut sig_rx) = tokio::sync::oneshot::channel::<()>();
    runtime.spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = sig_tx.send(());
    });
    // Give services a moment to bind, then print a status block.
    runtime.block_on(async {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    });
    print_status(app);

    let any_enabled = {
        let cfg = app.config.read().unwrap();
        ServiceId::ALL.iter().any(|id| cfg.service(*id).enabled)
    };
    if !any_enabled {
        eprintln!("error: no service enabled (use --http, --tftp, ... or --all)");
        return 2;
    }
    let any_running = ServiceId::ALL.iter().any(|id| app.services.status(*id).is_running());
    if !any_running {
        eprintln!("error: no service could be started");
        return 1;
    }

    runtime.block_on(async {
        loop {
            tokio::select! {
                _ = &mut sig_rx => break,
                entry = rx.recv() => {
                    match entry {
                        Ok(e) => println!("{}", e.render_line()),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            }
        }
    });
    println!("shutting down ...");
    0
}

fn print_status(app: &SharedApp) {
    let cfg = app.config.read().unwrap();
    println!("transferbuddy v{}", crate::VERSION);
    println!("  root:              {}", cfg.root.display());
    println!(
        "  address in URLs:   {}{}",
        cfg.advertised_ip("0.0.0.0", None)
            .map(|i| i.to_string())
            .unwrap_or_else(|| "-".into()),
        match &cfg.advertise {
            Some(pin) if netif::resolve_advertise(pin).is_some() =>
                format!("  ({pin}, pinned with --interface)"),
            Some(pin) => format!("  ({pin} unavailable — automatic selection)"),
            None => "  (automatic — pin one with --interface)".into(),
        }
    );
    for ifa in netif::candidates() {
        println!("    {:<8} {:<16} {}", ifa.name, ifa.ip.to_string(), ifa.kind.label());
    }
    for id in ServiceId::ALL {
        let sc = cfg.service(id);
        let status = app.services.status(id);
        println!(
            "  {:<9} {:<8} port {:<5} bind {:<15} {}",
            id.display_name(),
            status.label(),
            sc.port,
            sc.bind,
            if sc.enabled { "enabled" } else { "disabled" }
        );
    }
}
