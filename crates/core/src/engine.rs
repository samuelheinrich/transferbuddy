//! Shared application operations. Frontends render snapshots and send intentions;
//! listener startup, queues and device jobs do not depend on a frontend tick.
use crate::{
    cisco, config, deploy, files, fsroot, logging, services, session, switch, upgrade, App,
    SharedApp,
};
use services::{ServiceId, ServiceStatus};
use session::Protocol;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use switch::{Job, Switch, SwitchState};
use tokio::sync::{broadcast, watch};
mod queue;
mod recovery;
mod workflow;
pub use queue::{TransferDetails, TransferRequest};
pub use recovery::{
    cached_copy_endpoint, copy_endpoint, CopyEndpoint, Failure, FailureKind, NetworkSnapshot,
    RecoveryPhase, RecoveryStatus,
};
pub use workflow::{
    Preflight, ProfileTarget, QueueReview, TransferInspection, WorkProfile, WorkflowGoal,
    WorkflowSnapshot, WorkflowSpec, WorkflowTarget,
};

pub type OperationId = u64;
pub type DeviceId = u64;
pub type RequestId = u64;
#[derive(Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub enable_password: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkTargets {
    pub direct: Vec<String>,
    pub subnets: Vec<String>,
}
pub fn bulk_targets(input: &str) -> Result<BulkTargets, String> {
    let mut result = BulkTargets {
        direct: Vec::new(),
        subnets: Vec::new(),
    };
    for part in input
        .split(|c: char| c.is_whitespace() || c == ',' || c == ';')
        .filter(|p| !p.is_empty())
    {
        if part.contains('/') {
            switch::subnet_hosts(part)?;
            if !result.subnets.iter().any(|s| s == part) {
                result.subnets.push(part.into());
            }
        } else {
            let ip: std::net::IpAddr = part
                .parse()
                .map_err(|_| format!("invalid IP address or subnet: {part}"))?;
            let ip = ip.to_string();
            if !result.direct.contains(&ip) {
                result.direct.push(ip);
            }
        }
    }
    if result.direct.is_empty() && result.subnets.is_empty() {
        return Err("enter at least one device IP or subnet".into());
    }
    Ok(result)
}
pub const PROTOCOL_PRIORITY: [Protocol; 6] = [
    Protocol::Sftp,
    Protocol::Https,
    Protocol::Ftp,
    Protocol::Http,
    Protocol::Scp,
    Protocol::Tftp,
];
pub fn protocol_options(app: &App, receive: bool) -> Vec<Protocol> {
    if receive {
        return vec![Protocol::Ftp];
    }
    let active: Vec<_> = PROTOCOL_PRIORITY
        .into_iter()
        .filter(|p| app.services.status(cisco::service_of(*p)).is_running())
        .collect();
    if !active.is_empty() {
        return active;
    }
    let cfg = app.config.read().unwrap();
    let enabled: Vec<_> = PROTOCOL_PRIORITY
        .into_iter()
        .filter(|p| cfg.service(cisco::service_of(*p)).enabled)
        .collect();
    if enabled.is_empty() {
        PROTOCOL_PRIORITY.to_vec()
    } else {
        enabled
    }
}
pub fn copy_commands(app: &App, rel: &str) -> Result<Vec<(Protocol, String)>, String> {
    let cfg = app.config.read().unwrap().clone();
    fsroot::SecureRoot::new(&cfg.root)
        .map_err(|e| e.to_string())?
        .resolve(rel)
        .map_err(|e| e.to_string())?;
    let mut commands = Vec::new();
    for p in PROTOCOL_PRIORITY {
        let id = cisco::service_of(p);
        if cfg.service(id).enabled || app.services.status(id).is_running() {
            if let Some(ip) = cfg.advertised_ip(&cfg.service(id).bind, None) {
                commands.push((p, cisco::copy_command(p, &cfg, &ip, rel)));
            }
        }
    }
    Ok(commands)
}
/// Every business action must have an entry point in both interactive adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Workflow,
    Profiles,
    Report,
    Review,
    Services,
    Settings,
    Root,
    Connect,
    BulkConnect,
    CancelScan,
    Reconnect,
    Disconnect,
    RemoveDevice,
    ClearFinished,
    Refresh,
    Cli,
    ListLocal,
    ListRemote,
    Hash,
    CopyCommands,
    Protocol,
    Send,
    Receive,
    Queue,
    Retry,
    Resume,
    Cancel,
    Delete,
    AssignImage,
    Deploy,
    Verify,
    Install,
    Yolo,
    RemoveInactive,
    Confirmation,
    Logs,
    Shutdown,
}
pub const ACTIONS: &[Action] = &[
    Action::Workflow,
    Action::Profiles,
    Action::Report,
    Action::Review,
    Action::Services,
    Action::Settings,
    Action::Root,
    Action::Connect,
    Action::BulkConnect,
    Action::CancelScan,
    Action::Reconnect,
    Action::Disconnect,
    Action::RemoveDevice,
    Action::ClearFinished,
    Action::Refresh,
    Action::Cli,
    Action::ListLocal,
    Action::ListRemote,
    Action::Hash,
    Action::CopyCommands,
    Action::Protocol,
    Action::Send,
    Action::Receive,
    Action::Queue,
    Action::Retry,
    Action::Resume,
    Action::Cancel,
    Action::Delete,
    Action::AssignImage,
    Action::Deploy,
    Action::Verify,
    Action::Install,
    Action::Yolo,
    Action::RemoveInactive,
    Action::Confirmation,
    Action::Logs,
    Action::Shutdown,
];
#[derive(Clone)]
pub enum Setting {
    Service(ServiceId, config::ServiceConfig),
    Credentials(String, String),
    Uploads(config::UploadConfig),
    Advertise(Option<String>),
    AutoAcceptHostKeys(bool),
    Sound(bool),
    SpeedInBits(bool),
    LogLevel(logging::LogLevel),
    Intro(bool),
}
pub enum Command {
    SaveWorkflow {
        previous: Option<u64>,
        spec: WorkflowSpec,
    },
    StartWorkflow(u64),
    StopWorkflowServices(u64),
    SaveProfile {
        name: String,
        workflow: u64,
    },
    ExportWorkflow {
        workflow: u64,
        local: String,
    },
    StartService(ServiceId),
    StopService(ServiceId),
    RestartService(ServiceId),
    StartEnabled,
    StopAll,
    Set(Setting),
    SetMany(Vec<Setting>),
    ChangeRoot(PathBuf),
    Connect {
        host: String,
        port: u16,
        credentials: Credentials,
        bulk: bool,
    },
    BulkConnect {
        targets: String,
        port: u16,
        credentials: Credentials,
    },
    CancelScan,
    Reconnect(DeviceId),
    ReconnectMany(Vec<DeviceId>),
    UpdateCredentials(DeviceId, Credentials),
    Disconnect(DeviceId),
    RemoveDevice(DeviceId),
    ClearFinished,
    Refresh(DeviceId),
    OpenCli(DeviceId),
    CloseCli(DeviceId),
    CliInput(DeviceId, Vec<u8>),
    ResizeCli(DeviceId, u16, u16),
    ListLocal(String),
    ListRemote(DeviceId, String),
    Hash(String),
    ChooseProtocol(DeviceId, Protocol),
    Transfer {
        device: DeviceId,
        local: String,
        remote: String,
        receive: bool,
    },
    QueueTransfers(Vec<TransferRequest>),
    InspectOperation(OperationId),
    RequestOverwriteRetry(OperationId),
    ReviewPending(Vec<DeviceId>),
    ResumeReviewed(Vec<DeviceId>),
    RetryOperation(OperationId),
    ResumeQueue(DeviceId),
    CancelOperation(OperationId),
    AssignImage {
        devices: Vec<DeviceId>,
        local: String,
    },
    Deploy(DeviceId, Protocol),
    Verify(DeviceId),
    RequestDelete {
        device: DeviceId,
        remote: String,
        recursive: bool,
    },
    RequestWorkflowInstall {
        workflow: u64,
        device: DeviceId,
        yolo: bool,
    },
    RequestInstall {
        device: DeviceId,
        yolo: bool,
    },
    RemoveInactive(DeviceId),
    Reply {
        request: RequestId,
        input: String,
    },
    Shutdown,
}
#[derive(Debug, Clone)]
pub enum ConfirmationKind {
    OverwriteRetry {
        operation: OperationId,
        remote: String,
    },
    Delete {
        remote: String,
        recursive: bool,
    },
    Install {
        workflow: Option<u64>,
        remote: String,
        yolo: bool,
        md5: String,
        version: Option<String>,
    },
    HostKey {
        fingerprint: String,
    },
    Reload {
        prompt: String,
    },
    Cleanup {
        files: Vec<String>,
        warning: Option<String>,
    },
}
#[derive(Debug, Clone)]
pub struct Confirmation {
    pub id: RequestId,
    pub device: DeviceId,
    pub kind: ConfirmationKind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationState {
    Queued,
    Preparing,
    Paused,
    Running,
    Stopping,
    Complete,
    Cancelled,
    Uncertain(String),
    Failed(String),
}
impl OperationState {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Preparing | Self::Paused | Self::Running | Self::Stopping
        )
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Preparing => "Preparing",
            Self::Paused => "Waiting for connection / resume",
            Self::Running => "Running",
            Self::Stopping => "Stopping…",
            Self::Complete => "Complete",
            Self::Cancelled => "Cancelled",
            Self::Uncertain(_) => "Remote status unknown",
            Self::Failed(_) => "Failed",
        }
    }
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed(e) | Self::Uncertain(e) => Some(e),
            _ => None,
        }
    }
}
#[derive(Debug, Clone)]
pub struct Operation {
    pub id: OperationId,
    pub device: Option<DeviceId>,
    pub label: String,
    pub state: OperationState,
    pub transfer: Option<TransferDetails>,
}
#[derive(Clone)]
pub struct DeviceSnapshot {
    pub id: DeviceId,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub name: String,
    pub state: SwitchState,
    pub facts: switch::Facts,
    pub reach: switch::Reach,
    pub recovery: RecoveryStatus,
    pub upgrade: upgrade::Progress,
    pub assigned: Option<(String, u64)>,
    pub protocol: Protocol,
    pub transfer: Option<switch::Transfer>,
    pub transcript: Vec<switch::Line>,
    pub live: String,
    pub terminal: Option<vt100::Screen>,
    pub output_revision: u64,
}
#[derive(Clone)]
pub struct ServiceSnapshot {
    pub id: ServiceId,
    pub settings: config::ServiceConfig,
    pub status: ServiceStatus,
}
#[derive(Clone, Default)]
pub struct AppSnapshot {
    pub root: PathBuf,
    pub network: NetworkSnapshot,
    pub advertise: Option<String>,
    pub workflows: Vec<WorkflowSnapshot>,
    pub profiles: Vec<WorkProfile>,
    pub queue_reviews: Vec<QueueReview>,
    pub services: Vec<ServiceSnapshot>,
    pub devices: Vec<DeviceSnapshot>,
    pub transfers: Vec<session::SessionInfo>,
    pub scan: Option<switch::ScanProgress>,
    pub operations: Vec<Operation>,
    pub confirmations: Vec<Confirmation>,
    pub listings: HashMap<String, Result<Vec<files::FileEntry>, String>>,
    pub listing_revision: u64,
    pub hashes: HashMap<String, Result<files::FileHashes, String>>,
}
impl AppSnapshot {
    pub fn running_jobs(&self) -> bool {
        self.operations.iter().any(|o| {
            matches!(
                o.state,
                OperationState::Preparing | OperationState::Running | OperationState::Stopping
            )
        }) || self.devices.iter().any(|d| {
            matches!(
                d.state,
                SwitchState::Busy { .. } | SwitchState::Connecting | SwitchState::Rebooting
            )
        }) || self.scan.as_ref().is_some_and(|s| !s.finished)
            || self
                .transfers
                .iter()
                .any(|t| t.state == session::SessionState::Transferring)
    }
    pub fn active_jobs(&self) -> bool {
        self.operations.iter().any(|o| o.state.is_active())
            || self.devices.iter().any(|d| {
                matches!(
                    d.state,
                    SwitchState::Busy { .. }
                        | SwitchState::Connecting
                        | SwitchState::HostKey { .. }
                        | SwitchState::Rebooting
                        | SwitchState::ReloadConfirm { .. }
                        | SwitchState::CleanupConfirm { .. }
                )
            })
            || self.scan.as_ref().is_some_and(|s| !s.finished)
            || self
                .transfers
                .iter()
                .any(|t| t.state.is_active() && t.file.is_some())
    }
}
#[derive(Clone)]
pub enum AppEvent {
    Changed,
    Finished(Operation),
    Confirmation(Confirmation),
}
#[derive(Default)]
struct State {
    network: NetworkSnapshot,
    workflows: Vec<WorkflowSnapshot>,
    profiles: Vec<WorkProfile>,
    queue_reviews: Vec<QueueReview>,
    workflow_revision: u64,
    network_observed: bool,
    workflow_reports: HashMap<u64, String>,
    recoveries: HashMap<DeviceId, RecoveryStatus>,
    queue: queue::Queue,
    assignments: HashMap<DeviceId, (String, u64)>,
    operations: Vec<Operation>,
    pending: HashMap<RequestId, Confirmation>,
    device_questions: HashMap<DeviceId, (SwitchState, RequestId)>,
    listings: HashMap<String, Result<Vec<files::FileEntry>, String>>,
    listing_revision: u64,
    hashes: HashMap<String, Result<files::FileHashes, String>>,
}
pub struct Engine {
    app: Weak<App>,
    submission: Mutex<()>,
    state: Mutex<State>,
    next: AtomicU64,
    stopping: AtomicBool,
    events: broadcast::Sender<AppEvent>,
    snapshots: watch::Sender<AppSnapshot>,
}
impl Engine {
    pub fn new(app: Weak<App>) -> Arc<Self> {
        let (events, _) = broadcast::channel(256);
        let (snapshots, _) = watch::channel(AppSnapshot::default());
        let engine = Arc::new(Self {
            app,
            submission: Mutex::new(()),
            state: Mutex::new(State::default()),
            next: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            events,
            snapshots,
        });
        if let Some(app) = engine.app.upgrade() {
            engine.load_profiles(&app);
            engine.start_recovery(&app);
            let weak = Arc::downgrade(&engine);
            app.runtime.spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_millis(250));
                let mut previous = String::new();
                loop {
                    interval.tick().await;
                    let Some(e) = weak.upgrade() else { break };
                    if e.app.upgrade().is_none() {
                        break;
                    }
                    e.dispatch_queue();
                    e.refresh_queue_progress();
                    e.discover_questions();
                    e.finish_workflows();
                    let token = e.change_token();
                    if token != previous {
                        previous = token;
                        e.snapshots.send_replace(e.snapshot());
                        let _ = e.events.send(AppEvent::Changed);
                    }
                }
            });
        }
        engine
    }
    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }
    pub fn watch(&self) -> watch::Receiver<AppSnapshot> {
        self.snapshots.subscribe()
    }
    pub fn app(&self) -> Result<SharedApp, String> {
        self.app
            .upgrade()
            .ok_or_else(|| "application is shutting down".into())
    }
    pub fn device(&self, id: DeviceId) -> Result<Arc<Switch>, String> {
        self.app()?
            .switches
            .list()
            .into_iter()
            .find(|s| s.id == id)
            .ok_or_else(|| "device no longer exists".into())
    }
    fn idle(&self, id: DeviceId) -> Result<Arc<Switch>, String> {
        let s = self.device(id)?;
        if s.state() != SwitchState::Ready
            || self
                .state
                .lock()
                .unwrap()
                .operations
                .iter()
                .any(|o| o.device == Some(id) && o.state.is_active())
        {
            return Err("device is busy or disconnected".into());
        }
        Ok(s)
    }
    pub fn assignment(&self, id: DeviceId) -> Option<(String, u64)> {
        self.state.lock().unwrap().assignments.get(&id).cloned()
    }
    pub fn busy(&self, id: DeviceId) -> bool {
        self.state
            .lock()
            .unwrap()
            .operations
            .iter()
            .any(|o| o.device == Some(id) && o.state.is_active())
    }
    fn new_operation(&self, id: OperationId, device: Option<DeviceId>, label: &str) {
        let mut state = self.state.lock().unwrap();
        if state.operations.len() >= 1000 {
            if let Some(i) = state.operations.iter().position(|o| !o.state.is_active()) {
                let old = state.operations.remove(i);
                state.queue.entries_remove(old.id);
            }
        }
        state.operations.push(Operation {
            id,
            device,
            label: label.into(),
            state: OperationState::Running,
            transfer: None,
        });
    }
    fn finish(&self, id: OperationId, result: Result<(), String>) {
        let mut state = self.state.lock().unwrap();
        if let Some(o) = state.operations.iter_mut().find(|o| o.id == id) {
            o.state = match result {
                Ok(()) => OperationState::Complete,
                Err(e) => OperationState::Failed(e),
            };
            if let Some(app) = self.app.upgrade() {
                if let OperationState::Failed(ref e) = o.state {
                    let mut event = logging::Event::new(
                        logging::LogLevel::Error,
                        o.transfer
                            .as_ref()
                            .map(|t| t.protocol.label())
                            .unwrap_or("core"),
                        format!("{}: {e}", o.label),
                    );
                    if let Some(sw) = o.device.and_then(|id| self.device(id).ok()) {
                        let facts = sw.facts();
                        event = event.device(
                            facts.hostname.unwrap_or_else(|| sw.host.clone()),
                            facts.version.and_then(|v| v.model),
                        );
                        if let Some(ip) = sw.transfer_peer() {
                            event = event.ip(ip);
                        }
                    }
                    if let Some(command) = o.transfer.as_ref().and_then(|t| t.command.clone()) {
                        event = event.result(command);
                    }
                    app.logger.log(event);
                }
            }
            let _ = self.events.send(AppEvent::Finished(o.clone()));
        }
    }
    /// Cheap change detection: never clone file listings, transcripts, hashes or terminal screens.
    pub fn change_token(&self) -> String {
        use std::fmt::Write;
        let Some(app) = self.app.upgrade() else {
            return String::new();
        };
        let mut token = format!(
            "{}:{}",
            self.next.load(Ordering::Relaxed),
            app.logger.revision()
        );
        {
            let state = self.state.lock().unwrap();
            let _ = write!(token, "{:?}:{:?}", state.network, state.recoveries);
            let _ = write!(token, ":{}", state.workflow_revision);
            for op in &state.operations {
                let _ = write!(token, "{:?}:{:?}", op.id, op.state);
                if let Some(t) = &op.transfer {
                    let _ = write!(
                        token,
                        "{}:{}:{}:{:?}:{}",
                        t.size,
                        t.bytes,
                        t.speed.to_bits(),
                        t.eta,
                        t.cancellable
                    );
                }
            }
            let _ = write!(
                token,
                "{}:{}:{}",
                state.pending.len(),
                state.listings.len(),
                state.hashes.len()
            );
        }
        for id in ServiceId::ALL {
            let _ = write!(token, "{:?}", app.services.status(id));
        }
        for sw in app.switches.list() {
            let _ = write!(
                token,
                "{}:{:?}:{:?}:{:?}:{:?}:{:?}:{}:{}",
                sw.id,
                sw.state(),
                sw.facts(),
                sw.reach(),
                sw.upgrade(),
                sw.protocol(),
                sw.output_revision(),
                sw.cli_open()
            );
        }
        for t in app.sessions.snapshot() {
            let _ = write!(
                token,
                "{}:{:?}:{}:{}",
                t.id,
                t.state,
                t.bytes,
                t.current_speed.to_bits()
            );
        }
        if let Some(scan) = app.switches.scan() {
            let scan = scan.progress();
            let _ = write!(
                token,
                "{}:{}:{}:{}",
                scan.checked, scan.total, scan.reachable, scan.finished
            );
        }
        token
    }
    pub fn snapshot(&self) -> AppSnapshot {
        let Some(app) = self.app.upgrade() else {
            return AppSnapshot::default();
        };
        let cfg = app.config.read().unwrap().clone();
        let state = self.state.lock().unwrap();
        let mut confirmations: Vec<_> = state.pending.values().cloned().collect();
        confirmations.sort_by_key(|c| c.id);
        AppSnapshot {
            root: cfg.root.clone(),
            network: state.network.clone(),
            advertise: cfg.advertise.clone(),
            workflows: state.workflows.clone(),
            profiles: state.profiles.clone(),
            queue_reviews: state.queue_reviews.clone(),
            services: ServiceId::ALL
                .into_iter()
                .map(|id| ServiceSnapshot {
                    id,
                    settings: cfg.service(id).clone(),
                    status: app.services.status(id),
                })
                .collect(),
            devices: app
                .switches
                .list()
                .iter()
                .map(|s| DeviceSnapshot {
                    id: s.id,
                    host: s.host.clone(),
                    port: s.port,
                    username: s.connection_username(),
                    name: s.display_name(),
                    state: s.state(),
                    facts: s.facts(),
                    reach: s.reach(),
                    recovery: state.recoveries.get(&s.id).cloned().unwrap_or_default(),
                    upgrade: s.upgrade(),
                    assigned: state.assignments.get(&s.id).cloned(),
                    protocol: s.protocol(),
                    transfer: s.transfer(&app.sessions),
                    transcript: s.transcript(),
                    live: s.live(),
                    terminal: s.terminal_screen(),
                    output_revision: s.output_revision(),
                })
                .collect(),
            transfers: app.sessions.snapshot(),
            scan: app.switches.scan().map(|s| s.progress()),
            operations: state.operations.clone(),
            confirmations,
            listings: state.listings.clone(),
            listing_revision: state.listing_revision,
            hashes: state.hashes.clone(),
        }
    }
    fn request(&self, id: RequestId, device: DeviceId, kind: ConfirmationKind) {
        let c = Confirmation { id, device, kind };
        self.state.lock().unwrap().pending.insert(id, c.clone());
        let _ = self.events.send(AppEvent::Confirmation(c));
    }
    fn discover_questions(&self) {
        let Ok(app) = self.app() else { return };
        let devices = app.switches.list();
        for sw in devices {
            let status = sw.state();
            let kind = match &status {
                SwitchState::HostKey { fingerprint } => Some(ConfirmationKind::HostKey {
                    fingerprint: fingerprint.clone(),
                }),
                SwitchState::ReloadConfirm { prompt } => Some(ConfirmationKind::Reload {
                    prompt: prompt.clone(),
                }),
                SwitchState::CleanupConfirm { files, warning } => Some(ConfirmationKind::Cleanup {
                    files: files.clone(),
                    warning: warning.clone(),
                }),
                _ => None,
            };
            let mut state = self.state.lock().unwrap();
            if let Some((old, _)) = state.device_questions.get(&sw.id) {
                if *old == status {
                    continue;
                }
            }
            if let Some((_, old)) = state.device_questions.remove(&sw.id) {
                state.pending.remove(&old);
            }
            if let Some(kind) = kind {
                let id = self.next.fetch_add(1, Ordering::Relaxed);
                let c = Confirmation {
                    id,
                    device: sw.id,
                    kind,
                };
                state.pending.insert(id, c.clone());
                state.device_questions.insert(sw.id, (status, id));
                let _ = self.events.send(AppEvent::Confirmation(c));
            }
        }
    }
    pub fn pending_confirmations(&self) -> Vec<Confirmation> {
        self.discover_questions();
        let mut pending: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .pending
            .values()
            .cloned()
            .collect();
        pending.sort_by_key(|c| c.id);
        pending
    }
    pub fn submit(self: &Arc<Self>, command: Command) -> Result<OperationId, String> {
        let _submission = self.submission.lock().unwrap();
        let app = self.app()?;
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        match command {
            Command::SaveWorkflow { previous, spec } => {
                return self.save_workflow(id, previous, spec)
            }
            Command::StartWorkflow(workflow) => self.start_workflow(workflow)?,
            Command::StopWorkflowServices(workflow) => self.stop_workflow_services(workflow)?,
            Command::SaveProfile { name, workflow } => self.save_profile(name, workflow)?,
            Command::ExportWorkflow { workflow, local } => self.export_workflow(workflow, local)?,
            Command::StartService(s) => app.services.start(s),
            Command::StopService(s) => app.services.stop(s),
            Command::RestartService(s) => app.services.restart(s),
            Command::StartEnabled => app.services.start_all_enabled(),
            Command::StopAll => app.services.stop_all(),
            Command::Set(setting) => self.set_many(vec![setting])?,
            Command::SetMany(settings) => self.set_many(settings)?,
            Command::ChangeRoot(root) => {
                if self.snapshot().active_jobs() {
                    return Err("finish active transfers and upgrades before changing root".into());
                }
                let root = root.canonicalize().map_err(|e| e.to_string())?;
                fsroot::SecureRoot::new(&root).map_err(|e| e.to_string())?;
                app.services.stop_all();
                app.config.write().unwrap().root = root;
                let mut s = self.state.lock().unwrap();
                s.assignments.clear();
                s.hashes.clear();
                s.listings.clear();
                s.pending.clear();
                s.device_questions.clear();
                for sw in app.switches.list() {
                    sw.set_upgrade(upgrade::Progress::Idle);
                }
            }
            Command::Connect {
                host,
                port,
                credentials,
                bulk,
            } => {
                self.connect(&host, port, credentials, bulk)?;
            }
            Command::BulkConnect {
                targets,
                port,
                credentials,
            } => {
                let targets = bulk_targets(&targets)?;
                Self::validate_credentials(port, &credentials)?;
                let target = self.target("", port, credentials.clone(), true)?;
                if !targets.subnets.is_empty() {
                    app.switches
                        .start_scan_many(target, &targets.subnets, &targets.direct)?;
                }
                for host in targets.direct {
                    self.connect(&host, port, credentials.clone(), true)?;
                }
            }
            Command::CancelScan => {
                if let Some(s) = app.switches.scan() {
                    s.cancel()
                }
            }
            Command::UpdateCredentials(d, credentials) => {
                self.device(d)?.update_credentials(
                    credentials.username,
                    credentials.password,
                    credentials.enable_password,
                )?;
                self.reset_recovery(d);
            }
            Command::ReconnectMany(devices) => {
                let switches = devices
                    .into_iter()
                    .map(|d| self.device(d))
                    .collect::<Result<Vec<_>, _>>()?;
                for sw in switches.into_iter().filter(|sw| sw.state().is_over()) {
                    self.reset_recovery(sw.id);
                    app.switches.reconnect(sw);
                }
            }
            Command::Reconnect(d) => {
                self.reset_recovery(d);
                if !app.switches.reconnect(self.device(d)?) {
                    return Err("device is connected or cannot reconnect".into());
                }
            }
            Command::Disconnect(d) => {
                let sw = self.device(d)?;
                sw.cancel();
                sw.submit(Job::Disconnect);
            }
            Command::RemoveDevice(d) => {
                let sw = self.device(d)?;
                if self.busy(d)
                    || matches!(
                        sw.state(),
                        SwitchState::Busy { .. }
                            | SwitchState::Rebooting
                            | SwitchState::ReloadConfirm { .. }
                            | SwitchState::CleanupConfirm { .. }
                    )
                {
                    return Err("finish or cancel device jobs before removing this device".into());
                }
                sw.close_cli();
                sw.cancel();
                app.switches.forget(d);
                let mut state = self.state.lock().unwrap();
                state.assignments.remove(&d);
                state.pending.retain(|_, c| c.device != d);
                state.queue.forget_device(d);
            }
            Command::ClearFinished => {
                if self
                    .state
                    .lock()
                    .unwrap()
                    .operations
                    .iter()
                    .any(|o| o.transfer.is_some() && o.state.is_active())
                {
                    return Err("finish or cancel queued jobs before removing devices".into());
                }
                app.switches.forget_closed();
            }
            Command::Refresh(d) => {
                let sw = self.device(d)?;
                if sw.state() == SwitchState::Ready {
                    let sw = self.idle(d)?;
                    self.schedule(id, sw, Job::Facts, None, None, "refresh device");
                } else if matches!(
                    sw.state(),
                    SwitchState::Busy { .. } | SwitchState::CleanupConfirm { .. }
                ) {
                    let before = sw.facts().updated;
                    if !app.switches.refresh_facts(sw.clone()) {
                        return Err("cannot refresh device while busy".into());
                    }
                    self.new_operation(id, None, "refresh device on second SSH session");
                    let e = self.clone();
                    app.runtime.spawn(async move {
                        while sw.facts_refreshing() {
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                        e.finish(
                            id,
                            if sw.facts().updated != before {
                                Ok(())
                            } else {
                                Err("facts refresh failed; see Logs".into())
                            },
                        );
                    });
                } else {
                    return Err("device is disconnected".into());
                }
                return Ok(id);
            }
            Command::OpenCli(d) => {
                let sw = self.idle(d)?;
                if !sw.start_cli_tracked(id) {
                    return Err("cannot open CLI".into());
                }
                self.new_operation(id, Some(d), "interactive CLI");
                self.observe_job(id, sw);
                return Ok(id);
            }
            Command::CloseCli(d) => self.device(d)?.close_cli(),
            Command::CliInput(d, bytes) => self.device(d)?.cli_send(bytes),
            Command::ResizeCli(d, rows, cols) => self.device(d)?.resize_cli(rows, cols),
            Command::ListRemote(d, path) => {
                deploy::check_command(&format!("dir {path}"))?;
                let sw = self.idle(d)?;
                self.schedule(id, sw, Job::List { path }, None, None, "list remote files");
                return Ok(id);
            }
            Command::ListLocal(path) => {
                let root = app.config.read().unwrap().root.clone();
                let e = self.clone();
                self.new_operation(id, None, "list local files");
                app.runtime.spawn(async move {
                    let rel = path.clone();
                    let result = tokio::task::spawn_blocking(move || {
                        let mut browser = files::FileBrowser::new();
                        browser.cwd = rel;
                        browser.refresh(&root);
                        if let Some(err) = browser.error {
                            Err(err)
                        } else {
                            Ok(browser.entries)
                        }
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                    let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
                    {
                        let mut state = e.state.lock().unwrap();
                        state.listings.insert(path, result);
                        state.listing_revision += 1;
                    }
                    e.finish(id, outcome);
                });
                return Ok(id);
            }
            Command::Hash(path) => {
                let cfg = app.config.read().unwrap().clone();
                let jail = fsroot::SecureRoot::new(&cfg.root).map_err(|e| e.to_string())?;
                let abs = jail.resolve(&path).map_err(|e| e.to_string())?;
                if !abs.is_file() {
                    return Err("select a file".into());
                }
                let e = self.clone();
                self.state.lock().unwrap().hashes.remove(&path);
                self.new_operation(id, None, "compute file hashes");
                app.runtime.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        files::compute_hashes(&abs)
                            .map(|(md5, sha256, sha512)| files::FileHashes {
                                md5,
                                sha256,
                                sha512,
                            })
                            .map_err(|e| e.to_string())
                    })
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                    let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
                    let mut state = e.state.lock().unwrap();
                    if state.hashes.len() >= 100 {
                        state.hashes.clear();
                    }
                    state.hashes.insert(path, result);
                    drop(state);
                    e.finish(id, outcome);
                });
                return Ok(id);
            }
            Command::ChooseProtocol(d, p) => self.device(d)?.choose_protocol(p),
            Command::Transfer {
                device,
                local,
                remote,
                receive,
            } => {
                let sw = self.device(device)?;
                let protocol = if receive {
                    Protocol::Ftp
                } else {
                    sw.protocol()
                };
                self.queue_transfers(
                    id,
                    vec![TransferRequest {
                        device,
                        local,
                        remote,
                        receive,
                        protocol,
                        overwrite: false,
                        platform_check: false,
                    }],
                )?;
                return Ok(id);
            }
            Command::QueueTransfers(requests) => {
                self.queue_transfers(id, requests)?;
                return Ok(id);
            }
            Command::InspectOperation(old) => {
                self.inspect_operation(old, id)?;
                return Ok(id);
            }
            Command::ReviewPending(devices) => {
                self.review_pending(devices, id)?;
                return Ok(id);
            }
            Command::ResumeReviewed(devices) => self.resume_reviewed(devices)?,
            Command::RequestOverwriteRetry(operation) => {
                let state = self.state.lock().unwrap();
                let op = state
                    .operations
                    .iter()
                    .find(|o| o.id == operation)
                    .ok_or("Job no longer exists")?;
                if !matches!(
                    op.state,
                    OperationState::Failed(_)
                        | OperationState::Cancelled
                        | OperationState::Uncertain(_)
                ) {
                    return Err("Select an interrupted copy job".into());
                }
                let device = op.device.ok_or("Select a device job")?;
                let remote = op
                    .transfer
                    .as_ref()
                    .ok_or("Select a copy job")?
                    .remote
                    .clone();
                drop(state);
                self.request(
                    id,
                    device,
                    ConfirmationKind::OverwriteRetry { operation, remote },
                );
            }
            Command::RetryOperation(old) => {
                self.retry_operation(old, id, false)?;
                return Ok(id);
            }
            Command::ResumeQueue(device) => self.resume_queue(device)?,
            Command::CancelOperation(operation) => self.cancel_operation(operation)?,
            Command::AssignImage { devices, local } => self.assign_image(devices, local)?,
            ref c @ (Command::Deploy(_, _) | Command::Verify(_)) => {
                let (d, p) = match c {
                    Command::Deploy(d, p) => (*d, *p),
                    Command::Verify(d) => (*d, Protocol::Http),
                    _ => unreachable!(),
                };
                self.deploy_image(id, d, p, matches!(c, Command::Verify(_)))?;
                return Ok(id);
            }
            Command::RequestDelete {
                device,
                remote,
                recursive,
            } => {
                self.idle(device)?;
                deploy::check_delete_path(&remote)?;
                self.request(id, device, ConfirmationKind::Delete { remote, recursive });
            }
            Command::RequestInstall { device, yolo } => {
                self.request_install(id, device, yolo, None)?
            }
            Command::RequestWorkflowInstall {
                workflow,
                device,
                yolo,
            } => {
                let expected = self
                    .state
                    .lock()
                    .unwrap()
                    .workflows
                    .iter()
                    .find(|w| {
                        w.id == workflow
                            && w.spec.goal == WorkflowGoal::Upgrade
                            && !w.jobs.is_empty()
                    })
                    .and_then(|w| w.spec.targets.iter().find(|t| t.device == device))
                    .map(|t| t.local.clone())
                    .ok_or("Device is not in this upgrade workflow")?;
                if self
                    .assignment(device)
                    .is_none_or(|(local, _)| local != expected)
                {
                    return Err(
                        "Image assignment changed; open the current workflow before installing"
                            .into(),
                    );
                }
                self.request_install(id, device, yolo, Some(workflow))?;
            }
            Command::RemoveInactive(d) => {
                let sw = self.idle(d)?;
                self.schedule(id, sw, Job::RemoveInactive, None, None, "remove inactive");
                return Ok(id);
            }
            Command::Reply { request, input } => {
                let c = self
                    .state
                    .lock()
                    .unwrap()
                    .pending
                    .remove(&request)
                    .ok_or("confirmation has expired")?;
                let sw = self.device(c.device)?;
                let yes = input == "y";
                match c.kind {
                    ConfirmationKind::OverwriteRetry { operation, .. } if yes => {
                        self.overwrite_retry(operation, id)?;
                        return Ok(id);
                    }
                    ConfirmationKind::HostKey { fingerprint } => {
                        if sw.state() != (SwitchState::HostKey { fingerprint }) {
                            return Err("host key request expired".into());
                        }
                        sw.answer_host_key(yes)
                    }
                    ConfirmationKind::Reload { prompt } => {
                        if sw.state() != (SwitchState::ReloadConfirm { prompt }) {
                            return Err("reload request expired".into());
                        }
                        sw.answer_reload(yes)
                    }
                    ConfirmationKind::Cleanup { files, warning } => {
                        if sw.state() != (SwitchState::CleanupConfirm { files, warning }) {
                            return Err("cleanup request expired".into());
                        }
                        sw.answer_cleanup(yes)
                    }
                    ConfirmationKind::Delete { remote, recursive } if yes => {
                        self.idle(sw.id)?;
                        self.schedule(
                            id,
                            sw,
                            Job::Delete {
                                path: remote,
                                recursive,
                            },
                            None,
                            None,
                            "delete remote entry",
                        );
                        return Ok(id);
                    }
                    ConfirmationKind::Install {
                        workflow,
                        remote,
                        yolo,
                        md5,
                        version,
                    } if yes => {
                        self.idle(sw.id)?;
                        if let Some(reason) = upgrade::install_blocker(
                            &sw.upgrade(),
                            &sw.facts().version.unwrap_or_default(),
                        ) {
                            return Err(reason);
                        }
                        if !matches!(sw.upgrade(),upgrade::Progress::Verified{remote:r,md5:m,version:v}if r==remote&&m==md5&&v==version)
                        {
                            return Err("image assignment changed".into());
                        }
                        let mut state = self.state.lock().unwrap();
                        let assigned = state
                            .assignments
                            .get(&sw.id)
                            .map(|(local, _)| local.clone());
                        let flow = state.workflows.iter_mut().rev().find(|w| {
                            if let Some(workflow) = workflow {
                                w.id == workflow
                            } else {
                                w.finished.is_none()
                                    && !w.jobs.is_empty()
                                    && w.spec.goal == WorkflowGoal::Upgrade
                                    && w.spec.targets.iter().any(|t| {
                                        t.device == sw.id && Some(&t.local) == assigned.as_ref()
                                    })
                            }
                        });
                        if let Some(flow) = flow {
                            flow.jobs.push(id);
                            flow.finished = None;
                            flow.verified_hashes.insert(sw.id, md5);
                            let flow_id = flow.id;
                            state.workflow_reports.remove(&flow_id);
                            state.workflow_revision += 1;
                        }
                        drop(state);
                        self.schedule(id, sw, Job::Install { yolo }, None, None, "install image");
                        return Ok(id);
                    }
                    _ => {}
                }
            }
            Command::Shutdown => {
                self.stopping.store(true, Ordering::Release);
                app.shutdown();
            }
        }
        Ok(id)
    }
    fn validate_credentials(port: u16, c: &Credentials) -> Result<(), String> {
        if port == 0 {
            return Err("SSH port must be greater than zero".into());
        }
        if c.username.trim().is_empty() || c.password.is_empty() {
            return Err("enter username and password".into());
        }
        Ok(())
    }
    fn target(
        &self,
        host: &str,
        port: u16,
        c: Credentials,
        _bulk: bool,
    ) -> Result<switch::Target, String> {
        Self::validate_credentials(port, &c)?;
        Ok(switch::Target {
            host: host.trim().into(),
            port,
            username: c.username.trim().into(),
            password: c.password,
            enable_password: c.enable_password,
            known_hosts: self
                .app()?
                .config
                .read()
                .unwrap()
                .config_dir
                .join("state/known_hosts"),
            auto_trust: self.app()?.config.read().unwrap().auto_accept_host_keys,
        })
    }
    fn connect(
        &self,
        host: &str,
        port: u16,
        c: Credentials,
        bulk: bool,
    ) -> Result<Arc<Switch>, String> {
        if host.trim().is_empty() {
            return Err("enter the switch IP or hostname".into());
        }
        let target = self.target(host, port, c, bulk)?;
        Ok(self.app()?.switches.connect(target))
    }
    fn request_install(
        &self,
        id: OperationId,
        device: DeviceId,
        yolo: bool,
        workflow: Option<u64>,
    ) -> Result<(), String> {
        let sw = self.idle(device)?;
        if let Some(reason) =
            upgrade::install_blocker(&sw.upgrade(), &sw.facts().version.unwrap_or_default())
        {
            return Err(reason);
        }
        let upgrade::Progress::Verified {
            remote,
            md5,
            version,
        } = sw.upgrade()
        else {
            unreachable!()
        };
        self.request(
            id,
            device,
            ConfirmationKind::Install {
                workflow,
                remote,
                yolo,
                md5,
                version,
            },
        );
        Ok(())
    }
    fn assign_image(&self, devices: Vec<DeviceId>, local: String) -> Result<(), String> {
        let app = self.app()?;
        let cfg = app.config.read().unwrap().clone();
        let path = fsroot::SecureRoot::new(&cfg.root)
            .map_err(|e| e.to_string())?
            .resolve(&local)
            .map_err(|e| e.to_string())?;
        if !path.is_file() {
            return Err("select a local image".into());
        }
        let size = path.metadata().map_err(|e| e.to_string())?.len();
        let switches: Vec<_> = devices
            .iter()
            .map(|d| self.device(*d))
            .collect::<Result<_, _>>()?;
        if switches.iter().any(|sw| {
            self.busy(sw.id) || (sw.state().is_live() && sw.state() != SwitchState::Ready)
        }) {
            return Err("device is busy; wait before changing its image".into());
        }
        let mut state = self.state.lock().unwrap();
        for sw in switches {
            let file = (local.clone(), size);
            if state.assignments.get(&sw.id) != Some(&file) {
                sw.set_upgrade(upgrade::Progress::Idle);
                state.pending.retain(|_, c| {
                    c.device != sw.id || !matches!(c.kind, ConfirmationKind::Install { .. })
                });
            }
            state.assignments.insert(sw.id, file);
        }
        Ok(())
    }
    fn deploy_image(
        self: &Arc<Self>,
        id: OperationId,
        d: DeviceId,
        p: Protocol,
        verify: bool,
    ) -> Result<(), String> {
        let app = self.app()?;
        let sw = self.idle(d)?;
        let (rel, size) = self.assignment(d).ok_or("assign an image first")?;
        let cfg = app.config.read().unwrap().clone();
        let local_path = fsroot::SecureRoot::new(&cfg.root)
            .map_err(|e| e.to_string())?
            .resolve(&rel)
            .map_err(|e| e.to_string())?;
        let remote = format!(
            "{}{}",
            sw.facts().flash_device,
            rel.rsplit('/').next().unwrap()
        );
        let protocol = if verify { sw.protocol() } else { p };
        let job = if verify {
            Job::VerifyUpgrade {
                rel_path: rel,
                local_path,
                remote,
            }
        } else {
            let peer = sw.transfer_peer();
            let ip = cfg
                .advertised_ip(
                    &cfg.service(cisco::service_of(protocol)).bind,
                    peer.as_ref(),
                )
                .ok_or("no local address to advertise")?;
            let command = cisco::deploy_command(protocol, &cfg, &ip, &rel, &remote)?;
            Job::PrepareUpgrade {
                overwrite: false,
                rel_path: rel,
                local_path,
                remote,
                command,
                size,
                protocol,
            }
        };
        self.queue_image(
            id,
            sw,
            job,
            if verify { None } else { Some(protocol) },
            if verify {
                "verify image"
            } else {
                "upload & verify"
            },
        );
        Ok(())
    }
    fn transfer_job(
        &self,
        sw: &Switch,
        local: &str,
        remote: &str,
        receive: bool,
        protocol: Protocol,
    ) -> Result<(Job, u64), String> {
        let app = self.app()?;
        let cfg = app.config.read().unwrap().clone();
        let jail = fsroot::SecureRoot::new(&cfg.root).map_err(|e| e.to_string())?;
        deploy::check_storage_path(remote)?;
        let size = if receive {
            let abs = jail.resolve_for_write(local).map_err(|e| e.to_string())?;
            if abs.exists() {
                return Err("local file already exists".into());
            }
            let parent = remote
                .rsplit_once('/')
                .map(|(p, _)| p.to_string())
                .unwrap_or_else(|| format!("{}:", remote.split_once(':').unwrap().0));
            let name = remote.rsplit(['/', ':']).next().unwrap();
            let entries = sw.listing(&parent).ok_or("remote listing not loaded")??;
            let size = entries
                .iter()
                .find(|e| e.name == name && !e.is_dir)
                .ok_or("select a remote file")?
                .size;
            if cfg.uploads.max_upload_mib > 0 && size > cfg.uploads.max_upload_mib * 1024 * 1024 {
                return Err("file exceeds configured upload limit".into());
            }
            if fsroot::free_disk_space(abs.parent().ok_or("invalid local destination")?)
                .is_some_and(|f| size.saturating_add(64 * 1024 * 1024) > f)
            {
                return Err("not enough local disk space".into());
            }
            size
        } else {
            let abs = jail.resolve(local).map_err(|e| e.to_string())?;
            if !abs.is_file() {
                return Err("select a local file".into());
            }
            abs.metadata().map_err(|e| e.to_string())?.len()
        };
        let peer = sw.transfer_peer();
        let ip = copy_endpoint(&cfg, protocol, peer.as_ref())?.ip;
        let command = if receive {
            cisco::receive_command(&cfg, &ip, remote, local)?
        } else {
            cisco::deploy_command(protocol, &cfg, &ip, local, remote)?
        };
        Ok((
            Job::Copy {
                rel_path: local.into(),
                command,
                overwrite: false,
                platform_check: false,
            },
            size,
        ))
    }
    fn schedule(
        self: &Arc<Self>,
        id: OperationId,
        sw: Arc<Switch>,
        job: Job,
        protocol: Option<Protocol>,
        transfer: Option<(String, u64, bool)>,
        label: &str,
    ) {
        let Ok(app) = self.app() else { return };
        self.new_operation(id, Some(sw.id), label);
        let e = self.clone();
        let ready = protocol.is_none_or(|p| app.services.status(cisco::service_of(p)).is_running());
        if ready {
            let result = self.enqueue(id, &app, &sw, job, transfer);
            if let Err(err) = result {
                self.finish(id, Err(err));
                return;
            }
            self.observe_job(id, sw);
            return;
        }
        if let Some(p) = protocol {
            app.services.start(cisco::service_of(p));
        }
        app.runtime.clone().spawn(async move {
            let started = Instant::now();
            loop {
                if sw.state() != SwitchState::Ready {
                    e.finish(id, Err("device became busy or disconnected".into()));
                    return;
                }
                match app.services.status(cisco::service_of(protocol.unwrap())) {
                    ServiceStatus::Running => break,
                    ServiceStatus::Failed(err) => {
                        e.finish(id, Err(err));
                        return;
                    }
                    _ if started.elapsed() > Duration::from_secs(15) => {
                        e.finish(id, Err("service did not start within 15 seconds".into()));
                        return;
                    }
                    _ => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
            match e.enqueue(id, &app, &sw, job, transfer) {
                Ok(()) => e.observe_job(id, sw),
                Err(err) => e.finish(id, Err(err)),
            }
        });
    }
    fn enqueue(
        &self,
        id: OperationId,
        app: &App,
        sw: &Switch,
        job: Job,
        transfer: Option<(String, u64, bool)>,
    ) -> Result<(), String> {
        if sw.state() != SwitchState::Ready {
            return Err("device became busy".into());
        }
        if let Some((rel, size, receive)) = transfer {
            if receive {
                app.services.authorize_receive(
                    rel.clone(),
                    sw.transfer_peer().ok_or("device IP is unknown")?,
                );
                sw.begin_receive(rel, size)
            } else {
                sw.begin_transfer(rel, size, sw.protocol())
            }
        }
        if sw.submit_tracked(id, job) {
            Ok(())
        } else {
            Err("SSH session is closed".into())
        }
    }
    fn observe_job(self: &Arc<Self>, id: OperationId, sw: Arc<Switch>) {
        let Ok(app) = self.app() else { return };
        let e = self.clone();
        app.runtime.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(50)).await;
                if let Some(result) = sw.take_tracked_result(id) {
                    e.finish(id, result.map(|_| ()));
                    break;
                }
                if sw.state().is_over() {
                    e.finish(
                        id,
                        Err(sw
                            .last_result()
                            .and_then(Result::err)
                            .unwrap_or_else(|| "device disconnected".into())),
                    );
                    break;
                }
                if e.app.upgrade().is_none() {
                    break;
                }
            }
        });
    }
    fn set_many(&self, settings: Vec<Setting>) -> Result<(), String> {
        let app = self.app()?;
        let mut cfg = app.config.read().unwrap().clone();
        let mut restart = Vec::new();
        let endpoint_changed = settings.iter().any(|s| {
            matches!(
                s,
                Setting::Advertise(_)
                    | Setting::Service(_, _)
                    | Setting::Credentials(_, _)
                    | Setting::Uploads(_)
            )
        });
        let old_password = cfg.auth.password.clone();
        for setting in settings {
            match setting {
                Setting::Service(id, s) => {
                    if s.port == 0 {
                        return Err("port must be greater than zero".into());
                    }
                    s.bind
                        .parse::<std::net::IpAddr>()
                        .map_err(|_| "invalid bind address")?;
                    *cfg.service_mut(id) = s;
                    restart.push(id);
                }
                Setting::Credentials(user, password) => {
                    if user.trim().is_empty() || password.is_empty() {
                        return Err("username and password must not be empty".into());
                    }
                    cfg.auth.username = user;
                    cfg.auth.password = password;
                    restart.extend([ServiceId::Ftp, ServiceId::Ssh]);
                }
                Setting::Uploads(upload) => {
                    let root = fsroot::SecureRoot::new(&cfg.root).map_err(|e| e.to_string())?;
                    root.resolve(&upload.dir)
                        .or_else(|_| root.resolve_for_write(&upload.dir))
                        .map_err(|e| e.to_string())?;
                    cfg.uploads = upload;
                    restart.extend(ServiceId::ALL);
                }
                Setting::Advertise(value) => {
                    if value
                        .as_deref()
                        .is_some_and(|s| crate::netif::resolve_advertise(s).is_none())
                    {
                        return Err("interface or address is unavailable".into());
                    }
                    cfg.advertise = value;
                }
                Setting::AutoAcceptHostKeys(value) => cfg.auto_accept_host_keys = value,
                Setting::Sound(value) => cfg.sound = value,
                Setting::SpeedInBits(value) => cfg.speed_in_bits = value,
                Setting::LogLevel(value) => {
                    cfg.log_level = value;
                }
                Setting::Intro(value) => cfg.intro = value,
            }
        }
        cfg.validate(app.privileged).map_err(|e| e.to_string())?;
        if cfg.auth.password != old_password {
            crate::auth::store_password(
                &cfg.config_dir.join("state/secrets.toml"),
                &cfg.auth.password,
            )
            .map_err(|e| e.to_string())?;
        }
        cfg.save().map_err(|e| e.to_string())?;
        app.logger.set_level(cfg.log_level);
        let auto_trust = cfg.auto_accept_host_keys;
        *app.config.write().unwrap() = cfg;
        app.switches.set_auto_trust(auto_trust);
        if endpoint_changed {
            let mut state = self.state.lock().unwrap();
            state.network.revision += 1;
            state.queue_reviews.clear();
        }
        restart.sort_by_key(|id| *id as usize);
        restart.dedup();
        for id in restart {
            if app.services.status(id).is_running() {
                if app.config.read().unwrap().service(id).enabled {
                    app.services.restart(id)
                } else {
                    app.services.stop(id)
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> (tempfile::TempDir, SharedApp) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("abc.txt"), b"abc").unwrap();
        let mut cfg = config::Config {
            root,
            config_dir: dir.path().join("cfg"),
            sound: false,
            ..Default::default()
        };
        cfg.apply_cli(&crate::StartupOptions::default(), false)
            .unwrap();
        cfg.advertise = Some("127.0.0.1".into());
        (dir, App::new(cfg, tokio::runtime::Handle::current(), false))
    }
    fn device(app: &SharedApp) -> (Arc<Switch>, tokio::sync::mpsc::UnboundedReceiver<Job>) {
        let sw = Switch::for_test(
            "127.0.0.1",
            SwitchState::Ready,
            switch::Facts {
                flash_device: "flash:".into(),
                ..Default::default()
            },
            vec![],
        );
        let jobs = sw.test_job_receiver();
        app.switches.add_for_test(sw.clone());
        (sw, jobs)
    }
    async fn finished(e: &Engine, id: u64) -> OperationState {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(o) = e
                    .snapshot()
                    .operations
                    .into_iter()
                    .find(|o| o.id == id && !o.state.is_active())
                {
                    break o.state;
                }
                tokio::time::sleep(Duration::from_millis(10)).await
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn files_and_hashes_work_without_frontend_ticks() {
        let (_dir, app) = app();
        let e = app.engine();
        let hash = e.submit(Command::Hash("abc.txt".into())).unwrap();
        let list = e.submit(Command::ListLocal("".into())).unwrap();
        assert_eq!(finished(&e, hash).await, OperationState::Complete);
        assert_eq!(finished(&e, list).await, OperationState::Complete);
        assert_eq!(
            e.snapshot().hashes["abc.txt"].as_ref().unwrap().md5,
            "900150983cd24fb0d6963f7d28e17f72"
        );
        assert!(e
            .submit(Command::Hash("../cfg/config.toml".into()))
            .is_err());
        assert!(e.snapshot().listings[""]
            .as_ref()
            .unwrap()
            .iter()
            .any(|f| f.name == "abc.txt"));
    }
    #[tokio::test]
    async fn starts_listener_and_queues_copy_without_any_ui() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        {
            let mut cfg = app.config.write().unwrap();
            cfg.http.bind = "127.0.0.1".into();
            cfg.http.port = port;
        }
        let e = app.engine();
        let id = e
            .submit(Command::Transfer {
                device: sw.id,
                local: "abc.txt".into(),
                remote: "flash:abc.txt".into(),
                receive: false,
            })
            .unwrap();
        assert!(e.submit(Command::RemoveInactive(sw.id)).is_err());
        let job = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(job.untracked(),Job::Copy{command,..}if command.contains(&format!(":{port}/abc.txt")))
        );
        sw.complete_job_for_test();
        assert_eq!(finished(&e, id).await, OperationState::Complete);
        app.shutdown();
    }
    fn queue_request(device: u64, remote: &str) -> TransferRequest {
        TransferRequest {
            device,
            local: "abc.txt".into(),
            remote: remote.into(),
            receive: false,
            protocol: Protocol::Http,
            overwrite: false,
            platform_check: false,
        }
    }
    async fn wait_prepared(e: &Engine, id: u64) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if e.state.lock().unwrap().queue.prepared_for_test(id) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn queue_freezes_protocol_serializes_and_cancels_pending_files() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        app.config.write().unwrap().http.port = 0;
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let e = app.engine();
        let first = e
            .submit(Command::QueueTransfers(vec![
                queue_request(sw.id, "flash:first.txt"),
                queue_request(sw.id, "flash:second.txt"),
            ]))
            .unwrap();
        wait_prepared(&e, first).await;
        let second = e
            .snapshot()
            .operations
            .iter()
            .find(|o| o.id != first && o.transfer.is_some())
            .unwrap()
            .id;
        e.submit(Command::ChooseProtocol(sw.id, Protocol::Ftp))
            .unwrap();
        e.submit(Command::CancelOperation(second)).unwrap();
        assert_eq!(finished(&e, second).await, OperationState::Cancelled);
        assert!(e
            .submit(Command::ChangeRoot(app.config.read().unwrap().root.clone()))
            .is_err());
        sw.set_state_for_test(SwitchState::Ready);
        let job = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(job.untracked(), Job::Copy { command, .. } if command.starts_with("copy http://") && command.ends_with("flash:first.txt"))
        );
        assert_eq!(
            sw.protocol(),
            Protocol::Ftp,
            "Executing an old job must preserve the user's newer protocol selection"
        );
        assert!(jobs.try_recv().is_err());
        sw.complete_job_for_test();
        assert_eq!(finished(&e, first).await, OperationState::Complete);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn retries_rebuild_copy_urls_for_transfers_and_upgrade_images() {
        for image in [false, true] {
            let (_dir, app) = app();
            let (sw, mut jobs) = device(&app);
            {
                let mut cfg = app.config.write().unwrap();
                cfg.http.bind = "0.0.0.0".into();
                cfg.http.port = 0;
            }
            let e = app.engine();
            let command = if image {
                e.submit(Command::AssignImage {
                    devices: vec![sw.id],
                    local: "abc.txt".into(),
                })
                .unwrap();
                Command::Deploy(sw.id, Protocol::Http)
            } else {
                Command::Transfer {
                    device: sw.id,
                    local: "abc.txt".into(),
                    remote: "flash:abc.txt".into(),
                    receive: false,
                }
            };
            let id = e.submit(command).unwrap();
            let first = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
                .await
                .unwrap()
                .unwrap();
            let get_command = |job: &Job| match job.clone().untracked() {
                Job::Copy { command, .. } | Job::PrepareUpgrade { command, .. } => command.clone(),
                _ => panic!("expected copy or upgrade"),
            };
            assert!(get_command(&first).contains("127.0.0.1"));
            sw.complete_job_for_test();
            assert_eq!(finished(&e, id).await, OperationState::Complete);
            e.finish(id, Err("simulated lost transfer".into()));
            e.submit(Command::Set(Setting::Advertise(Some("::1".into()))))
                .unwrap();
            let retry = e.submit(Command::RetryOperation(id)).unwrap();
            let next = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
                .await
                .unwrap()
                .unwrap();
            let command = get_command(&next);
            assert!(
                command.contains("[::1]"),
                "retry kept old interface: {command}"
            );
            assert!(!command.contains("127.0.0.1"));
            let operation = e
                .snapshot()
                .operations
                .into_iter()
                .find(|o| o.id == retry)
                .unwrap();
            assert_eq!(
                operation.transfer.unwrap().command.as_deref(),
                Some(command.as_str())
            );
            assert!(app
                .logger
                .entries()
                .iter()
                .any(|entry| entry.command.as_deref() == Some(command.as_str())
                    && entry.proto == Protocol::Http.label()));
            sw.complete_job_for_test();
            finished(&e, retry).await;
            app.shutdown();
        }
    }
    #[tokio::test]
    async fn removal_preserves_other_devices_and_rejects_busy_jobs() {
        let (_dir, app) = app();
        let (first, _jobs) = device(&app);
        let mut second = Switch::for_test(
            "127.0.0.2",
            SwitchState::Ready,
            switch::Facts::default(),
            vec![],
        );
        Arc::get_mut(&mut second).unwrap().id = 2;
        let _jobs2 = second.test_job_receiver();
        app.switches.add_for_test(second.clone());
        let e = app.engine();
        first.set_state_for_test(SwitchState::Busy {
            what: "copy".into(),
        });
        assert!(e.submit(Command::RemoveDevice(first.id)).is_err());
        assert_eq!(app.switches.list().len(), 2);
        first.set_state_for_test(SwitchState::Ready);
        e.submit(Command::RemoveDevice(first.id)).unwrap();
        assert!(e.device(first.id).is_err());
        assert!(e.device(second.id).is_ok());
        app.shutdown();
    }
    #[tokio::test]
    async fn source_change_is_rejected_again_when_retried() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let id = e
            .submit(Command::QueueTransfers(vec![queue_request(
                sw.id,
                "flash:a.txt",
            )]))
            .unwrap();
        wait_prepared(&e, id).await;
        std::fs::write(
            app.config.read().unwrap().root.join("abc.txt"),
            "changed content",
        )
        .unwrap();
        sw.set_state_for_test(SwitchState::Ready);
        assert!(
            matches!(finished(&e, id).await, OperationState::Failed(e) if e.contains("source file changed"))
        );
        let retry = e.submit(Command::RetryOperation(id)).unwrap();
        assert!(
            matches!(finished(&e, retry).await, OperationState::Failed(e) if e.contains("source file changed"))
        );
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn disconnected_queue_requires_explicit_resume_and_keeps_its_destination() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        sw.set_state_for_test(SwitchState::Offline {
            reason: "VPN lost".into(),
        });
        let id = e
            .submit(Command::QueueTransfers(vec![queue_request(
                sw.id,
                "flash:original/a.txt",
            )]))
            .unwrap();
        wait_prepared(&e, id).await;
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(
            e.snapshot()
                .operations
                .iter()
                .find(|o| o.id == id)
                .unwrap()
                .state,
            OperationState::Paused
        );
        sw.set_state_for_test(SwitchState::Ready);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert!(jobs.try_recv().is_err());
        e.submit(Command::ResumeQueue(sw.id)).unwrap();
        let job = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(job.untracked(), Job::Copy { command, .. } if command.ends_with("flash:original/a.txt"))
        );
        sw.complete_job_for_test();
        assert_eq!(finished(&e, id).await, OperationState::Complete);
        app.shutdown();
    }
    #[tokio::test]
    async fn devices_run_in_parallel_but_one_devices_jobs_remain_fifo() {
        let (_dir, app) = app();
        let (a, mut a_jobs) = device(&app);
        let mut b = Switch::for_test(
            "127.0.0.2",
            SwitchState::Ready,
            switch::Facts {
                flash_device: "flash:".into(),
                ..Default::default()
            },
            vec![],
        );
        Arc::get_mut(&mut b).unwrap().id = 2;
        let mut b_jobs = b.test_job_receiver();
        app.switches.add_for_test(b.clone());
        app.config.write().unwrap().http.port = 0;
        let e = app.engine();
        let first = e
            .submit(Command::QueueTransfers(vec![
                queue_request(a.id, "flash:first"),
                queue_request(a.id, "flash:second"),
                queue_request(b.id, "flash:parallel"),
            ]))
            .unwrap();
        let a_first = tokio::time::timeout(Duration::from_secs(3), a_jobs.recv())
            .await
            .unwrap()
            .unwrap();
        let b_first = tokio::time::timeout(Duration::from_secs(3), b_jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(a_first.untracked(), Job::Copy { command, .. } if command.ends_with("flash:first"))
        );
        assert!(
            matches!(b_first.untracked(), Job::Copy { command, .. } if command.ends_with("flash:parallel"))
        );
        assert!(a_jobs.try_recv().is_err());
        a.complete_job_for_test();
        b.complete_job_for_test();
        assert_eq!(finished(&e, first).await, OperationState::Complete);
        let next = tokio::time::timeout(Duration::from_secs(3), a_jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(next.untracked(), Job::Copy { command, .. } if command.ends_with("flash:second"))
        );
        a.complete_job_for_test();
        app.shutdown();
    }
    #[tokio::test]
    async fn bind_failure_never_sends_a_copy_and_releases_device() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        {
            let mut cfg = app.config.write().unwrap();
            cfg.http.bind = "127.0.0.1".into();
            cfg.http.port = occupied.local_addr().unwrap().port();
        }
        let e = app.engine();
        let id = e
            .submit(Command::Transfer {
                device: sw.id,
                local: "abc.txt".into(),
                remote: "flash:abc.txt".into(),
                receive: false,
            })
            .unwrap();
        assert!(matches!(finished(&e, id).await, OperationState::Failed(_)));
        assert!(jobs.try_recv().is_err());
        assert!(!e.busy(sw.id));
        app.shutdown();
    }
    #[tokio::test]
    async fn destructive_confirmation_is_exact_one_use_and_rechecked() {
        for input in ["Y", "yes", " y", "y\n", "", "n"] {
            let (_dir, app) = app();
            let (sw, mut jobs) = device(&app);
            let e = app.engine();
            let request = e
                .submit(Command::RequestDelete {
                    device: sw.id,
                    remote: "flash:folder".into(),
                    recursive: true,
                })
                .unwrap();
            e.submit(Command::Reply {
                request,
                input: input.into(),
            })
            .unwrap();
            assert!(jobs.try_recv().is_err());
            assert!(e
                .submit(Command::Reply {
                    request,
                    input: "y".into()
                })
                .is_err());
        }
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        let request = e
            .submit(Command::RequestDelete {
                device: sw.id,
                remote: "flash:folder".into(),
                recursive: true,
            })
            .unwrap();
        e.submit(Command::Reply {
            request,
            input: "y".into(),
        })
        .unwrap();
        assert!(
            matches!(jobs.try_recv().unwrap().untracked(),Job::Delete{path,recursive:true}if path=="flash:folder")
        );
        assert!(e
            .submit(Command::RequestDelete {
                device: sw.id,
                remote: "flash:*".into(),
                recursive: false
            })
            .is_err());
    }
    #[tokio::test]
    async fn root_change_invalidates_images_and_questions_and_blocks_jobs() {
        let (dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        e.submit(Command::AssignImage {
            devices: vec![sw.id],
            local: "abc.txt".into(),
        })
        .unwrap();
        let request = e
            .submit(Command::RequestDelete {
                device: sw.id,
                remote: "flash:file".into(),
                recursive: false,
            })
            .unwrap();
        let next = dir.path().join("next");
        std::fs::create_dir(&next).unwrap();
        e.submit(Command::ChangeRoot(next.clone())).unwrap();
        assert!(e.assignment(sw.id).is_none());
        assert!(e
            .submit(Command::Reply {
                request,
                input: "y".into()
            })
            .is_err());
        e.submit(Command::RemoveInactive(sw.id)).unwrap();
        assert!(matches!(
            jobs.try_recv().unwrap().untracked(),
            Job::RemoveInactive
        ));
        assert!(e
            .submit(Command::ChangeRoot(dir.path().join("root")))
            .is_err());
    }
    #[tokio::test]
    async fn changed_host_key_question_cannot_be_answered_by_an_old_id() {
        let (_dir, app) = app();
        let (sw, mut answer) = Switch::for_test_host_key("one");
        app.switches.add_for_test(sw.clone());
        let e = app.engine();
        let first = e.pending_confirmations()[0].id;
        sw.set_state_for_test(SwitchState::HostKey {
            fingerprint: "two".into(),
        });
        assert!(e
            .submit(Command::Reply {
                request: first,
                input: "y".into()
            })
            .is_err());
        assert!(answer.try_recv().is_err());
        let second = e.pending_confirmations()[0].id;
        assert_ne!(first, second);
        e.submit(Command::Reply {
            request: second,
            input: "n".into(),
        })
        .unwrap();
        assert!(!answer.try_recv().unwrap());
    }
    #[tokio::test]
    async fn invalid_settings_batch_has_no_partial_effects() {
        let (_dir, app) = app();
        let cfg = app.config.read().unwrap().clone();
        let e = app.engine();
        let mut invalid = cfg.http.clone();
        invalid.port = 0;
        assert!(e
            .submit(Command::SetMany(vec![
                Setting::Sound(true),
                Setting::Service(ServiceId::Http, invalid)
            ]))
            .is_err());
        assert!(!app.config.read().unwrap().sound);
        assert!(!cfg.config_dir.join("config.toml").exists());
    }
    #[tokio::test]
    async fn mixed_bulk_is_validated_before_any_connection() {
        let (_dir, app) = app();
        let result = app.engine().submit(Command::BulkConnect {
            targets: "127.0.0.1, invalid/20".into(),
            port: 22,
            credentials: Credentials {
                username: "user".into(),
                password: "secret".into(),
                enable_password: "".into(),
            },
        });
        assert!(result.is_err());
        assert!(app.switches.list().is_empty());
    }
    #[tokio::test]
    async fn workflow_preflight_blocks_missing_source_before_queueing() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        let spec = WorkflowSpec {
            protocol: Protocol::Http,
            targets: vec![WorkflowTarget {
                device: sw.id,
                local: "missing.bin".into(),
                remote: "flash:missing.bin".into(),
            }],
            ..Default::default()
        };
        assert!(e.preflight(&spec)[0].blocker.is_some());
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec,
            })
            .unwrap();
        assert!(e.submit(Command::StartWorkflow(flow)).is_err());
        assert!(e.snapshot().operations.is_empty());
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn workflow_starts_once_and_profiles_and_reports_preserve_exact_mapping() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        let spec = WorkflowSpec {
            protocol: Protocol::Http,
            targets: vec![WorkflowTarget {
                device: sw.id,
                local: "abc.txt".into(),
                remote: "flash:backups/saved.txt".into(),
            }],
            ..Default::default()
        };
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: spec.clone(),
            })
            .unwrap();
        e.submit(Command::SaveProfile {
            name: "Backup, A".into(),
            workflow: flow,
        })
        .unwrap();
        let content =
            std::fs::read_to_string(app.config.read().unwrap().config_dir.join("profiles.toml"))
                .unwrap();
        assert!(!content.contains("password"));
        assert!(!content.contains("username"));
        let profile = &e.snapshot().profiles[0];
        assert_eq!(profile.targets[0].remote, spec.targets[0].remote);
        e.submit(Command::StartWorkflow(flow)).unwrap();
        assert!(e.submit(Command::StartWorkflow(flow)).is_err());
        let job = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(job.untracked(), Job::Copy { command, .. } if command.ends_with("flash:backups/saved.txt"))
        );
        sw.complete_job_for_test();
        let op = e.snapshot().workflows[0].jobs[0];
        assert_eq!(finished(&e, op).await, OperationState::Complete);
        e.finish_workflows();
        e.submit(Command::ExportWorkflow {
            workflow: flow,
            local: "report.csv".into(),
        })
        .unwrap();
        let report =
            std::fs::read_to_string(app.config.read().unwrap().root.join("report.csv")).unwrap();
        assert!(report.contains("Complete"));
        assert!(e
            .submit(Command::ExportWorkflow {
                workflow: flow,
                local: "report.csv".into()
            })
            .is_err());
        assert!(e
            .submit(Command::ExportWorkflow {
                workflow: flow,
                local: "../escape.csv".into()
            })
            .is_err());
        assert!(e
            .submit(Command::SaveWorkflow {
                previous: Some(flow),
                spec
            })
            .is_err());
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn route_change_pauses_queue_until_review_and_explicit_resume() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        e.stopping.store(true, Ordering::Release);
        app.config.write().unwrap().http.port = 0;
        let interfaces = crate::netif::interfaces();
        e.network_changed(
            interfaces.clone(),
            [(sw.id, Some("127.0.0.1".parse().unwrap()))].into(),
        );
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let id = e
            .submit(Command::QueueTransfers(vec![queue_request(
                sw.id,
                "flash:original.txt",
            )]))
            .unwrap();
        wait_prepared(&e, id).await;
        e.network_changed(
            interfaces,
            [(sw.id, Some("127.0.0.2".parse().unwrap()))].into(),
        );
        sw.set_state_for_test(SwitchState::Ready);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(jobs.try_recv().is_err());
        assert!(e.submit(Command::ResumeReviewed(vec![sw.id])).is_err());
        let review = e.submit(Command::ReviewPending(vec![sw.id])).unwrap();
        assert_eq!(finished(&e, review).await, OperationState::Complete);
        assert!(e.snapshot().queue_reviews[0].ready);
        e.submit(Command::ResumeReviewed(vec![sw.id])).unwrap();
        let job = tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(job.untracked(), Job::Copy { command, .. } if command.contains("127.0.0.1") && command.ends_with("flash:original.txt"))
        );
        sw.complete_job_for_test();
        assert_eq!(finished(&e, id).await, OperationState::Complete);
        app.shutdown();
    }
    #[tokio::test]
    async fn queue_review_rejects_changed_source_and_stale_network_revision() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        e.stopping.store(true, Ordering::Release);
        let interfaces = crate::netif::interfaces();
        e.network_changed(interfaces.clone(), Default::default());
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let id = e
            .submit(Command::QueueTransfers(vec![queue_request(
                sw.id,
                "flash:a.txt",
            )]))
            .unwrap();
        wait_prepared(&e, id).await;
        {
            let mut state = e.state.lock().unwrap();
            state.queue.pause(sw.id);
            state
                .operations
                .iter_mut()
                .find(|o| o.id == id)
                .unwrap()
                .state = OperationState::Paused;
        }
        sw.set_state_for_test(SwitchState::Ready);
        let review = e.submit(Command::ReviewPending(vec![sw.id])).unwrap();
        finished(&e, review).await;
        assert!(e.snapshot().queue_reviews[0].ready);
        e.network_changed(Vec::new(), Default::default());
        assert!(e.submit(Command::ResumeReviewed(vec![sw.id])).is_err());
        std::fs::write(
            app.config.read().unwrap().root.join("abc.txt"),
            "changed content",
        )
        .unwrap();
        let review = e.submit(Command::ReviewPending(vec![sw.id])).unwrap();
        finished(&e, review).await;
        assert!(!e.snapshot().queue_reviews[0].ready);
        assert!(e.submit(Command::ResumeReviewed(vec![sw.id])).is_err());
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn uncertain_retry_requires_inspection_and_partial_overwrite_requires_lowercase_y() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let old = e
            .submit(Command::QueueTransfers(vec![queue_request(
                sw.id,
                "flash:original.txt",
            )]))
            .unwrap();
        wait_prepared(&e, old).await;
        {
            let mut state = e.state.lock().unwrap();
            let op = state.operations.iter_mut().find(|o| o.id == old).unwrap();
            op.state = OperationState::Uncertain("VPN lost".into());
        }
        assert!(e.submit(Command::RetryOperation(old)).is_err());
        {
            let mut state = e.state.lock().unwrap();
            state
                .operations
                .iter_mut()
                .find(|o| o.id == old)
                .unwrap()
                .transfer
                .as_mut()
                .unwrap()
                .inspection = Some(TransferInspection::Partial {
                actual: 1,
                expected: 3,
            });
        }
        assert!(e.submit(Command::RetryOperation(old)).is_err());
        let question = e.submit(Command::RequestOverwriteRetry(old)).unwrap();
        e.submit(Command::Reply {
            request: question,
            input: "Y".into(),
        })
        .unwrap();
        assert_eq!(e.snapshot().operations.len(), 1);
        assert!(e
            .submit(Command::Reply {
                request: question,
                input: "y".into()
            })
            .is_err());
        let question = e.submit(Command::RequestOverwriteRetry(old)).unwrap();
        let retry = e
            .submit(Command::Reply {
                request: question,
                input: "y".into(),
            })
            .unwrap();
        assert!(e.submit(Command::RetryOperation(old)).is_err());
        assert!(e.state.lock().unwrap().queue.overwrite_for_test(retry));
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn initial_login_failure_never_enters_automatic_reconnect() {
        let (_dir, app) = app();
        let (sw, _jobs) = device(&app);
        let e = app.engine();
        sw.set_state_for_test(SwitchState::Failed {
            reason: "SSH login failed".into(),
        });
        for _ in 0..4 {
            e.recovery_tick();
        }
        let recovery = e.snapshot().devices[0].recovery.clone();
        assert_eq!(recovery.phase, RecoveryPhase::LoginRequired);
        assert_eq!(recovery.attempts, 0);
        assert!(recovery.next_attempt.is_none());
        app.shutdown();
    }
    #[test]
    fn failures_distinguish_transport_authentication_algorithms_and_source_changes() {
        assert_eq!(
            Failure::from_message("TCP connection failed before SSH negotiation: No route to host")
                .kind,
            FailureKind::Network
        );
        assert_eq!(
            Failure::from_message("SSH negotiation failed: no matching key exchange").kind,
            FailureKind::Negotiation
        );
        assert_eq!(
            Failure::from_message("SSH authentication limit reached after reboot").kind,
            FailureKind::Authentication
        );
        assert_eq!(
            Failure::from_message("Source file changed; select it again").kind,
            FailureKind::SourceChanged
        );
    }
    #[tokio::test]
    async fn completed_workflow_report_survives_device_removal() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: WorkflowSpec {
                    protocol: Protocol::Http,
                    targets: vec![WorkflowTarget {
                        device: sw.id,
                        local: "abc.txt".into(),
                        remote: "flash:a.txt".into(),
                    }],
                    ..Default::default()
                },
            })
            .unwrap();
        e.submit(Command::StartWorkflow(flow)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.complete_job_for_test();
        let op = e.snapshot().workflows[0].jobs[0];
        finished(&e, op).await;
        e.finish_workflows();
        e.submit(Command::RemoveDevice(sw.id)).unwrap();
        e.submit(Command::ExportWorkflow {
            workflow: flow,
            local: "removed.csv".into(),
        })
        .unwrap();
        let report =
            std::fs::read_to_string(app.config.read().unwrap().root.join("removed.csv")).unwrap();
        assert!(report.contains("127.0.0.1"));
        assert!(report.contains("flash:a.txt"));
        assert!(report.contains("Complete"));
        app.shutdown();
    }
    #[tokio::test]
    async fn workflow_install_rejects_reassigned_images_and_tracks_its_own_confirmation() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        sw.facts_for_test(|f| {
            f.version = Some(cisco::parse_show_version(include_str!(
                "../../../testdata/show_version_c9200l.txt"
            )))
        });
        let image = "cat9k_lite_iosxe.17.15.06.SPA.bin";
        std::fs::write(app.config.read().unwrap().root.join(image), "abc").unwrap();
        let spec = WorkflowSpec {
            goal: WorkflowGoal::Upgrade,
            protocol: Protocol::Http,
            targets: vec![WorkflowTarget {
                device: sw.id,
                local: image.into(),
                remote: format!("flash:{image}"),
            }],
            ..Default::default()
        };
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec,
            })
            .unwrap();
        assert!(e
            .submit(Command::RequestWorkflowInstall {
                workflow: flow,
                device: sw.id,
                yolo: false
            })
            .is_err());
        e.submit(Command::StartWorkflow(flow)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.set_upgrade(upgrade::Progress::Verified {
            remote: format!("flash:{image}"),
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            version: Some("17.15.6".into()),
        });
        sw.complete_job_for_test();
        let op = e.snapshot().workflows[0].jobs[0];
        finished(&e, op).await;
        let question = e
            .submit(Command::RequestWorkflowInstall {
                workflow: flow,
                device: sw.id,
                yolo: false,
            })
            .unwrap();
        let install = e
            .submit(Command::Reply {
                request: question,
                input: "y".into(),
            })
            .unwrap();
        assert!(e.snapshot().workflows[0].jobs.contains(&install));
        assert!(e
            .submit(Command::Reply {
                request: question,
                input: "y".into()
            })
            .is_err());
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.complete_job_for_test();
        finished(&e, install).await;
        e.state
            .lock()
            .unwrap()
            .assignments
            .insert(sw.id, ("another.bin".into(), 3));
        assert!(e
            .submit(Command::RequestWorkflowInstall {
                workflow: flow,
                device: sw.id,
                yolo: false
            })
            .is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn a_saved_workflow_cannot_use_a_different_root() {
        let (dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: WorkflowSpec {
                    protocol: Protocol::Http,
                    targets: vec![WorkflowTarget {
                        device: sw.id,
                        local: "abc.txt".into(),
                        remote: "flash:abc.txt".into(),
                    }],
                    ..Default::default()
                },
            })
            .unwrap();
        let other = dir.path().join("other");
        std::fs::create_dir(&other).unwrap();
        std::fs::write(other.join("abc.txt"), "different content").unwrap();
        e.submit(Command::ChangeRoot(other)).unwrap();
        assert!(
            matches!(e.submit(Command::StartWorkflow(flow)),Err(error) if error.contains("root changed"))
        );
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn verified_destination_inspection_refreshes_the_workflow_report() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: WorkflowSpec {
                    protocol: Protocol::Http,
                    targets: vec![WorkflowTarget {
                        device: sw.id,
                        local: "abc.txt".into(),
                        remote: "flash:abc.txt".into(),
                    }],
                    ..Default::default()
                },
            })
            .unwrap();
        e.submit(Command::StartWorkflow(flow)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.complete_job_for_test();
        let op = e.snapshot().workflows[0].jobs[0];
        finished(&e, op).await;
        {
            let mut state = e.state.lock().unwrap();
            state
                .operations
                .iter_mut()
                .find(|o| o.id == op)
                .unwrap()
                .state = OperationState::Uncertain("VPN lost".into());
            state.workflows[0].finished = None;
            state
                .workflow_reports
                .insert(flow, "Remote status unknown\n".into());
        }
        e.finish_workflows();
        assert!(e.snapshot().workflows[0].finished.is_none());
        let inspect = e.submit(Command::InspectOperation(op)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.inspection_for_test(TransferInspection::Verified);
        sw.complete_job_for_test();
        assert_eq!(finished(&e, inspect).await, OperationState::Complete);
        e.submit(Command::ExportWorkflow {
            workflow: flow,
            local: "recovered.csv".into(),
        })
        .unwrap();
        let report =
            std::fs::read_to_string(app.config.read().unwrap().root.join("recovered.csv")).unwrap();
        assert!(report.contains("Complete"));
        assert!(!report.contains("Remote status unknown"));
        assert!(!report.contains("VPN lost"));
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn successful_retry_finishes_workflow_and_tracks_latest_attempt_in_history() {
        let (_dir, app) = app();
        let (sw, mut jobs) = device(&app);
        let e = app.engine();
        app.config.write().unwrap().http.port = 0;
        let flow = e
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: WorkflowSpec {
                    protocol: Protocol::Http,
                    targets: vec![WorkflowTarget {
                        device: sw.id,
                        local: "abc.txt".into(),
                        remote: "flash:abc.txt".into(),
                    }],
                    ..Default::default()
                },
            })
            .unwrap();
        e.submit(Command::StartWorkflow(flow)).unwrap();
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.complete_job_for_test();
        let original = e.snapshot().workflows[0].jobs[0];
        finished(&e, original).await;
        {
            let mut state = e.state.lock().unwrap();
            let op = state
                .operations
                .iter_mut()
                .find(|o| o.id == original)
                .unwrap();
            op.state = OperationState::Uncertain("VPN lost".into());
            op.transfer.as_mut().unwrap().inspection = Some(TransferInspection::Missing);
            state.workflows[0].finished = None;
        }
        sw.set_state_for_test(SwitchState::Busy { what: "CLI".into() });
        let first_retry = e.submit(Command::RetryOperation(original)).unwrap();
        wait_prepared(&e, first_retry).await;
        e.submit(Command::CancelOperation(first_retry)).unwrap();
        // Even retrying the original history row follows the same attempt lineage.
        let retry = e.submit(Command::RetryOperation(original)).unwrap();
        assert_eq!(e.snapshot().workflows[0].jobs, vec![retry]);
        sw.set_state_for_test(SwitchState::Ready);
        tokio::time::timeout(Duration::from_secs(3), jobs.recv())
            .await
            .unwrap()
            .unwrap();
        sw.complete_job_for_test();
        assert_eq!(finished(&e, retry).await, OperationState::Complete);
        e.finish_workflows();
        assert!(e.snapshot().workflows[0].finished.is_some());
        assert!(matches!(
            e.snapshot()
                .operations
                .iter()
                .find(|o| o.id == original)
                .unwrap()
                .state,
            OperationState::Uncertain(_)
        ));
        e.submit(Command::ExportWorkflow {
            workflow: flow,
            local: "retry.csv".into(),
        })
        .unwrap();
        let report =
            std::fs::read_to_string(app.config.read().unwrap().root.join("retry.csv")).unwrap();
        assert!(report.contains("Complete"));
        assert!(!report.contains("VPN lost"));
        assert!(jobs.try_recv().is_err());
        app.shutdown();
    }
    #[tokio::test]
    async fn device_host_key_policy_defaults_to_auto_and_persists_opt_out_for_bulk_and_single() {
        let (_dir, app) = app();
        let e = app.engine();
        let creds = Credentials {
            username: "cisco".into(),
            password: "test".into(),
            enable_password: String::new(),
        };
        assert!(app.config.read().unwrap().auto_accept_host_keys);
        for bulk in [false, true] {
            assert!(
                e.target("127.0.0.1", 22, creds.clone(), bulk)
                    .unwrap()
                    .auto_trust
            );
        }
        e.submit(Command::Set(Setting::AutoAcceptHostKeys(false)))
            .unwrap();
        for bulk in [false, true] {
            assert!(
                !e.target("127.0.0.1", 22, creds.clone(), bulk)
                    .unwrap()
                    .auto_trust
            );
        }
        let text =
            std::fs::read_to_string(app.config.read().unwrap().config_dir.join("config.toml"))
                .unwrap();
        let config: config::Config = toml::from_str(&text).unwrap();
        assert!(!config.auto_accept_host_keys);
        let old_config: config::Config = toml::from_str("sound = false").unwrap();
        assert!(old_config.auto_accept_host_keys);
        app.shutdown();
    }
}
