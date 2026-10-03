pub mod auth;
pub mod certs;
pub mod cisco;
pub mod config;
pub mod deploy;
pub mod engine;
pub mod files;
pub mod fsroot;
pub mod logging;
pub mod netif;
pub mod platform;
pub mod services;
pub mod session;
pub mod sshkeys;
pub mod startup;
pub mod switch;
pub mod upgrade;
pub mod workspace;

use config::Config;
use logging::Logger;
use services::ServiceManager;
use session::SessionManager;
pub use startup::StartupOptions;
use std::sync::Arc;
use switch::SwitchManager;
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
    pub engine_state: std::sync::OnceLock<Arc<engine::Engine>>,
}

pub type SharedApp = Arc<App>;

impl App {
    pub fn new(config: Config, runtime: tokio::runtime::Handle, stdout: bool) -> SharedApp {
        let logger = Arc::new(Logger::new(
            config.log_level,
            config.log_file_path(),
            stdout,
        ));
        let sessions = Arc::new(SessionManager::new(config.session_history_secs));
        let services = ServiceManager::new(runtime.clone(), logger.clone(), sessions.clone());
        let switches = SwitchManager::new(runtime.clone(), logger.clone());
        switches.set_auto_trust(config.auto_accept_host_keys);
        let app = Arc::new(Self {
            config: std::sync::RwLock::new(config),
            logger,
            sessions,
            services,
            switches,
            privileged: platform::is_privileged(),
            runtime,
            engine_state: Default::default(),
        });
        app.services.attach_app(&app);
        app.sessions.start_metrics_task(&app.runtime);
        app.engine();
        app
    }
    pub fn engine(self: &Arc<Self>) -> Arc<engine::Engine> {
        self.engine_state
            .get_or_init(|| engine::Engine::new(Arc::downgrade(self)))
            .clone()
    }
    pub fn shutdown(&self) {
        if let Some(scan) = self.switches.scan() {
            scan.cancel();
        }
        self.services.stop_all();
        self.switches.disconnect_all();
    }
}

pub use vt100 as terminal_types;
pub mod sound;
pub mod terminal;
