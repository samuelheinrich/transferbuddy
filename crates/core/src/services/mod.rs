pub mod ftp;
pub mod http;
pub mod ssh;
pub mod tftp;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use tokio::sync::watch;

use crate::auth::Authenticator;
use crate::config::Config;
use crate::fsroot::SecureRoot;
use crate::logging::{Event, LogLevel, Logger};
use crate::session::SessionManager;
use crate::App;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceId {
    Ftp,
    Http,
    Https,
    /// SFTP and SCP share this one SSH service.
    Ssh,
    Tftp,
}

impl ServiceId {
    pub const ALL: [ServiceId; 5] = [
        ServiceId::Http,
        ServiceId::Https,
        ServiceId::Ftp,
        ServiceId::Ssh,
        ServiceId::Tftp,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            ServiceId::Ftp => "FTP",
            ServiceId::Http => "HTTP",
            ServiceId::Https => "HTTPS",
            ServiceId::Ssh => "SFTP/SCP",
            ServiceId::Tftp => "TFTP",
        }
    }

    pub fn flag_name(self) -> &'static str {
        match self {
            ServiceId::Ftp => "ftp",
            ServiceId::Http => "http",
            ServiceId::Https => "https",
            ServiceId::Ssh => "sftp",
            ServiceId::Tftp => "tftp",
        }
    }

    pub fn default_port(self, privileged: bool) -> u16 {
        match (self, privileged) {
            (ServiceId::Ftp, true) => 21,
            (ServiceId::Ftp, false) => 2121,
            (ServiceId::Http, true) => 80,
            (ServiceId::Http, false) => 8080,
            (ServiceId::Https, true) => 443,
            (ServiceId::Https, false) => 8443,
            (ServiceId::Ssh, true) => 22,
            (ServiceId::Ssh, false) => 2222,
            (ServiceId::Tftp, true) => 69,
            (ServiceId::Tftp, false) => 6969,
        }
    }

    /// FTP, HTTP and TFTP transmit everything in cleartext.
    pub fn encrypted(self) -> bool {
        matches!(self, ServiceId::Https | ServiceId::Ssh)
    }

    pub fn log_proto(self) -> &'static str {
        match self {
            ServiceId::Ftp => "ftp",
            ServiceId::Http => "http",
            ServiceId::Https => "https",
            ServiceId::Ssh => "ssh",
            ServiceId::Tftp => "tftp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Failed(String),
}

impl ServiceStatus {
    pub fn label(&self) -> &'static str {
        match self {
            ServiceStatus::Stopped => "Stopped",
            ServiceStatus::Starting => "Starting",
            ServiceStatus::Running => "Running",
            ServiceStatus::Stopping => "Stopping",
            ServiceStatus::Failed(_) => "Failed",
        }
    }
    pub fn is_running(&self) -> bool {
        matches!(self, ServiceStatus::Running)
    }
}

impl std::fmt::Display for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceStatus::Failed(e) => write!(f, "Failed: {e}"),
            other => write!(f, "{}", other.label()),
        }
    }
}

/// State shared between the manager and a running service task.
pub struct ServiceShared {
    pub status: Mutex<ServiceStatus>,
    pub started_at: Mutex<Option<Instant>>,
    receive_files: Mutex<HashMap<String, (std::net::IpAddr, Instant)>>,
}

impl ServiceShared {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            status: Mutex::new(ServiceStatus::Stopped),
            started_at: Mutex::new(None),
            receive_files: Mutex::new(HashMap::new()),
        })
    }
}

/// Everything a protocol adapter needs to run, captured at start time.
#[derive(Clone)]
pub struct ServiceCtx {
    pub id: ServiceId,
    pub cfg: Config,
    pub root: Arc<SecureRoot>,
    pub logger: Arc<Logger>,
    pub sessions: Arc<SessionManager>,
    pub auth: Arc<Authenticator>,
    pub shutdown: watch::Receiver<bool>,
    shared: Arc<ServiceShared>,
}

impl ServiceCtx {
    /// A TUI receive authorizes one exact path from one connected device for one hour.
    pub fn take_receive_file(&self, path: &str, peer: std::net::IpAddr) -> bool {
        let mut files = self.shared.receive_files.lock().unwrap();
        files.retain(|_, (_, at)| at.elapsed() < std::time::Duration::from_secs(3600));
        if files.get(path).is_some_and(|(ip, _)| *ip == peer) {
            files.remove(path);
            true
        } else {
            false
        }
    }

    pub fn bind_addr(&self) -> SocketAddr {
        let sc = self.cfg.service(self.id);
        let ip = sc
            .bind
            .parse()
            .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        SocketAddr::new(ip, sc.port)
    }

    /// Adapter calls this once its listener is bound.
    pub fn set_running(&self) {
        *self.shared.status.lock().unwrap() = ServiceStatus::Running;
        *self.shared.started_at.lock().unwrap() = Some(Instant::now());
        let sc = self.cfg.service(self.id);
        self.logger.log(Event::new(
            LogLevel::Info,
            self.id.log_proto(),
            format!("listening on {}:{}", sc.bind, sc.port),
        ));
        if !self.id.encrypted() {
            self.logger.log(Event::new(
                LogLevel::Warning,
                self.id.log_proto(),
                "unencrypted protocol — credentials and data are visible on the network",
            ));
        }
        if sc.bind == "0.0.0.0" || sc.bind == "::" {
            self.logger.log(Event::new(
                LogLevel::Warning,
                self.id.log_proto(),
                "listening on all interfaces — anyone on the network can reach this service",
            ));
        }
    }

    #[allow(dead_code)] // part of the adapter API; some adapters poll it
    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// True once the per-service session limit is reached.
    pub fn at_session_limit(&self) -> bool {
        self.sessions.active_count() >= self.cfg.max_sessions
    }
}

/// In-progress upload: data goes to a uniquely named `.part` temp file and
/// is only renamed to the final name after a successful transfer. Dropping
/// the guard without `finalize()` removes the temp file.
pub struct UploadGuard {
    pub file: Option<tokio::fs::File>,
    temp_path: std::path::PathBuf,
    final_path: std::path::PathBuf,
    rel: String,
    finalized: bool,
    no_replace: bool,
}

impl UploadGuard {
    pub async fn finalize(mut self) -> Result<String, String> {
        if let Some(f) = self.file.take() {
            f.sync_all()
                .await
                .map_err(|e| format!("sync failed: {e}"))?;
        }
        if self.no_replace {
            tokio::fs::hard_link(&self.temp_path, &self.final_path)
                .await
                .map_err(|e| format!("finalizing receive failed: {e}"))?;
            let _ = tokio::fs::remove_file(&self.temp_path).await;
        } else {
            tokio::fs::rename(&self.temp_path, &self.final_path)
                .await
                .map_err(|e| format!("finalizing upload failed: {e}"))?;
        }
        self.finalized = true;
        Ok(self.rel.clone())
    }
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        if !self.finalized {
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}

/// Validate and prepare an upload of `size` bytes named `name` (no paths —
/// uploads always land flat in the configured upload directory).
pub async fn begin_upload(ctx: &ServiceCtx, name: &str, size: u64) -> Result<UploadGuard, String> {
    if !ctx.cfg.uploads.enabled {
        return Err("uploads are disabled".into());
    }
    crate::fsroot::validate_filename(name)?;
    let limit = ctx.cfg.uploads.max_upload_mib;
    if limit > 0 && size > limit * 1024 * 1024 {
        return Err(format!("file exceeds upload limit of {limit} MiB"));
    }
    let dir = ctx.cfg.upload_dir_abs();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("upload directory not writable: {e}"))?;
    // The upload dir must stay inside the root.
    let canonical = dir
        .canonicalize()
        .map_err(|e| format!("upload directory not accessible: {e}"))?;
    if !canonical.starts_with(ctx.root.root()) {
        return Err("upload directory escapes the shared root".into());
    }
    if let Some(free) = crate::fsroot::free_disk_space(&canonical) {
        if size > 0 && size + 64 * 1024 * 1024 > free {
            return Err(format!(
                "not enough disk space (need {}, {} free)",
                crate::session::fmt_bytes(size),
                crate::session::fmt_bytes(free)
            ));
        }
    }
    let final_path = canonical.join(name);
    if final_path.exists() && !ctx.cfg.uploads.overwrite {
        return Err(format!(
            "file {name} already exists (overwrite is disabled)"
        ));
    }
    let unique: u32 = rand::random();
    let temp_path = canonical.join(format!(".{name}.part-{unique:08x}"));
    let file = tokio::fs::File::create(&temp_path)
        .await
        .map_err(|e| format!("cannot create upload file: {e}"))?;
    let rel = match ctx.root.relative(&final_path) {
        Some(r) => r,
        None => name.to_string(),
    };
    Ok(UploadGuard {
        file: Some(file),
        temp_path,
        final_path,
        rel,
        finalized: false,
        no_replace: false,
    })
}

/// Explicit TUI receives use the selected local directory; ordinary uploads
/// continue to use the configured upload directory and upload policy.
pub async fn begin_receive(ctx: &ServiceCtx, rel: &str) -> Result<UploadGuard, String> {
    let final_path = ctx.root.resolve_for_write(rel).map_err(|e| e.to_string())?;
    if final_path.exists() {
        return Err(
            "local file already exists — choose another directory or rename it first".into(),
        );
    }
    let parent = final_path.parent().ok_or("missing destination directory")?;
    let name = final_path
        .file_name()
        .ok_or("missing filename")?
        .to_string_lossy();
    let temp_path = parent.join(format!(".{name}.part-{:08x}", rand::random::<u32>()));
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(UploadGuard {
        file: Some(file),
        temp_path,
        final_path,
        rel: rel.into(),
        finalized: false,
        no_replace: true,
    })
}

struct ServiceState {
    shutdown_tx: Option<watch::Sender<bool>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

struct ManagerInner {
    rt: tokio::runtime::Handle,
    logger: Arc<Logger>,
    sessions: Arc<SessionManager>,
    states: Mutex<HashMap<ServiceId, Arc<ServiceShared>>>,
    running: Mutex<HashMap<ServiceId, ServiceState>>,
    app: Mutex<Weak<App>>,
}

#[derive(Clone)]
pub struct ServiceManager {
    inner: Arc<ManagerInner>,
}

impl ServiceManager {
    pub fn new(
        rt: tokio::runtime::Handle,
        logger: Arc<Logger>,
        sessions: Arc<SessionManager>,
    ) -> Self {
        let mut states = HashMap::new();
        for id in ServiceId::ALL {
            states.insert(id, ServiceShared::new());
        }
        Self {
            inner: Arc::new(ManagerInner {
                rt,
                logger,
                sessions,
                states: Mutex::new(states),
                running: Mutex::new(HashMap::new()),
                app: Mutex::new(Weak::new()),
            }),
        }
    }

    pub fn attach_app(&self, app: &Arc<App>) {
        *self.inner.app.lock().unwrap() = Arc::downgrade(app);
    }

    fn shared(&self, id: ServiceId) -> Arc<ServiceShared> {
        self.inner.states.lock().unwrap().get(&id).unwrap().clone()
    }

    pub fn status(&self, id: ServiceId) -> ServiceStatus {
        self.shared(id).status.lock().unwrap().clone()
    }

    pub fn authorize_receive(&self, path: String, peer: std::net::IpAddr) {
        self.shared(ServiceId::Ftp)
            .receive_files
            .lock()
            .unwrap()
            .insert(path, (peer, Instant::now()));
    }
    pub fn revoke_receive(&self, path: &str) {
        self.shared(ServiceId::Ftp)
            .receive_files
            .lock()
            .unwrap()
            .remove(path);
    }

    pub fn uptime(&self, id: ServiceId) -> Option<std::time::Duration> {
        if !self.status(id).is_running() {
            return None;
        }
        self.shared(id)
            .started_at
            .lock()
            .unwrap()
            .map(|t| t.elapsed())
    }

    fn set_status(&self, id: ServiceId, status: ServiceStatus) {
        *self.shared(id).status.lock().unwrap() = status;
    }

    pub fn start(&self, id: ServiceId) {
        let Some(app) = self.app() else { return };
        {
            let status = self.status(id);
            if matches!(status, ServiceStatus::Running | ServiceStatus::Starting) {
                return;
            }
        }
        let cfg = app.config.read().unwrap().clone();
        let root = match SecureRoot::new(&cfg.root) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                let msg = format!("root directory not accessible: {e}");
                self.inner.logger.log(
                    Event::new(LogLevel::Error, id.log_proto(), "start failed").error(msg.clone()),
                );
                self.set_status(id, ServiceStatus::Failed(msg));
                return;
            }
        };

        let auth = Arc::new(Authenticator::new(
            &cfg.auth.username,
            &cfg.auth.password,
            cfg.auth.max_login_failures,
            cfg.auth.login_lockout_secs,
        ));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let shared = self.shared(id);
        *shared.status.lock().unwrap() = ServiceStatus::Starting;

        let ctx = ServiceCtx {
            id,
            cfg,
            root,
            logger: self.inner.logger.clone(),
            sessions: self.inner.sessions.clone(),
            auth,
            shutdown: shutdown_rx,
            shared: shared.clone(),
        };
        let mgr = self.clone();
        let task = self.inner.rt.spawn(async move {
            let result = match id {
                ServiceId::Http => http::run(ctx.clone(), false).await,
                ServiceId::Https => http::run(ctx.clone(), true).await,
                ServiceId::Tftp => tftp::run(ctx.clone()).await,
                ServiceId::Ftp => ftp::run(ctx.clone()).await,
                ServiceId::Ssh => ssh::run(ctx.clone()).await,
            };
            match result {
                Ok(()) => {
                    mgr.set_status(id, ServiceStatus::Stopped);
                    mgr.inner
                        .logger
                        .log(Event::new(LogLevel::Info, id.log_proto(), "stopped"));
                }
                Err(e) => {
                    mgr.inner.logger.log(
                        Event::new(LogLevel::Error, id.log_proto(), "service failed")
                            .error(e.clone()),
                    );
                    mgr.set_status(id, ServiceStatus::Failed(e));
                }
            }
            *mgr.shared(id).started_at.lock().unwrap() = None;
        });
        self.inner.running.lock().unwrap().insert(
            id,
            ServiceState {
                shutdown_tx: Some(shutdown_tx),
                task: Some(task),
            },
        );
    }

    pub fn stop(&self, id: ServiceId) {
        let state = self.inner.running.lock().unwrap().remove(&id);
        if let Some(state) = state {
            if self.status(id).is_running() || self.status(id) == ServiceStatus::Starting {
                self.set_status(id, ServiceStatus::Stopping);
            }
            if let Some(tx) = state.shutdown_tx {
                let _ = tx.send(true);
            }
            // The task notices the shutdown signal and exits; status becomes
            // Stopped in its wrapper.
            drop(state.task);
        } else if self.status(id) != ServiceStatus::Stopped {
            self.set_status(id, ServiceStatus::Stopped);
        }
    }

    pub fn restart(&self, id: ServiceId) {
        let task = {
            let mut running = self.inner.running.lock().unwrap();
            match running.remove(&id) {
                Some(mut state) => {
                    self.set_status(id, ServiceStatus::Stopping);
                    if let Some(tx) = state.shutdown_tx.take() {
                        let _ = tx.send(true);
                    }
                    state.task.take()
                }
                None => None,
            }
        };
        let mgr = self.clone();
        self.inner.rt.spawn(async move {
            if let Some(task) = task {
                let _ = task.await;
            }
            mgr.start(id);
        });
    }

    /// The shared `App`, without holding the lock afterwards. Taking the guard
    /// into an `if let` would keep it alive for the whole block, and every
    /// method that does real work locks it again — that deadlocks.
    fn app(&self) -> Option<Arc<App>> {
        let app = self.inner.app.lock().unwrap().upgrade();
        app
    }

    pub fn start_all_enabled(&self) {
        let Some(app) = self.app() else { return };
        let enabled: Vec<ServiceId> = {
            let cfg = app.config.read().unwrap();
            ServiceId::ALL
                .iter()
                .copied()
                .filter(|id| cfg.service(*id).enabled)
                .collect()
        };
        for id in enabled {
            self.start(id);
        }
    }

    pub fn stop_all(&self) {
        for id in ServiceId::ALL {
            self.stop(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::{LogLevel, Logger};

    #[tokio::test]
    async fn ftp_receive_is_scoped_to_peer_and_path_and_round_trips_arbitrary_files() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut cfg = Config {
            root: dir.path().to_path_buf(),
            ..Default::default()
        };
        cfg.ftp.port = port;
        cfg.ftp.bind = "127.0.0.1".into();
        cfg.uploads.enabled = false;
        let shared = ServiceShared::new();
        let (shutdown, rx) = watch::channel(false);
        let ctx = ServiceCtx {
            id: ServiceId::Ftp,
            root: Arc::new(SecureRoot::new(dir.path()).unwrap()),
            logger: Arc::new(Logger::new(LogLevel::Debug, None, false)),
            sessions: Arc::new(SessionManager::new(600)),
            auth: Arc::new(Authenticator::new("cisco", "secret", 5, 60)),
            cfg,
            shutdown: rx,
            shared: shared.clone(),
        };
        let server_ctx = ctx.clone();
        let server = tokio::spawn(async move { ftp::run(server_ctx).await });
        let stream = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                    break stream;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let (read, mut write) = stream.into_split();
        let mut read = BufReader::new(read);
        async fn response(
            read: &mut BufReader<tokio::net::tcp::OwnedReadHalf>,
            code: &str,
        ) -> String {
            let mut line = String::new();
            tokio::time::timeout(std::time::Duration::from_secs(5), read.read_line(&mut line))
                .await
                .unwrap()
                .unwrap();
            assert!(line.starts_with(code), "expected {code}, got {line}");
            line
        }
        response(&mut read, "220").await;
        write.write_all(b"USER cisco\r\n").await.unwrap();
        response(&mut read, "331").await;
        write.write_all(b"PASS secret\r\n").await.unwrap();
        response(&mut read, "230").await;
        shared.receive_files.lock().unwrap().insert(
            "nested/data.txt".into(),
            ("192.0.2.1".parse().unwrap(), Instant::now()),
        );
        write.write_all(b"STOR nested/data.txt\r\n").await.unwrap();
        response(&mut read, "550").await;
        assert!(shared
            .receive_files
            .lock()
            .unwrap()
            .contains_key("nested/data.txt"));
        shared.receive_files.lock().unwrap().insert(
            "nested/data.txt".into(),
            ("127.0.0.1".parse().unwrap(), Instant::now()),
        );
        write.write_all(b"EPSV\r\n").await.unwrap();
        let epsv = response(&mut read, "229").await;
        let data_port: u16 = epsv
            .split("|||")
            .nth(1)
            .unwrap()
            .split('|')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut data = tokio::net::TcpStream::connect(("127.0.0.1", data_port))
            .await
            .unwrap();
        write.write_all(b"STOR nested/data.txt\r\n").await.unwrap();
        response(&mut read, "150").await;
        data.write_all(b"arbitrary content\n").await.unwrap();
        data.shutdown().await.unwrap();
        drop(data);
        response(&mut read, "226").await;
        assert_eq!(
            std::fs::read(dir.path().join("nested/data.txt")).unwrap(),
            b"arbitrary content\n"
        );
        assert!(!dir.path().join("data.txt").exists());
        assert!(!shared
            .receive_files
            .lock()
            .unwrap()
            .contains_key("nested/data.txt"));
        write.write_all(b"STOR nested/data.txt\r\n").await.unwrap();
        response(&mut read, "550").await;
        // The same server also serves the file back from its exact local path.
        write.write_all(b"EPSV\r\n").await.unwrap();
        let epsv = response(&mut read, "229").await;
        let data_port: u16 = epsv
            .split("|||")
            .nth(1)
            .unwrap()
            .split('|')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let mut data = tokio::net::TcpStream::connect(("127.0.0.1", data_port))
            .await
            .unwrap();
        write.write_all(b"RETR nested/data.txt\r\n").await.unwrap();
        response(&mut read, "150").await;
        let mut body = Vec::new();
        data.read_to_end(&mut body).await.unwrap();
        response(&mut read, "226").await;
        assert_eq!(body, b"arbitrary content\n");
        for path in ["../outside.txt", "escape/outside.txt"] {
            shared
                .receive_files
                .lock()
                .unwrap()
                .insert(path.into(), ("127.0.0.1".parse().unwrap(), Instant::now()));
            write
                .write_all(format!("STOR {path}\r\n").as_bytes())
                .await
                .unwrap();
            response(&mut read, "550").await;
        }
        assert!(!outside.path().join("outside.txt").exists());
        let mut guard = begin_receive(&ctx, "nested/race.txt").await.unwrap();
        guard
            .file
            .as_mut()
            .unwrap()
            .write_all(b"new")
            .await
            .unwrap();
        std::fs::write(dir.path().join("nested/race.txt"), b"existing").unwrap();
        assert!(guard.finalize().await.is_err());
        assert_eq!(
            std::fs::read(dir.path().join("nested/race.txt")).unwrap(),
            b"existing"
        );
        assert!(!std::fs::read_dir(dir.path().join("nested"))
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().contains(".part-")));
        shutdown.send(true).unwrap();
        server.await.unwrap().unwrap();
    }

    /// A real `App` with one service enabled on a free port.
    fn test_app() -> (tokio::runtime::Runtime, Arc<App>, u16) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let mut cfg = Config {
            root: std::env::temp_dir(),
            ..Default::default()
        };
        cfg.http.enabled = true;
        cfg.http.port = port;
        cfg.http.bind = "127.0.0.1".into();
        let logger = Arc::new(Logger::new(LogLevel::Debug, None, false));
        let sessions = Arc::new(SessionManager::new(600));
        let services = ServiceManager::new(rt.handle().clone(), logger.clone(), sessions.clone());
        let switches = crate::switch::SwitchManager::new(rt.handle().clone(), logger.clone());
        let app = Arc::new(App {
            config: std::sync::RwLock::new(cfg),
            logger,
            sessions,
            services,
            switches,
            privileged: false,
            runtime: rt.handle().clone(),
            engine_state: Default::default(),
        });
        app.services.attach_app(&app);
        (rt, app, port)
    }

    /// `S` in the services view used to hang the whole TUI: the `App` guard
    /// was held across `start()`, which locks it again.
    #[test]
    fn start_all_enabled_does_not_deadlock() {
        let (_rt, app, _port) = test_app();
        let services = app.clone();
        let done = std::thread::spawn(move || {
            services.services.start_all_enabled();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "start_all_enabled deadlocked"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        done.join().unwrap();
        app.services.stop_all();
    }

    /// The same for the single-service path, which the deadlock fix touched.
    #[test]
    fn start_and_stop_are_reentrant_safe() {
        let (_rt, app, _port) = test_app();
        let done = std::thread::spawn(move || {
            app.services.start(ServiceId::Http);
            app.services.stop(ServiceId::Http);
            app.services.start_all_enabled();
            app.services.stop_all();
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !done.is_finished() {
            assert!(
                std::time::Instant::now() < deadline,
                "service control deadlocked"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        done.join().unwrap();
    }
}
