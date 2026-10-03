//! Long-lived SSH sessions to Cisco devices.
//!
//! A [`Switch`] is one open session. It is opened once and then kept alive:
//! the first thing it does is read `dir` and `show version`, after that it
//! waits for jobs — today a `copy`, later the install and the reload it has to
//! sit through. A separate ping monitor watches reachability, which is what
//! makes surviving a reload possible at all.
//!
//! Everything this module may type on a device goes through
//! [`crate::deploy::vet`]; the whitelist there is the whole security story.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::ChannelMsg;
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::cisco::{FlashUsage, VersionInfo};
use crate::deploy::{self, PromptAction, Wire};
use crate::logging::{Event, LogLevel, Logger};
use crate::session::{Protocol, SessionInfo, SessionManager};

/// Timeouts. A `copy` of a 500 MB image over TFTP can genuinely take an hour.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const STEP_TIMEOUT: Duration = Duration::from_secs(30);
const COPY_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Transcript lines kept per session.
const TRANSCRIPT_MAX: usize = 2000;
/// How often the ping monitor checks a device.
const PING_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(not(test))]
const REBOOT_GRACE: Duration = Duration::from_secs(5);
#[cfg(test)]
const REBOOT_GRACE: Duration = Duration::from_millis(50);
#[cfg(not(test))]
const RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
#[cfg(test)]
const RECONNECT_INTERVAL: Duration = Duration::from_millis(100);
/// How long an unanswered host key question keeps a session waiting.
const HOST_KEY_TIMEOUT: Duration = Duration::from_secs(120);
static KNOWN_HOSTS_WRITE: Mutex<()> = Mutex::new(());

fn same_host(a: &str, b: &str) -> bool {
    match (a.parse::<std::net::IpAddr>(), b.parse::<std::net::IpAddr>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a
            .trim_end_matches('.')
            .eq_ignore_ascii_case(b.trim_end_matches('.')),
    }
}

/// IPv4 host range; /31 and /32 have no excluded network/broadcast address.
pub fn subnet_hosts(cidr: &str) -> Result<Vec<String>, String> {
    let (address, prefix) = cidr
        .trim()
        .split_once('/')
        .ok_or("enter an IPv4 subnet, e.g. 192.168.10.0/24")?;
    let ip: std::net::Ipv4Addr = address.parse().map_err(|_| "invalid IPv4 subnet address")?;
    let prefix: u32 = prefix.parse().map_err(|_| "invalid subnet prefix")?;
    if !(16..=32).contains(&prefix) {
        return Err("experimental scan supports IPv4 /16 through /32".into());
    }
    let count = 1u64 << (32 - prefix);
    let network = u64::from(u32::from(ip)) & !(count - 1);
    let skip = u64::from(prefix < 31);
    Ok((network + skip..network + count - skip)
        .map(|ip| std::net::Ipv4Addr::from(ip as u32).to_string())
        .collect())
}

#[derive(Clone, Default)]
pub struct ScanProgress {
    pub subnet: String,
    pub total: usize,
    pub checked: usize,
    pub reachable: usize,
    pub last_host: String,
    pub finished: bool,
    pub cancelled: bool,
    pub error: Option<String>,
}

pub struct SubnetScan {
    progress: Mutex<ScanProgress>,
    cancel: AtomicBool,
}

impl SubnetScan {
    pub fn progress(&self) -> ScanProgress {
        self.progress.lock().unwrap().clone()
    }
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Everything needed to open a session.
#[derive(Debug, Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub enable_password: String,
    pub known_hosts: PathBuf,
    /// Automatically accept and store unknown and replacement device keys.
    pub auto_trust: bool,
}

/// Work handed to an open session.
#[derive(Debug, Clone)]
pub enum Job {
    /// Correlate a result with the originating core operation, including facts/list jobs.
    Tracked {
        id: u64,
        job: Box<Job>,
        cancel: Option<Arc<AtomicBool>>,
    },
    /// Re-read `dir` and `show version`.
    Facts,
    List {
        path: String,
    },
    RemoveInactive,
    Delete {
        path: String,
        recursive: bool,
    },
    InspectTransfer {
        local_path: PathBuf,
        remote: String,
        expected_size: u64,
        receive: bool,
        upgrade_image: bool,
    },
    VerifyUpgrade {
        rel_path: String,
        local_path: PathBuf,
        remote: String,
    },
    Cli,
    PrepareUpgrade {
        overwrite: bool,
        rel_path: String,
        local_path: PathBuf,
        remote: String,
        command: String,
        size: u64,
        protocol: Protocol,
    },
    Install {
        yolo: bool,
    },
    /// Let the device pull one file.
    Copy {
        rel_path: String,
        command: String,
        overwrite: bool,
        platform_check: bool,
    },
    /// `exit` and close the session.
    Disconnect,
}

impl Job {
    #[cfg(any(test, feature = "test-support"))]
    pub fn untracked(self) -> Self {
        match self {
            Self::Tracked { job, .. } => *job,
            job => job,
        }
    }
    fn label(&self) -> String {
        match self {
            Job::Tracked { job, .. } => job.label(),
            Job::InspectTransfer { .. } => "inspect destination".into(),
            Job::Facts => "reading device facts".into(),
            Job::List { path } => format!("listing {path}"),
            Job::RemoveInactive => "install remove inactive".into(),
            Job::Delete { path, .. } => format!("deleting {path}"),
            Job::VerifyUpgrade { rel_path, .. } => format!("verifying existing {rel_path}"),
            Job::Cli => "interactive CLI".into(),
            Job::PrepareUpgrade { rel_path, .. } => format!("deploy and verify {rel_path}"),
            Job::Install { .. } => "install activate commit".into(),
            Job::Copy { rel_path, .. } => format!("copying {rel_path}"),
            Job::Disconnect => "disconnecting".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchState {
    Connecting,
    /// Waiting for the user to accept an unknown host key.
    HostKey {
        fingerprint: String,
    },
    ReloadConfirm {
        prompt: String,
    },
    Rebooting,
    CleanupConfirm {
        files: Vec<String>,
        warning: Option<String>,
    },
    /// Logged in, privileged, idle.
    Ready,
    Busy {
        what: String,
    },
    /// The session is gone but the device may come back — this is the state a
    /// reload leaves behind.
    Offline {
        reason: String,
    },
    /// The session never came up.
    Failed {
        reason: String,
    },
    /// Closed on purpose.
    Closed,
}

impl SwitchState {
    pub fn label(&self) -> &'static str {
        match self {
            SwitchState::Connecting => "connecting",
            SwitchState::HostKey { .. } => "host key?",
            SwitchState::CleanupConfirm { .. } => "cleanup?",
            SwitchState::ReloadConfirm { .. } => "reload?",
            SwitchState::Rebooting => "rebooting",
            SwitchState::Ready => "ready",
            SwitchState::Busy { .. } => "busy",
            SwitchState::Offline { .. } => "offline",
            SwitchState::Failed { .. } => "failed",
            SwitchState::Closed => "closed",
        }
    }
    pub fn is_live(&self) -> bool {
        matches!(
            self,
            SwitchState::Ready
                | SwitchState::Busy { .. }
                | SwitchState::CleanupConfirm { .. }
                | SwitchState::ReloadConfirm { .. }
                | SwitchState::Rebooting
        )
    }
    pub fn is_over(&self) -> bool {
        matches!(
            self,
            SwitchState::Closed | SwitchState::Failed { .. } | SwitchState::Offline { .. }
        )
    }
}

/// What the device told us about itself.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub hostname: Option<String>,
    pub version: Option<VersionInfo>,
    pub flash: Option<FlashUsage>,
    /// Device the flash numbers came from, e.g. `flash:`.
    pub flash_device: String,
    pub updated: Option<Instant>,
}

/// Result of the last ping.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    pub online: bool,
    pub rtt: Option<Duration>,
    pub checked: Option<Instant>,
    /// Since when the device has been unreachable — the reload timer.
    pub down_since: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A line transferbuddy typed.
    Sent,
    /// Device output.
    Output,
    /// transferbuddy's own commentary.
    Info,
    Error,
}

#[derive(Debug, Clone)]
pub struct Line {
    pub kind: LineKind,
    pub text: String,
}

/// One explicitly requested copy, correlated with the file server's counters.
#[derive(Clone)]
pub struct Transfer {
    pub rel_path: String,
    pub size: u64,
    pub protocol: Protocol,
    pub direction: crate::session::Direction,
    pub started: Instant,
    pub ended: Option<Instant>,
    pub session: Option<SessionInfo>,
}

/// One open session to one device.
pub struct Switch {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub opened: Instant,
    target: Mutex<Option<Target>>,
    protocol: Mutex<Protocol>,
    transfer: Mutex<Option<Transfer>>,
    peer_ips: Mutex<Vec<std::net::IpAddr>>,
    refreshing: AtomicBool,
    state: Mutex<SwitchState>,
    facts: Mutex<Facts>,
    inspection: Mutex<Option<crate::engine::TransferInspection>>,
    listings:
        Mutex<std::collections::HashMap<String, Result<Vec<crate::cisco::RemoteFile>, String>>>,
    transcript: Mutex<VecDeque<Line>>,
    /// The device's current, not yet newline-terminated output.
    live: Mutex<String>,
    reach: Mutex<Reach>,
    /// Result of the last finished job, for the UI.
    last_result: Mutex<Option<std::result::Result<String, String>>>,
    host_key_reply: Mutex<Option<oneshot::Sender<bool>>>,
    cleanup_reply: Mutex<Option<oneshot::Sender<bool>>>,
    /// Number of finished jobs, so the UI can announce each one once.
    jobs_done: AtomicU64,
    commands_done: AtomicU64,
    command_result: Mutex<Option<Result<String, String>>>,
    tracked_results: Mutex<std::collections::HashMap<u64, Result<String, String>>>,
    tracked_id: AtomicU64,
    copy_id: AtomicU64,
    copying: AtomicBool,
    copy_abort: AtomicBool,
    copy_aborted: AtomicU64,
    job_cancel: Mutex<Option<Arc<AtomicBool>>>,
    output_revision: AtomicU64,
    jobs: Mutex<mpsc::UnboundedSender<Job>>,
    cli_input: Mutex<Option<mpsc::UnboundedSender<Vec<u8>>>>,
    terminal: Mutex<vt100::Parser>,
    cli_size: Mutex<Option<(u16, u16)>>,
    cli_receiver: Mutex<Option<mpsc::UnboundedReceiver<Vec<u8>>>>,
    reload_reply: Mutex<Option<oneshot::Sender<bool>>>,
    upgrade: Mutex<crate::upgrade::Progress>,
    protocol_chosen: AtomicBool,
    discovered: AtomicBool,
    monitor_generation: AtomicU64,
    cancel: Arc<AtomicBool>,
}

impl Switch {
    /// Login secrets remain session-only and can be replaced without losing jobs.
    pub fn update_credentials(
        &self,
        username: String,
        password: String,
        enable_password: String,
    ) -> Result<(), String> {
        if username.trim().is_empty() || password.is_empty() {
            return Err("username and password must not be empty".into());
        }
        if !self.state().is_over() {
            return Err("disconnect before editing credentials".into());
        }
        let mut stored = self.target.lock().unwrap();
        let target = stored.as_mut().ok_or("missing reconnect credentials")?;
        target.username = username;
        target.password = password;
        target.enable_password = enable_password;
        Ok(())
    }
    pub fn connection_username(&self) -> String {
        self.target
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.username.clone())
            .unwrap_or_else(|| self.username.clone())
    }
    pub fn can_reconnect(&self) -> bool {
        self.target.lock().unwrap().is_some() && !self.cancel_requested()
    }

    pub fn upgrade(&self) -> crate::upgrade::Progress {
        self.upgrade.lock().unwrap().clone()
    }
    pub fn set_upgrade(&self, progress: crate::upgrade::Progress) {
        *self.upgrade.lock().unwrap() = progress;
    }
    pub fn protocol_chosen(&self) -> bool {
        self.protocol_chosen.load(Ordering::Relaxed)
    }
    pub fn choose_protocol(&self, protocol: Protocol) {
        self.set_protocol(protocol);
        self.protocol_chosen.store(true, Ordering::Relaxed);
    }
    pub fn answer_reload(&self, accept: bool) {
        if let Some(tx) = self.reload_reply.lock().unwrap().take() {
            let _ = tx.send(accept);
        }
    }
    pub fn start_cli(&self) -> bool {
        self.start_cli_with_id(None)
    }
    pub fn start_cli_tracked(&self, id: u64) -> bool {
        self.start_cli_with_id(Some(id))
    }
    fn start_cli_with_id(&self, id: Option<u64>) -> bool {
        if self.state() != SwitchState::Ready || self.cli_input.lock().unwrap().is_some() {
            return false;
        }
        *self.terminal.lock().unwrap() = vt100::Parser::new(24, 80, 2000);
        let (tx, rx) = mpsc::unbounded_channel();
        *self.cli_input.lock().unwrap() = Some(tx);
        *self.cli_receiver.lock().unwrap() = Some(rx);
        if id.map_or_else(
            || self.submit(Job::Cli),
            |id| self.submit_tracked(id, Job::Cli),
        ) {
            true
        } else {
            self.close_cli();
            false
        }
    }
    pub fn cli_send(&self, bytes: Vec<u8>) {
        if let Some(tx) = self.cli_input.lock().unwrap().as_ref() {
            let _ = tx.send(bytes);
        }
    }
    pub fn terminal_screen(&self) -> Option<vt100::Screen> {
        if self.cli_open() {
            Some(self.terminal.lock().unwrap().screen().clone())
        } else {
            None
        }
    }
    pub fn resize_cli(&self, rows: u16, cols: u16) {
        let size = (rows.clamp(1, 500), cols.clamp(1, 500));
        let mut pending = self.cli_size.lock().unwrap();
        if self.terminal.lock().unwrap().screen().size() != size {
            self.terminal.lock().unwrap().set_size(size.0, size.1);
            *pending = Some(size);
        }
    }
    pub fn cli_open(&self) -> bool {
        self.cli_input.lock().unwrap().is_some()
    }
    pub fn close_cli(&self) {
        self.cli_input.lock().unwrap().take();
    }
    pub fn protocol(&self) -> Protocol {
        *self.protocol.lock().unwrap()
    }
    pub fn set_protocol(&self, protocol: Protocol) {
        *self.protocol.lock().unwrap() = protocol;
    }
    #[cfg(test)]
    pub(crate) fn inspection_for_test(&self, result: crate::engine::TransferInspection) {
        *self.inspection.lock().unwrap() = Some(result);
    }
    pub fn take_inspection(&self) -> Option<crate::engine::TransferInspection> {
        self.inspection.lock().unwrap().take()
    }
    pub fn listing(&self, path: &str) -> Option<Result<Vec<crate::cisco::RemoteFile>, String>> {
        self.listings.lock().unwrap().get(path).cloned()
    }
    pub fn transfer_peer(&self) -> Option<std::net::IpAddr> {
        self.host
            .parse()
            .ok()
            .or_else(|| self.peer_ips.lock().unwrap().first().copied())
    }
    pub fn begin_receive(&self, rel_path: String, size: u64) {
        self.begin_transfer(rel_path, size, Protocol::Ftp);
        self.transfer.lock().unwrap().as_mut().unwrap().direction =
            crate::session::Direction::Upload;
    }
    pub fn begin_transfer(&self, rel_path: String, size: u64, protocol: Protocol) {
        *self.transfer.lock().unwrap() = Some(Transfer {
            rel_path,
            size,
            protocol,
            direction: crate::session::Direction::Download,
            started: Instant::now(),
            ended: None,
            session: None,
        });
    }
    pub fn transfer(&self, sessions: &SessionManager) -> Option<Transfer> {
        let mut guard = self.transfer.lock().unwrap();
        let transfer = guard.as_mut()?;
        let ips = self.peer_ips.lock().unwrap();
        if let Some(session) = sessions
            .snapshot()
            .into_iter()
            .filter(|s| {
                s.protocol == transfer.protocol
                    && s.direction == Some(transfer.direction)
                    && s.file.as_deref().map(|f| f.trim_start_matches('/'))
                        == Some(transfer.rel_path.as_str())
                    && s.transfer_started.unwrap_or(s.started) >= transfer.started
                    && transfer
                        .ended
                        .is_none_or(|end| s.transfer_started.unwrap_or(s.started) <= end)
                    && (ips.contains(&s.peer.ip())
                        || self.host.parse::<std::net::IpAddr>().ok() == Some(s.peer.ip()))
            })
            .max_by_key(|s| s.transfer_started.unwrap_or(s.started))
        {
            let mut session = session;
            if let Some(end) = transfer.ended {
                session.transfer_ended = session.transfer_ended.or(Some(end));
                session.current_speed = 0.0;
            }
            transfer.session = Some(session);
        }
        Some(transfer.clone())
    }
    pub fn state(&self) -> SwitchState {
        self.state.lock().unwrap().clone()
    }
    pub fn facts(&self) -> Facts {
        self.facts.lock().unwrap().clone()
    }
    pub fn facts_refreshing(&self) -> bool {
        self.refreshing.load(Ordering::Acquire)
    }
    pub fn reach(&self) -> Reach {
        self.reach.lock().unwrap().clone()
    }
    pub fn last_result(&self) -> Option<std::result::Result<String, String>> {
        self.last_result.lock().unwrap().clone()
    }
    /// Whether an abort was requested. Used by the UI tests.
    pub fn cancel_requested(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn commands_done(&self) -> u64 {
        self.commands_done.load(Ordering::Acquire)
    }
    pub fn command_result(&self) -> Option<Result<String, String>> {
        self.command_result.lock().unwrap().clone()
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_state_for_test(&self, state: SwitchState) {
        self.set_state(state);
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn complete_job_for_test(&self) {
        let id = self.tracked_id.load(Ordering::Acquire);
        if id != 0 {
            self.tracked_results
                .lock()
                .unwrap()
                .insert(id, Ok(String::new()));
        }
        *self.command_result.lock().unwrap() = Some(Ok(String::new()));
        self.commands_done.fetch_add(1, Ordering::Release);
        self.jobs_done.fetch_add(1, Ordering::Release);
        self.set_state(SwitchState::Ready);
    }
    pub fn output_revision(&self) -> u64 {
        self.output_revision.load(Ordering::Acquire)
    }
    pub fn jobs_done(&self) -> u64 {
        self.jobs_done.load(Ordering::Relaxed)
    }
    pub fn live(&self) -> String {
        self.live.lock().unwrap().clone()
    }
    pub fn transcript(&self) -> Vec<Line> {
        self.transcript.lock().unwrap().iter().cloned().collect()
    }
    /// Name to show in lists: the device's own hostname once it is known.
    pub fn display_name(&self) -> String {
        match self.facts().hostname {
            Some(name) => name,
            None => self.host.clone(),
        }
    }

    /// Queue work. Returns false when the session is no longer accepting any.
    pub fn submit(&self, job: Job) -> bool {
        self.jobs.lock().unwrap().send(job).is_ok()
    }
    pub fn submit_tracked(&self, id: u64, job: Job) -> bool {
        self.tracked_id.store(id, Ordering::Release);
        self.submit(Job::Tracked {
            id,
            job: Box::new(job),
            cancel: None,
        })
    }
    pub fn submit_cancellable(&self, id: u64, job: Job, cancel: Arc<AtomicBool>) -> bool {
        self.tracked_id.store(id, Ordering::Release);
        *self.job_cancel.lock().unwrap() = Some(cancel.clone());
        self.submit(Job::Tracked {
            id,
            job: Box::new(job),
            cancel: Some(cancel),
        })
    }
    fn job_cancelled(&self) -> bool {
        self.job_cancel
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|token| token.load(Ordering::Acquire))
    }
    pub fn take_tracked_result(&self, id: u64) -> Option<Result<String, String>> {
        self.tracked_results.lock().unwrap().remove(&id)
    }
    pub fn tracked_operation(&self) -> u64 {
        self.tracked_id.load(Ordering::Acquire)
    }
    pub fn tracked_started(&self, id: u64) -> bool {
        self.copy_id.load(Ordering::Acquire) == id
    }
    pub fn copy_abort_confirmed(&self, id: u64) -> bool {
        self.copy_aborted.load(Ordering::Acquire) == id
    }
    pub fn request_copy_abort(&self, id: u64) -> bool {
        if self.tracked_operation() != id
            || matches!(self.upgrade(), crate::upgrade::Progress::Verifying)
        {
            return false;
        }
        if let Some(token) = self.job_cancel.lock().unwrap().as_ref() {
            token.store(true, Ordering::Release);
        } else if self.copy_id.load(Ordering::Acquire) != id
            || !self.copying.load(Ordering::Acquire)
        {
            return false;
        }
        self.copy_abort.store(true, Ordering::Release);
        true
    }

    /// Abort whatever is running or being waited for. A `copy` cannot be taken
    /// back on the device side, so this tears the session down instead of
    /// pretending otherwise. Safe to call in any state, including while the
    /// connection is still being set up or a host key question is open.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        // A pending host key question would otherwise keep the driver parked.
        self.answer_host_key(false);
        self.answer_cleanup(false);
        self.answer_reload(false);
        self.close_cli();
    }

    /// Answer the pending host key question.
    pub(crate) fn set_auto_trust(&self, auto_trust: bool) {
        if let Some(target) = self.target.lock().unwrap().as_mut() {
            target.auto_trust = auto_trust;
        }
    }

    pub fn answer_host_key(&self, accept: bool) {
        if let Some(tx) = self.host_key_reply.lock().unwrap().take() {
            let _ = tx.send(accept);
        }
    }

    pub fn answer_cleanup(&self, accept: bool) {
        if let Some(tx) = self.cleanup_reply.lock().unwrap().take() {
            let _ = tx.send(accept);
        }
    }

    /// A detached session with an open host key question, plus the receiver
    /// the answer arrives on.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test_host_key(fingerprint: &str) -> (Arc<Self>, oneshot::Receiver<bool>) {
        let switch = Self::for_test(
            "10.20.30.40",
            SwitchState::HostKey {
                fingerprint: fingerprint.to_string(),
            },
            Facts::default(),
            Vec::new(),
        );
        let (tx, rx) = oneshot::channel();
        *switch.host_key_reply.lock().unwrap() = Some(tx);
        (switch, rx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test_cleanup() -> (Arc<Self>, oneshot::Receiver<bool>) {
        let switch = Self::for_test(
            "10.20.30.40",
            SwitchState::CleanupConfirm {
                files: vec!["/flash/cat9k_lite_iosxe.17.12.06.SPA.bin".into()],
                warning: Some("DANGER: deletion list contains the RUNNING IOS 17.12.06".into()),
            },
            Facts::default(),
            Vec::new(),
        );
        let (tx, rx) = oneshot::channel();
        *switch.cleanup_reply.lock().unwrap() = Some(tx);
        (switch, rx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test_reload() -> (Arc<Self>, oneshot::Receiver<bool>) {
        let switch = Self::for_test(
            "10.20.30.40",
            SwitchState::ReloadConfirm {
                prompt: "Do you want to proceed? [y/n]".into(),
            },
            Facts::default(),
            Vec::new(),
        );
        let (tx, rx) = oneshot::channel();
        *switch.reload_reply.lock().unwrap() = Some(tx);
        (switch, rx)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_job_receiver(&self) -> mpsc::UnboundedReceiver<Job> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.jobs.lock().unwrap() = tx;
        rx
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_cli_receiver(&self) -> mpsc::UnboundedReceiver<Vec<u8>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.cli_input.lock().unwrap() = Some(tx);
        rx
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_cli_output(&self, bytes: &[u8]) {
        self.terminal.lock().unwrap().process(bytes);
        self.output_revision.fetch_add(1, Ordering::Relaxed);
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_listing(&self, path: &str, entries: Vec<crate::cisco::RemoteFile>) {
        self.listings
            .lock()
            .unwrap()
            .insert(path.into(), Ok(entries));
    }

    /// A detached session with a fixed state, for rendering tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        host: &str,
        state: SwitchState,
        facts: Facts,
        transcript: Vec<Line>,
    ) -> Arc<Self> {
        let (tx, _rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            id: 1,
            host: host.to_string(),
            port: 22,
            username: "netadmin".into(),
            opened: Instant::now(),
            target: Mutex::new(None),
            protocol: Mutex::new(Protocol::Http),
            transfer: Mutex::new(None),
            peer_ips: Mutex::new(Vec::new()),
            refreshing: AtomicBool::new(false),
            state: Mutex::new(state),
            inspection: Mutex::new(None),
            listings: Mutex::new(std::collections::HashMap::new()),
            facts: Mutex::new(facts),
            transcript: Mutex::new(transcript.into()),
            live: Mutex::new(String::new()),
            reach: Mutex::new(Reach {
                online: true,
                rtt: Some(Duration::from_micros(1234)),
                checked: Some(Instant::now()),
                down_since: None,
            }),
            last_result: Mutex::new(None),
            host_key_reply: Mutex::new(None),
            cleanup_reply: Mutex::new(None),
            jobs_done: AtomicU64::new(0),
            commands_done: AtomicU64::new(0),
            command_result: Mutex::new(None),
            tracked_results: Mutex::new(std::collections::HashMap::new()),
            tracked_id: AtomicU64::new(0),
            copy_id: AtomicU64::new(0),
            copying: AtomicBool::new(false),
            copy_abort: AtomicBool::new(false),
            copy_aborted: AtomicU64::new(0),
            job_cancel: Mutex::new(None),
            output_revision: AtomicU64::new(0),
            jobs: Mutex::new(tx),
            cli_input: Mutex::new(None),
            terminal: Mutex::new(vt100::Parser::new(24, 80, 2000)),
            cli_size: Mutex::new(None),
            cli_receiver: Mutex::new(None),
            reload_reply: Mutex::new(None),
            upgrade: Mutex::new(crate::upgrade::Progress::Idle),
            protocol_chosen: AtomicBool::new(false),
            discovered: AtomicBool::new(false),
            monitor_generation: AtomicU64::new(0),
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Adjust the facts of a detached test session.
    #[cfg(any(test, feature = "test-support"))]
    pub fn facts_for_test(&self, f: impl FnOnce(&mut Facts)) {
        f(&mut self.facts.lock().unwrap());
    }

    fn set_state(&self, state: SwitchState) {
        *self.state.lock().unwrap() = state;
    }

    fn push(&self, kind: LineKind, text: impl Into<String>) {
        self.output_revision.fetch_add(1, Ordering::Release);
        let mut t = self.transcript.lock().unwrap();
        t.push_back(Line {
            kind,
            text: text.into(),
        });
        while t.len() > TRANSCRIPT_MAX {
            t.pop_front();
        }
    }
}

/// Every open session, plus the runtime to drive them.
#[derive(Clone)]
pub struct SwitchManager {
    switches: Arc<Mutex<Vec<Arc<Switch>>>>,
    runtime: tokio::runtime::Handle,
    logger: Arc<Logger>,
    next_id: Arc<AtomicU64>,
    connect_slots: Arc<Semaphore>,
    scan: Arc<Mutex<Option<Arc<SubnetScan>>>>,
    // 2 leaves explicit Target policies untouched in standalone/test managers.
    auto_trust_policy: Arc<AtomicU64>,
}

impl SwitchManager {
    #[cfg(any(test, feature = "test-support"))]
    pub fn add_for_test(&self, switch: Arc<Switch>) {
        self.switches.lock().unwrap().push(switch);
    }

    pub(crate) fn set_auto_trust(&self, enabled: bool) {
        self.auto_trust_policy
            .store(u64::from(enabled), Ordering::Release);
        for sw in self.list() {
            sw.set_auto_trust(enabled);
        }
    }

    pub fn scan(&self) -> Option<Arc<SubnetScan>> {
        self.scan.lock().unwrap().clone()
    }

    pub fn start_scan_many(
        &self,
        target: Target,
        cidrs: &[String],
        direct: &[String],
    ) -> Result<(), String> {
        self.start_scan_many_with_probe(target, cidrs, direct, Protocol::Http, |host| async move {
            ping_probe(&host)
                .await
                .map(|rtt| rtt.is_some())
                .map_err(|e| e.to_string())
        })
    }

    #[cfg(test)]
    pub fn start_scan(&self, target: Target, cidr: &str, protocol: Protocol) -> Result<(), String> {
        self.start_scan_with_probe(target, cidr, protocol, |host| async move {
            ping_probe(&host)
                .await
                .map(|rtt| rtt.is_some())
                .map_err(|e| e.to_string())
        })
    }

    #[cfg(test)]
    fn start_scan_with_probe<F, Fut>(
        &self,
        target: Target,
        cidr: &str,
        protocol: Protocol,
        probe: F,
    ) -> Result<(), String>
    where
        F: Fn(String) -> Fut + Clone + Send + 'static,
        Fut: std::future::Future<Output = Result<bool, String>> + Send + 'static,
    {
        self.start_scan_many_with_probe(target, &[cidr.to_owned()], &[], protocol, probe)
    }

    fn start_scan_many_with_probe<F, Fut>(
        &self,
        target: Target,
        cidrs: &[String],
        direct: &[String],
        protocol: Protocol,
        probe: F,
    ) -> Result<(), String>
    where
        F: Fn(String) -> Fut + Clone + Send + 'static,
        Fut: std::future::Future<Output = Result<bool, String>> + Send + 'static,
    {
        let mut seen: std::collections::HashSet<String> = direct.iter().cloned().collect();
        let mut hosts = Vec::new();
        for cidr in cidrs {
            for host in subnet_hosts(cidr)? {
                if seen.insert(host.clone()) {
                    hosts.push(host);
                }
            }
        }

        let mut current = self.scan.lock().unwrap();
        if current.as_ref().is_some_and(|s| !s.progress().finished) {
            return Err("a subnet scan is already running (S cancels it)".into());
        }
        let scan = Arc::new(SubnetScan {
            progress: Mutex::new(ScanProgress {
                subnet: cidrs.join(", "),
                total: hosts.len(),
                ..Default::default()
            }),
            cancel: AtomicBool::new(false),
        });
        *current = Some(scan.clone());
        let manager = self.clone();
        self.runtime.spawn(async move {
            sweep_subnet(hosts, scan, probe, move |host| {
                let mut target = target.clone();
                target.host = host;
                let sw = manager.connect(target);
                sw.discovered.store(true, Ordering::Relaxed);
                sw.set_protocol(protocol);
            })
            .await;
        });
        Ok(())
    }

    pub fn new(runtime: tokio::runtime::Handle, logger: Arc<Logger>) -> Self {
        Self {
            switches: Arc::new(Mutex::new(Vec::new())),
            runtime,
            logger,
            next_id: Arc::new(AtomicU64::new(1)),
            connect_slots: Arc::new(Semaphore::new(16)),
            scan: Arc::new(Mutex::new(None)),
            auto_trust_policy: Arc::new(AtomicU64::new(2)),
        }
    }

    pub fn list(&self) -> Vec<Arc<Switch>> {
        self.switches
            .lock()
            .unwrap()
            .iter()
            .filter(|s| {
                !(s.discovered.load(Ordering::Relaxed)
                    && matches!(s.state(), SwitchState::Failed { .. } | SwitchState::Closed))
            })
            .cloned()
            .collect()
    }

    pub fn get(&self, id: u64) -> Option<Arc<Switch>> {
        self.switches
            .lock()
            .unwrap()
            .iter()
            .find(|s| s.id == id)
            .cloned()
    }

    /// An existing, usable session for the same device and user.
    pub fn find_live(&self, host: &str, port: u16, username: &str) -> Option<Arc<Switch>> {
        self.switches
            .lock()
            .unwrap()
            .iter()
            .find(|s| {
                same_host(&s.host, host)
                    && s.port == port
                    && s.connection_username() == username
                    && !s.state().is_over()
            })
            .cloned()
    }

    /// Drop finished sessions from the list.
    pub fn forget_closed(&self) {
        self.switches
            .lock()
            .unwrap()
            .retain(|s| !s.state().is_over());
    }

    pub fn forget(&self, id: u64) {
        self.switches.lock().unwrap().retain(|s| s.id != id);
    }

    pub fn disconnect_all(&self) {
        for sw in self.list() {
            sw.cancel();
            sw.submit(Job::Disconnect);
        }
    }

    /// A busy copy owns its console. Read facts on a second SSH connection.
    pub fn refresh_facts(&self, switch: Arc<Switch>) -> bool {
        if !matches!(
            switch.state(),
            SwitchState::Busy { .. } | SwitchState::CleanupConfirm { .. }
        ) {
            return switch.submit(Job::Facts);
        }
        let Some(target) = switch.target.lock().unwrap().clone() else {
            return false;
        };
        if switch.refreshing.swap(true, Ordering::Relaxed) {
            return false;
        }
        let logger = self.logger.clone();
        self.runtime.spawn(async move {
            let result = async {
                let mut shell = open_shell(&switch, &target, false).await?;
                collect_facts(&switch, &mut shell).await?;
                shell.channel.close().await?;
                Ok::<_, anyhow::Error>(())
            }
            .await;
            if let Err(e) = result {
                logger.log(
                    Event::new(LogLevel::Warning, "switch", "refresh on second SSH session")
                        .error(format!("{e:#}")),
                );
                switch.push(LineKind::Error, format!("refresh failed: {e:#}"));
            }
            switch.refreshing.store(false, Ordering::Relaxed);
        });
        true
    }

    /// Reuse in-memory credentials and keep the same row and upgrade assignment.
    pub fn reconnect(&self, switch: Arc<Switch>) -> bool {
        if !switch.state().is_over() {
            return false;
        }
        let Some(target) = switch.target.lock().unwrap().clone() else {
            return false;
        };
        let (tx, rx) = mpsc::unbounded_channel();
        *switch.jobs.lock().unwrap() = tx;
        switch.cancel.store(false, Ordering::Relaxed);
        switch.set_state(SwitchState::Connecting);
        let slots = self.connect_slots.clone();
        let logger = self.logger.clone();
        let sw = switch.clone();
        self.runtime.spawn(async move {
            let permit = tokio::select! { result = slots.acquire_owned() => result.ok(), _ = cancelled(&sw) => None };
            if let Some(permit) = permit { drive(sw, target, rx, logger, permit).await; }
            else { sw.set_state(SwitchState::Closed); }
        });
        let generation = switch.monitor_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.runtime.spawn(monitor_reachability(switch, generation));
        true
    }

    /// Open a session and start the driver and the ping monitor.
    pub fn connect(&self, mut target: Target) -> Arc<Switch> {
        let mut switches = self.switches.lock().unwrap();
        match self.auto_trust_policy.load(Ordering::Acquire) {
            0 => target.auto_trust = false,
            1 => target.auto_trust = true,
            _ => {}
        }
        if let Some(existing) = switches
            .iter()
            .find(|s| same_host(&s.host, &target.host) && s.port == target.port)
        {
            if !existing.state().is_over() {
                return existing.clone();
            }
        }
        // Reconnecting replaces a finished row, including failed bulk attempts.
        switches.retain(|s| {
            if same_host(&s.host, &target.host) && s.port == target.port {
                s.cancel(); // Stop the old row's offline ping monitor, too.
                false
            } else {
                true
            }
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let switch = Arc::new(Switch {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            host: target.host.clone(),
            port: target.port,
            username: target.username.clone(),
            opened: Instant::now(),
            target: Mutex::new(Some(target.clone())),
            protocol: Mutex::new(Protocol::Http),
            transfer: Mutex::new(None),
            peer_ips: Mutex::new(Vec::new()),
            refreshing: AtomicBool::new(false),
            state: Mutex::new(SwitchState::Connecting),
            inspection: Mutex::new(None),
            listings: Mutex::new(std::collections::HashMap::new()),
            facts: Mutex::new(Facts {
                flash_device: "flash:".into(),
                ..Facts::default()
            }),
            transcript: Mutex::new(VecDeque::new()),
            live: Mutex::new(String::new()),
            reach: Mutex::new(Reach::default()),
            last_result: Mutex::new(None),
            host_key_reply: Mutex::new(None),
            cleanup_reply: Mutex::new(None),
            jobs_done: AtomicU64::new(0),
            commands_done: AtomicU64::new(0),
            command_result: Mutex::new(None),
            tracked_results: Mutex::new(std::collections::HashMap::new()),
            tracked_id: AtomicU64::new(0),
            copy_id: AtomicU64::new(0),
            copying: AtomicBool::new(false),
            copy_abort: AtomicBool::new(false),
            copy_aborted: AtomicU64::new(0),
            job_cancel: Mutex::new(None),
            output_revision: AtomicU64::new(0),
            jobs: Mutex::new(tx),
            cli_input: Mutex::new(None),
            terminal: Mutex::new(vt100::Parser::new(24, 80, 2000)),
            cli_size: Mutex::new(None),
            cli_receiver: Mutex::new(None),
            reload_reply: Mutex::new(None),
            upgrade: Mutex::new(crate::upgrade::Progress::Idle),
            protocol_chosen: AtomicBool::new(false),
            discovered: AtomicBool::new(false),
            monitor_generation: AtomicU64::new(0),
            cancel: Arc::new(AtomicBool::new(false)),
        });
        switches.push(switch.clone());
        drop(switches);

        let logger = self.logger.clone();
        let slots = self.connect_slots.clone();
        let session = switch.clone();
        self.runtime.spawn(async move {
            let permit = tokio::select! {
                permit = slots.acquire_owned() => permit.ok(),
                _ = cancelled(&session) => None,
            };
            if let Some(permit) = permit {
                drive(session, target, rx, logger, permit).await;
            } else {
                session.set_state(SwitchState::Closed);
            }
        });
        let generation = switch.monitor_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.runtime
            .spawn(monitor_reachability(switch.clone(), generation));
        switch
    }
}

/// Keep slow or missing replies off the UI thread and bound OS process usage.
async fn sweep_subnet<F, Fut>(
    hosts: Vec<String>,
    scan: Arc<SubnetScan>,
    probe: F,
    on_reachable: impl Fn(String) + Send,
) where
    F: Fn(String) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = Result<bool, String>> + Send + 'static,
{
    let mut hosts = hosts.into_iter();
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        if scan.cancel.load(Ordering::Relaxed) {
            tasks.abort_all();
            break;
        }
        while tasks.len() < 32 {
            let Some(host) = hosts.next() else { break };
            let probe = probe.clone();
            tasks.spawn(async move {
                let result = probe(host.clone()).await;
                (host, result)
            });
        }
        if tasks.is_empty() {
            break;
        }
        let result = tokio::select! {
            result = tasks.join_next() => result,
            _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
        };
        match result {
            Some(Ok((host, result))) => {
                let mut progress = scan.progress.lock().unwrap();
                progress.checked += 1;
                progress.last_host = host.clone();
                match result {
                    Ok(true) => {
                        progress.reachable += 1;
                        drop(progress);
                        on_reachable(host);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        progress.error = Some(error);
                    }
                }
            }
            Some(Err(e)) => {
                scan.progress.lock().unwrap().error = Some(e.to_string());
            }
            None => break,
        }
    }
    let mut progress = scan.progress.lock().unwrap();
    progress.cancelled = scan.cancel.load(Ordering::Relaxed);
    progress.finished = true;
}

// ---------------------------------------------------------------- driver

async fn drive(
    switch: Arc<Switch>,
    target: Target,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    logger: Arc<Logger>,
    permit: tokio::sync::OwnedSemaphorePermit,
) {
    let log = |level: LogLevel, action: String, detail: Option<String>| {
        let facts = switch.facts();
        let mut ev = Event::new(level, "switch", action).device(
            facts
                .hostname
                .clone()
                .unwrap_or_else(|| target.host.clone()),
            facts.version.as_ref().and_then(|v| v.model.clone()),
        );
        if let Ok(ip) = target.host.parse::<std::net::IpAddr>() {
            ev = ev.ip(ip);
        }
        if let Some(d) = detail {
            ev = ev.result(d);
        }
        logger.log(ev);
    };

    if let Ok(addresses) = tokio::net::lookup_host((target.host.as_str(), target.port)).await {
        *switch.peer_ips.lock().unwrap() = addresses.map(|a| a.ip()).collect();
    }
    switch.push(
        LineKind::Info,
        format!("connecting to {}:{}", target.host, target.port),
    );
    // Connecting must stay abortable: a device that never answers, or a host
    // key question nobody wants to answer, must not park the session forever.
    let opened = tokio::select! {
        result = open_shell(&switch, &target, true) => result,
        _ = cancelled(&switch) => Err(anyhow!("cancelled before the session was up")),
    };
    let mut shell = match opened {
        Ok(shell) => shell,
        Err(e) => {
            let reason = format!("{e:#}");
            switch.push(LineKind::Error, reason.clone());
            switch.set_state(SwitchState::Failed {
                reason: reason.clone(),
            });
            log(
                LogLevel::Error,
                format!("connect to {} failed", target.host),
                Some(reason),
            );
            return;
        }
    };
    switch.set_state(SwitchState::Ready);
    drop(permit);
    log(
        LogLevel::Info,
        format!("connected to {}", switch.display_name()),
        None,
    );

    // Facts first, so a session is useful the moment it appears in the list.
    let _ = switch.submit(Job::Facts);

    loop {
        let job = tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => job,
                None => break,
            },
            _ = cancelled(&switch) => {
                let _ = shell.channel.close().await;
                switch.set_state(SwitchState::Closed);
                return;
            }
            // Nothing to do — but notice right away when the device hangs up
            // (which is what a reload looks like from here).
            alive = shell.idle() => {
                if !alive {
                    switch.push(LineKind::Error, "the device closed the session");
                    switch.set_state(SwitchState::Offline { reason: "session closed by device".into() });
                    log(LogLevel::Warning, format!("{} closed the session", switch.display_name()), None);
                    return;
                }
                continue;
            }
        };

        let (tracked, job, cancel) = match job {
            Job::Tracked { id, job, cancel } => (Some(id), *job, cancel),
            job => (None, job, None),
        };
        if cancel.as_ref().is_some_and(|c| c.load(Ordering::Acquire)) {
            if let Some(id) = tracked {
                switch
                    .tracked_results
                    .lock()
                    .unwrap()
                    .insert(id, Err("cancelled before the command started".into()));
            }
            switch.jobs_done.fetch_add(1, Ordering::Release);
            continue;
        }
        *switch.job_cancel.lock().unwrap() = cancel;
        switch
            .tracked_id
            .store(tracked.unwrap_or(0), Ordering::Release);
        if matches!(job, Job::Disconnect) {
            let _ = shell.send("exit", Wire::Command).await;
            let _ = shell.channel.close().await;
            switch.set_state(SwitchState::Closed);
            log(
                LogLevel::Info,
                format!("disconnected from {}", switch.display_name()),
                None,
            );
            return;
        }

        switch.cancel.store(false, Ordering::Relaxed);
        switch.set_state(SwitchState::Busy { what: job.label() });
        let outcome = run_job(&switch, &mut shell, &job, &logger).await;
        switch.copying.store(false, Ordering::Release);
        switch.job_cancel.lock().unwrap().take();
        if matches!(job, Job::Cli) {
            switch.close_cli();
            switch.cli_receiver.lock().unwrap().take();
        }
        match &outcome {
            Ok(summary) => {
                if !summary.is_empty() {
                    switch.push(LineKind::Info, summary.clone());
                }
                if !matches!(job, Job::Facts | Job::List { .. }) {
                    log(LogLevel::Info, job.label(), Some(summary.clone()));
                }
            }
            Err(e) => {
                let reason = format!("{e:#}");
                if matches!(
                    job,
                    Job::PrepareUpgrade { .. } | Job::VerifyUpgrade { .. } | Job::Install { .. }
                ) {
                    switch.set_upgrade(crate::upgrade::Progress::Failed(reason.clone()));
                }
                switch.push(LineKind::Error, reason.clone());
                log(LogLevel::Error, job.label(), Some(reason));
            }
        }
        *switch.command_result.lock().unwrap() = Some(
            outcome
                .as_ref()
                .map(Clone::clone)
                .map_err(|e| format!("{e:#}")),
        );
        switch.commands_done.fetch_add(1, Ordering::Release);
        if let Some(id) = tracked {
            switch.tracked_results.lock().unwrap().insert(
                id,
                outcome
                    .as_ref()
                    .map(Clone::clone)
                    .map_err(|e| format!("{e:#}")),
            );
        }
        if !matches!(job, Job::Facts | Job::List { .. }) {
            *switch.last_result.lock().unwrap() = Some(
                outcome
                    .as_ref()
                    .map(|s| s.clone())
                    .map_err(|e| format!("{e:#}")),
            );
            switch.jobs_done.fetch_add(1, Ordering::Relaxed);
        }

        // A broken session cannot be reused — say so instead of looking idle.
        if let Err(e) = &outcome {
            let text = format!("{e:#}");
            if !text.starts_with("copy cancelled; prompt recovered")
                && (text.contains("closed the session")
                    || text.contains("cancelled")
                    || matches!(
                        job,
                        Job::RemoveInactive
                            | Job::Delete { .. }
                            | Job::Install { .. }
                            | Job::PrepareUpgrade { .. }
                    ))
            {
                let _ = shell.channel.close().await;
                switch.set_state(SwitchState::Offline { reason: text });
                return;
            }
        }
        switch.set_state(SwitchState::Ready);
    }
    switch.set_state(SwitchState::Closed);
}

/// Resolves once the session has been asked to stop.
async fn cancelled(switch: &Arc<Switch>) {
    while !switch.cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn run_job(
    switch: &Arc<Switch>,
    shell: &mut Shell,
    job: &Job,
    logger: &Logger,
) -> Result<String> {
    match job {
        Job::Tracked { .. } => bail!("nested tracked job"),
        Job::InspectTransfer {
            local_path,
            remote,
            expected_size,
            receive,
            upgrade_image,
        } => {
            use crate::engine::TransferInspection;
            deploy::check_storage_path(remote).map_err(|e| anyhow!(e))?;
            let parent = remote
                .rsplit_once('/')
                .map(|(p, _)| p.to_owned())
                .unwrap_or_else(|| format!("{}:", remote.split_once(':').unwrap().0));
            let name = remote.rsplit(['/', ':']).next().unwrap_or(remote);
            let listing = shell
                .run_command(&format!("dir {parent}"), STEP_TIMEOUT)
                .await?;
            let files = crate::cisco::parse_dir_entries(&listing).map_err(|e| anyhow!(e))?;
            let inspection = match files.iter().find(|f| f.name == name && !f.is_dir) {
                None => TransferInspection::Missing,
                Some(file) if file.size != *expected_size => TransferInspection::Partial {
                    actual: file.size,
                    expected: *expected_size,
                },
                Some(_) if *receive && !local_path.is_file() => TransferInspection::Missing,
                Some(_) => {
                    let output = shell
                        .run_command(&format!("verify /md5 {remote}"), COPY_TIMEOUT)
                        .await?;
                    let remote_md5 = crate::upgrade::remote_md5(&output)
                        .ok_or_else(|| anyhow!("Device did not return a usable MD5"))?;
                    let path = local_path.clone();
                    let local_md5 =
                        tokio::task::spawn_blocking(move || crate::upgrade::local_md5(&path))
                            .await??;
                    if local_md5.eq_ignore_ascii_case(&remote_md5) {
                        if *upgrade_image {
                            switch.set_upgrade(crate::upgrade::Progress::Verified {
                                remote: remote.clone(),
                                md5: local_md5.clone(),
                                version: local_path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .and_then(crate::upgrade::image_version),
                            });
                        }
                        TransferInspection::Verified
                    } else {
                        TransferInspection::Partial {
                            actual: *expected_size,
                            expected: *expected_size,
                        }
                    }
                }
            };
            let label = inspection.label();
            *switch.inspection.lock().unwrap() = Some(inspection);
            Ok(label)
        }
        Job::Facts => {
            collect_facts(switch, shell).await?;
            Ok(String::new())
        }
        Job::List { path } => {
            let result = match shell
                .run_command(&format!("dir {path}"), STEP_TIMEOUT)
                .await
            {
                Ok(output) => crate::cisco::parse_dir_entries(&output),
                Err(e) => Err(format!("{e:#}")),
            };
            switch
                .listings
                .lock()
                .unwrap()
                .insert(path.clone(), result.clone());
            result.map_err(|e| anyhow!(e))?;
            Ok(String::new())
        }
        Job::Cli => {
            let mut input = switch
                .cli_receiver
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow!("CLI was closed"))?;
            let target = switch
                .target
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow!("missing SSH credentials"))?;
            let mut cli = tokio::select! {
                result = open_shell(switch, &target, true) => result?,
                _ = async { while switch.cli_open() { tokio::time::sleep(Duration::from_millis(50)).await; } } => return Ok("CLI closed".into()),
            };
            cli.raw_cli = true;
            switch.terminal.lock().unwrap().process(cli.tail.as_bytes());
            let result = loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        let size=switch.cli_size.lock().unwrap().take();
                        if let Some((rows,cols))=size { cli.channel.window_change(cols as u32, rows as u32,0,0).await?; }
                    },
                    bytes = input.recv() => match bytes { Some(bytes) => cli.channel.data(bytes.as_slice()).await?, None => break Ok("CLI closed".into()) },
                    alive = cli.idle() => if !alive { break Ok("CLI session ended".into()); },
                    _ = cancelled(switch) => break Err(anyhow!("cancelled")),
                }
                cli.take_output();
            };
            switch.close_cli();
            let _ = cli.channel.close().await;
            result
        }
        Job::Delete { path, recursive } => run_delete(switch, shell, path, *recursive).await,
        Job::VerifyUpgrade {
            rel_path,
            local_path,
            remote,
        } => {
            if switch.job_cancelled() {
                bail!("copy cancelled; prompt recovered — upload preparation cancelled");
            }
            verify_upgrade(switch, shell, rel_path, local_path, remote).await?;
            collect_facts(switch, shell).await?;
            Ok("existing image: local and remote MD5 match".into())
        }
        Job::PrepareUpgrade {
            overwrite,
            rel_path,
            local_path,
            remote,
            command,
            size,
            protocol,
        } => {
            if let Some(warning) = switch
                .facts()
                .version
                .as_ref()
                .and_then(|v| crate::cisco::platform_warning(v, rel_path))
            {
                switch.push(LineKind::Info, warning);
            }
            // A previous run or a manual copy may already have put this image on flash.
            let listing = shell
                .run_command(&format!("dir {remote}"), STEP_TIMEOUT)
                .await?;
            let name = remote
                .split_once(':')
                .map(|(_, p)| p.rsplit('/').next().unwrap_or(p))
                .unwrap_or("");
            let exists = crate::cisco::parse_dir_entries(&listing)
                .is_ok_and(|entries| entries.iter().any(|e| !e.is_dir && e.name == name));
            if !exists || *overwrite {
                switch.set_upgrade(crate::upgrade::Progress::Copying);
                switch.begin_transfer(rel_path.clone(), *size, *protocol);
                let result = run_copy(shell, command, *overwrite).await;
                if let Some(transfer) = switch.transfer.lock().unwrap().as_mut() {
                    transfer.ended = Some(Instant::now());
                }
                result?;
            } else {
                switch.push(
                    LineKind::Info,
                    "image already on device — verifying without recopying",
                );
            }
            verify_upgrade(switch, shell, rel_path, local_path, remote).await?;
            let _ = collect_facts(switch, shell).await;
            Ok("image available; local and remote MD5 match".into())
        }
        Job::Install { yolo } => run_install(switch, shell, *yolo, logger).await,
        Job::RemoveInactive => {
            collect_facts(switch, shell).await?;
            let summary = run_remove_inactive(switch, shell).await?;
            collect_facts(switch, shell).await?;
            Ok(summary)
        }
        Job::Copy {
            command,
            overwrite,
            platform_check,
            rel_path,
        } => {
            if let Some(warning) = switch
                .facts()
                .version
                .as_ref()
                .filter(|_| *platform_check)
                .and_then(|v| crate::cisco::platform_warning(v, rel_path))
            {
                switch.push(LineKind::Info, warning);
            }
            let result = run_copy(shell, command, *overwrite).await;
            if let Some(transfer) = switch.transfer.lock().unwrap().as_mut() {
                transfer.ended = Some(Instant::now());
            }
            let summary = result?;
            // The copy just changed how much room is left.
            if let Some(device) = copy_destination_device(command) {
                switch.facts.lock().unwrap().flash_device = device;
            }
            let _ = collect_facts(switch, shell).await;
            Ok(summary)
        }
        Job::Disconnect => Ok(String::new()),
    }
}

async fn verify_upgrade(
    switch: &Arc<Switch>,
    shell: &mut Shell,
    rel_path: &str,
    local_path: &std::path::Path,
    remote: &str,
) -> Result<()> {
    switch.set_upgrade(crate::upgrade::Progress::Verifying);
    let path = local_path.to_path_buf();
    let local = tokio::task::spawn_blocking(move || crate::upgrade::local_md5(&path)).await??;
    let output = shell
        .run_command(&format!("verify /md5 {remote}"), COPY_TIMEOUT)
        .await?;
    let hash = crate::upgrade::remote_md5(&output)
        .ok_or_else(|| anyhow!("image missing or device did not report an MD5 checksum"))?;
    if hash != local {
        bail!("MD5 mismatch: local {local}, device {hash}; installation disabled");
    }
    switch.set_upgrade(crate::upgrade::Progress::Verified {
        remote: remote.into(),
        md5: local,
        version: crate::upgrade::image_version(rel_path),
    });
    Ok(())
}

// The UI has already required plain y for this exact path before enqueueing the job.
async fn run_delete(
    switch: &Arc<Switch>,
    shell: &mut Shell,
    path: &str,
    recursive: bool,
) -> Result<String> {
    deploy::check_delete_path(path).map_err(|e| anyhow!(e))?;
    shell.take_output();
    shell
        .send(
            &format!(
                "delete /force {}{path}",
                if recursive { "/recursive " } else { "" }
            ),
            Wire::Command,
        )
        .await?;
    loop {
        match shell.expect(STEP_TIMEOUT).await? {
            Signal::PromptEnabled(_) => break,
            Signal::Question(prompt) => {
                let name = path.rsplit(['/', ':']).next().unwrap_or("");
                let filename_prompt = prompt
                    .strip_prefix("Delete filename [")
                    .and_then(|p| p.split_once(']'))
                    .is_some_and(|(p, _)| p == name || p == path);
                let confirm_prompt = prompt
                    .strip_prefix("Delete ")
                    .and_then(|p| p.strip_suffix("[confirm]"))
                    .map(|p| {
                        p.trim()
                            .trim_end_matches('?')
                            .trim()
                            .trim_matches(['\u{27}', '"'])
                    })
                    .is_some_and(|p| p.replace(":/", ":") == path.replace(":/", ":"));
                if filename_prompt || confirm_prompt {
                    shell.send("", Wire::DeleteAnswer).await?;
                } else {
                    bail!("unexpected delete prompt: {prompt}; session closed without answering");
                }
            }
            _ => bail!("device closed the session during deletion"),
        }
    }
    let output = shell.take_output();
    if let Some(error) = output
        .lines()
        .find(|l| l.trim_start().starts_with('%') || l.to_ascii_lowercase().contains("error"))
    {
        bail!("delete failed: {error}");
    }
    let parent = path
        .rsplit_once('/')
        .map(|(parent, _)| parent.to_string())
        .unwrap_or_else(|| format!("{}:", path.split_once(':').unwrap().0));
    let listing = shell
        .run_command(&format!("dir {parent}"), STEP_TIMEOUT)
        .await?;
    let entries = crate::cisco::parse_dir_entries(&listing);
    if entries.as_ref().is_ok_and(|entries| {
        entries
            .iter()
            .any(|entry| entry.name == path.rsplit(['/', ':']).next().unwrap_or(""))
    }) {
        bail!("device still reports {path} after delete");
    }
    switch.listings.lock().unwrap().insert(parent, entries);
    // Any deletion may have removed the verified image or a parent directory.
    switch.set_upgrade(crate::upgrade::Progress::Idle);
    collect_facts(switch, shell).await?;
    Ok(format!("deleted {path}"))
}

/// `copy <url> flash:img.bin` -> `flash:`
fn copy_destination_device(command: &str) -> Option<String> {
    let dst = command.split_whitespace().nth(2)?;
    deploy::check_destination(dst).ok()?;
    let (device, _) = dst.split_once(':')?;
    Some(format!("{device}:"))
}

/// Read `dir <device>:` and `show version` into the session's facts.
async fn collect_facts(switch: &Arc<Switch>, shell: &mut Shell) -> Result<()> {
    let device = switch.facts().flash_device;
    let listing = shell
        .run_command(&format!("dir {device}"), STEP_TIMEOUT)
        .await?;
    let usage = crate::cisco::parse_dir_totals(&listing);
    switch
        .listings
        .lock()
        .unwrap()
        .insert(device.clone(), crate::cisco::parse_dir_entries(&listing));

    let version_output = shell.run_command("show version", STEP_TIMEOUT).await?;
    let version = crate::cisco::parse_show_version(&version_output);

    let mut facts = switch.facts.lock().unwrap();
    facts.flash = usage;
    facts.version = Some(version);
    if facts.hostname.is_none() {
        facts.hostname = shell.hostname.clone();
    }
    facts.updated = Some(Instant::now());
    drop(facts);

    if let Some(usage) = usage {
        switch.push(
            LineKind::Info,
            format!(
                "{device} {} free of {} ({:.0}% used)",
                fmt_mb(usage.free),
                fmt_mb(usage.total),
                usage.used_fraction() * 100.0
            ),
        );
    }
    Ok(())
}

/// Flash sizes are always shown in MB — a switch has one flash and comparing
/// it against an image size is the only thing the number is for.
pub fn fmt_mb(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

// ---------------------------------------------------------------- ping

/// Ping the device every few seconds. This is how a reload is noticed and,
/// later, how the device is caught the moment it comes back.
async fn monitor_reachability(switch: Arc<Switch>, generation: u64) {
    loop {
        if switch.monitor_generation.load(Ordering::Relaxed) != generation {
            return;
        }
        let state = switch.state();
        if state.is_over()
            && (!matches!(state, SwitchState::Offline { .. }) || switch.cancel_requested())
        {
            return;
        }
        let rtt = ping_once(&switch.host).await;
        if switch.monitor_generation.load(Ordering::Relaxed) != generation {
            return;
        }
        {
            let mut reach = switch.reach.lock().unwrap();
            let was_online = reach.online;
            reach.online = rtt.is_some();
            reach.rtt = rtt;
            reach.checked = Some(Instant::now());
            match (was_online, reach.online) {
                (true, false) => reach.down_since = Some(Instant::now()),
                (false, true) => reach.down_since = None,
                _ => {}
            }
        }
        tokio::time::sleep(PING_INTERVAL).await;
    }
}

/// One ICMP echo through the system `ping`, so no raw socket (and no root) is
/// needed. `None` means no reply.
async fn ping_once(host: &str) -> Option<Duration> {
    ping_probe(host).await.ok().flatten()
}

async fn ping_probe(host: &str) -> Result<Option<Duration>> {
    let mut command = tokio::process::Command::new("ping");
    command.kill_on_drop(true).args(["-n", "-c", "1"]);
    #[cfg(target_os = "macos")]
    command.args(["-W", "1000"]);
    #[cfg(not(target_os = "macos"))]
    command.args(["-W", "1"]);
    let output =
        match tokio::time::timeout(Duration::from_secs(2), command.arg(host).output()).await {
            Ok(output) => output.map_err(|e| anyhow!("cannot run ping: {e}"))?,
            Err(_) => return Ok(None),
        };
    if !output.status.success() {
        return Ok(None);
    }
    Ok(parse_ping_rtt(&String::from_utf8_lossy(&output.stdout)))
}

/// `64 bytes from 10.0.0.1: icmp_seq=0 ttl=254 time=1.234 ms`
fn parse_ping_rtt(output: &str) -> Option<Duration> {
    let (_, rest) = output.split_once("time=")?;
    let ms: f64 = rest
        .split_whitespace()
        .next()?
        .trim_end_matches("ms")
        .parse()
        .ok()?;
    Some(Duration::from_secs_f64(ms / 1000.0))
}

// ---------------------------------------------------------------- ssh

/// Cisco's SSH implementations are conservative: the modern defaults of russh
/// alone do not reach an IOS device, so the legacy algorithms are offered as
/// well — last, so a modern device still negotiates modern ones.
fn client_config() -> client::Config {
    use russh::{cipher, kex, mac};
    use russh_keys::key;

    let mut cfg = client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(Duration::from_secs(30)),
        ..Default::default()
    };
    cfg.preferred.kex = std::borrow::Cow::Borrowed(&[
        kex::CURVE25519,
        kex::CURVE25519_PRE_RFC_8731,
        kex::ECDH_SHA2_NISTP256,
        kex::ECDH_SHA2_NISTP384,
        kex::ECDH_SHA2_NISTP521,
        kex::DH_G16_SHA512,
        kex::DH_G14_SHA256,
        kex::DH_G14_SHA1,
        kex::EXTENSION_SUPPORT_AS_CLIENT,
        kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT,
    ]);
    cfg.preferred.key = std::borrow::Cow::Borrowed(&[
        key::ED25519,
        key::ECDSA_SHA2_NISTP256,
        key::ECDSA_SHA2_NISTP521,
        key::RSA_SHA2_256,
        key::RSA_SHA2_512,
        key::SSH_RSA,
    ]);
    cfg.preferred.cipher = std::borrow::Cow::Borrowed(&[
        cipher::CHACHA20_POLY1305,
        cipher::AES_256_GCM,
        cipher::AES_256_CTR,
        cipher::AES_192_CTR,
        cipher::AES_128_CTR,
        cipher::AES_256_CBC,
        cipher::AES_192_CBC,
        cipher::AES_128_CBC,
        cipher::TRIPLE_DES_CBC,
    ]);
    cfg.preferred.mac = std::borrow::Cow::Borrowed(&[
        mac::HMAC_SHA512_ETM,
        mac::HMAC_SHA256_ETM,
        mac::HMAC_SHA512,
        mac::HMAC_SHA256,
        mac::HMAC_SHA1_ETM,
        mac::HMAC_SHA1,
    ]);
    cfg
}

struct TransportProbe;
#[async_trait::async_trait]
impl client::Handler for TransportProbe {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, _: &russh_keys::key::PublicKey) -> Result<bool> {
        // Connectivity diagnosis only. Never authenticate or persist this key.
        Ok(true)
    }
}

pub async fn test_ssh_transport(host: &str, port: u16) -> Result<()> {
    let session = tokio::time::timeout(
        Duration::from_secs(10),
        client::connect(Arc::new(client_config()), (host, port), TransportProbe),
    )
    .await
    .map_err(|_| anyhow!("SSH transport test timed out for {host}:{port}"))?
    .map_err(|e| describe_connect_error(e, host, port))?;
    session
        .disconnect(
            russh::Disconnect::ByApplication,
            "Transport test complete; no authentication attempted",
            "",
        )
        .await?;
    Ok(())
}

/// Keep negotiation and TCP failures distinct from authentication failures.
fn describe_connect_error(error: anyhow::Error, host: &str, port: u16) -> anyhow::Error {
    if let Some(russh::Error::NoCommonAlgo { kind, ours, theirs }) =
        error.downcast_ref::<russh::Error>()
    {
        let category = match kind {
            russh::AlgorithmKind::Kex => "key exchange",
            russh::AlgorithmKind::Key => "host key",
            russh::AlgorithmKind::Cipher => "cipher",
            russh::AlgorithmKind::Mac => "MAC",
            russh::AlgorithmKind::Compression => "compression",
        };
        return anyhow!("SSH negotiation failed for {host}:{port}: no matching {category}; device offers [{}]; TransferBuddy offers [{}]", theirs.join(", "), ours.join(", "));
    }
    let reason = format!("{error:#}");
    #[cfg(target_os = "macos")]
    if macos_network_hint(&reason).is_some() {
        return anyhow!("TCP connection to {host}:{port} failed before SSH negotiation: {reason}");
    }
    error.context(format!("SSH connection to {host}:{port} failed"))
}

/// EHOSTUNREACH can also mean a missing route or VPN. Do not claim that
/// macOS privacy denied access without evidence from the operating system.
pub fn macos_network_hint(reason: &str) -> Option<&'static str> {
    #[cfg(target_os = "macos")]
    if reason.contains("os error 65") {
        return Some("Check the network/VPN. If this host works in Terminal, enable TransferBuddy in System Settings → Privacy & Security → Local Network, then reconnect.");
    }
    let _ = reason;
    None
}

/// Trust on first use: an unknown key is shown to the user and, once accepted,
/// recorded in transferbuddy's own `known_hosts`. Single-device adds refuse
/// changed keys; bulk import and discovery explicitly allow replacements.
struct ClientHandler {
    switch: Arc<Switch>,
    known_hosts: PathBuf,
    allow_unknown: bool,
    auto_trust: bool,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = format!("SHA256:{}", server_public_key.fingerprint());
        let auto_trust = self
            .switch
            .target
            .lock()
            .unwrap()
            .as_ref()
            .map(|t| t.auto_trust)
            .unwrap_or(self.auto_trust);
        if auto_trust {
            store_host_key(
                &self.switch.host,
                self.switch.port,
                server_public_key,
                &self.known_hosts,
                true,
            )?;
            self.switch.push(LineKind::Info, format!("host key {fingerprint} automatically trusted and stored (replacement keys allowed)"));
            return Ok(true);
        }
        match russh_keys::check_known_hosts_path(
            &self.switch.host,
            self.switch.port,
            server_public_key,
            &self.known_hosts,
        ) {
            Ok(true) => {
                self.switch
                    .push(LineKind::Info, format!("host key {fingerprint} is known"));
                return Ok(true);
            }
            Ok(false) => {}
            Err(russh_keys::Error::KeyChanged { line }) => {
                bail!(
                    "the host key of {} changed (known_hosts line {}). Refusing to connect — \
                     if the device was replaced or re-imaged, remove that line from {}",
                    self.switch.host,
                    line,
                    self.known_hosts.display()
                );
            }
            Err(e) => bail!("cannot read {}: {e}", self.known_hosts.display()),
        }

        if !self.allow_unknown {
            bail!("refresh requires the already trusted device host key");
        }
        let (tx, rx) = oneshot::channel();
        *self.switch.host_key_reply.lock().unwrap() = Some(tx);
        self.switch.set_state(SwitchState::HostKey {
            fingerprint: fingerprint.clone(),
        });
        let answer = tokio::time::timeout(HOST_KEY_TIMEOUT, rx).await;
        // Take the sender back so a late answer cannot resolve a dead wait.
        self.switch.host_key_reply.lock().unwrap().take();
        match answer {
            Ok(Ok(true)) => {}
            Ok(_) => bail!("host key rejected"),
            Err(_) => bail!(
                "no answer to the host key question within {} minutes",
                HOST_KEY_TIMEOUT.as_secs() / 60
            ),
        }
        // Back to connecting, or the question stays on screen and every
        // further key press reads as another answer to it.
        self.switch.set_state(SwitchState::Connecting);
        store_host_key(
            &self.switch.host,
            self.switch.port,
            server_public_key,
            &self.known_hosts,
            false,
        )
        .map_err(|e| anyhow!("cannot store the host key: {e}"))?;
        self.switch.push(
            LineKind::Info,
            format!("host key {fingerprint} accepted and stored"),
        );
        Ok(true)
    }
}

/// Serialize updates so simultaneous imports cannot overwrite another device's key.
fn store_host_key(
    host: &str,
    port: u16,
    key: &russh_keys::key::PublicKey,
    path: &std::path::Path,
    replace: bool,
) -> Result<()> {
    let _guard = KNOWN_HOSTS_WRITE.lock().unwrap();
    if !replace {
        russh_keys::known_hosts::learn_known_hosts_path(host, port, key, path)?;
        return Ok(());
    }
    let old = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let matching = russh_keys::known_hosts::known_host_keys_path(host, port, path)?;
    let name = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let mut output = String::new();
    for (i, line) in old.lines().enumerate() {
        if matching.iter().any(|(number, _)| *number == i + 1) {
            // Preserve other plain aliases sharing a known_hosts row.
            if let Some((names, rest)) = line.split_once(' ') {
                let aliases: Vec<_> = names
                    .split(',')
                    .filter(|n| *n != name && !n.starts_with('|'))
                    .collect();
                if !aliases.is_empty() {
                    output.push_str(&format!("{} {rest}\n", aliases.join(",")));
                }
            }
        } else {
            output.push_str(line);
            output.push('\n');
        }
    }
    use russh_keys::PublicKeyBase64;
    output.push_str(&format!(
        "{name} {} {}\n",
        key.name(),
        key.public_key_base64()
    ));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, output)?;
    std::fs::rename(temp, path)?;
    Ok(())
}

/// What the device is currently waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    /// Privileged EXEC prompt (`Switch#`).
    PromptEnabled(String),
    /// User EXEC prompt (`Switch>`).
    PromptUser(String),
    /// `Password:`
    Password,
    /// A question, e.g. `Destination filename [x]?`
    Question(String),
    /// The device closed the session.
    Closed,
}

/// Classify the device's trailing, not yet newline-terminated output.
fn tail_signal(tail: &str) -> Option<Signal> {
    let t = tail.trim_end_matches(' ');
    if t.is_empty() {
        return None;
    }
    let lower = t.to_ascii_lowercase();
    // SSH data can split immediately after '?', before the [y/n] suffix.
    if (lower.contains("do you want to remove") || lower.contains("do you want to proceed"))
        && !lower.contains("[y/n]")
        && !lower.contains("[yes/no]")
    {
        return None;
    }
    if lower.ends_with("password:") || lower.ends_with("passphrase:") {
        return Some(Signal::Password);
    }
    if t.ends_with('?')
        || lower.ends_with("[confirm]")
        || lower.ends_with("[y/n]")
        || lower.ends_with("[yes/no]")
    {
        return Some(Signal::Question(t.to_string()));
    }
    // A device prompt is a single token ending in # or >, e.g. "cat9k-1#".
    if (t.ends_with('#') || t.ends_with('>')) && !t.contains(' ') {
        return Some(if t.ends_with('#') {
            Signal::PromptEnabled(t.to_string())
        } else {
            Signal::PromptUser(t.to_string())
        });
    }
    None
}

/// The interactive shell on the device.
struct Shell {
    channel: russh::Channel<client::Msg>,
    switch: Arc<Switch>,
    /// Kept alive so the session is not torn down under the channel.
    _session: client::Handle<ClientHandler>,
    /// Output since the last newline — where prompts live.
    tail: String,
    /// Everything the device printed since the last [`Shell::take_output`].
    collected: String,
    /// The device's own name, learned from its prompt.
    hostname: Option<String>,
    record_output: bool,
    escape: u8,
    raw_cli: bool,
}

// Streaming escape stripping, including sequences split across SSH packets.
fn terminal_text(data: &[u8], escape: &mut u8) -> String {
    let mut output = Vec::with_capacity(data.len());
    for &byte in data {
        match *escape {
            0 if byte == 27 => *escape = 1,
            0 => output.push(byte),
            1 => {
                *escape = match byte {
                    b'[' => 2,
                    b']' => 3,
                    _ => 0,
                }
            }
            2 => {
                if (0x40..=0x7e).contains(&byte) {
                    *escape = 0;
                }
            }
            3 => {
                if byte == 7 {
                    *escape = 0;
                } else if byte == 27 {
                    *escape = 4;
                }
            }
            _ => *escape = if byte == b'\\' || byte == 7 { 0 } else { 3 },
        }
    }
    String::from_utf8_lossy(&output).into_owned()
}

impl Shell {
    /// Write one line to the device, after [`deploy::vet`] approved it.
    async fn send(&mut self, text: &str, wire: Wire) -> Result<()> {
        deploy::vet(text, wire).map_err(|e| anyhow!(e))?;
        let shown = match wire {
            Wire::Secret => "********".to_string(),
            Wire::Answer if text.is_empty() => "<Enter>".to_string(),
            _ => text.to_string(),
        };
        if self.record_output {
            self.switch.push(LineKind::Sent, shown);
        }
        let line = format!("{text}\r");
        self.channel.data(line.as_bytes()).await?;
        self.tail.clear();
        Ok(())
    }

    /// Feed one chunk of device output into the line buffer.
    fn absorb(&mut self, data: &[u8]) {
        self.switch.output_revision.fetch_add(1, Ordering::Release);
        if self.raw_cli {
            self.switch.terminal.lock().unwrap().process(data);
        }
        let text = terminal_text(data, &mut self.escape);
        self.collected.push_str(&text);
        for ch in text.chars() {
            match ch {
                '\n' => {
                    let line = std::mem::take(&mut self.tail);
                    if self.record_output {
                        self.switch
                            .push(LineKind::Output, line.trim_end().to_string());
                    }
                }
                '\r' => {}
                // Backspace: IOS redraws its line this way.
                '\u{8}' => {
                    self.tail.pop();
                }
                c => self.tail.push(c),
            }
        }
        if self.record_output {
            *self.switch.live.lock().unwrap() = self.tail.clone();
        }
    }

    /// Read until the device asks for something, with a deadline.
    async fn expect(&mut self, timeout: Duration) -> Result<Signal> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.switch.cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if Instant::now() >= deadline {
                bail!("the device did not answer within {}s", timeout.as_secs());
            }
            match tokio::time::timeout(Duration::from_millis(250), self.channel.wait()).await {
                // Nothing arrived in this slice — re-check cancel and deadline.
                Err(_) => {}
                Ok(None) => return Ok(Signal::Closed),
                Ok(Some(msg)) => match msg {
                    ChannelMsg::Data { ref data } => self.absorb(data),
                    ChannelMsg::ExtendedData { ref data, .. } => self.absorb(data),
                    ChannelMsg::Eof | ChannelMsg::Close => return Ok(Signal::Closed),
                    _ => {}
                },
            }
            if let Some(sig) = tail_signal(&self.tail) {
                if let Signal::PromptEnabled(p) | Signal::PromptUser(p) = &sig {
                    if self.hostname.is_none() {
                        self.hostname = crate::cisco::hostname_from_prompt(p);
                    }
                }
                return Ok(sig);
            }
        }
    }

    /// Await one message while no job is running. `false` = session gone.
    async fn idle(&mut self) -> bool {
        match self.channel.wait().await {
            None | Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => false,
            Some(ChannelMsg::Data { ref data }) => {
                self.absorb(data);
                true
            }
            Some(_) => true,
        }
    }

    fn take_output(&mut self) -> String {
        std::mem::take(&mut self.collected)
    }

    /// Send a whitelisted command and return everything the device printed
    /// before the next prompt.
    async fn run_command(&mut self, command: &str, timeout: Duration) -> Result<String> {
        self.take_output();
        self.send(command, Wire::Command).await?;
        match self.expect(timeout).await? {
            Signal::PromptEnabled(_) => Ok(self.take_output()),
            Signal::PromptUser(_) => bail!("the device dropped back to user EXEC mode"),
            Signal::Question(q) => bail!("unexpected prompt after {command:?}: {q:?}"),
            Signal::Password => bail!("the device asked for a password after {command:?}"),
            Signal::Closed => bail!("the device closed the session"),
        }
    }
}

/// Connect, authenticate, open a shell and get to a privileged prompt.
#[derive(Default)]
struct LoginBudget {
    used: AtomicU64,
}
impl LoginBudget {
    fn reserve(&self) -> Result<()> {
        if self
            .used
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 3).then_some(n + 1)
            })
            .is_err()
        {
            bail!("SSH authentication limit reached after reboot; edit credentials or reconnect manually");
        }
        Ok(())
    }
}
async fn open_shell(switch: &Arc<Switch>, target: &Target, record_output: bool) -> Result<Shell> {
    open_shell_with_budget(switch, target, record_output, None).await
}
async fn open_shell_with_budget(
    switch: &Arc<Switch>,
    target: &Target,
    record_output: bool,
    budget: Option<&LoginBudget>,
) -> Result<Shell> {
    let handler = ClientHandler {
        switch: switch.clone(),
        known_hosts: target.known_hosts.clone(),
        allow_unknown: record_output,
        auto_trust: target.auto_trust,
    };
    let connection = client::connect(
        Arc::new(client_config()),
        (target.host.as_str(), target.port),
        handler,
    );
    tokio::pin!(connection);
    let mut elapsed = Duration::ZERO;
    let mut session = loop {
        let started = Instant::now();
        let waiting_for_key = matches!(switch.state(), SwitchState::HostKey { .. });
        tokio::select! {
            result = &mut connection => break result.map_err(|e| describe_connect_error(e, &target.host, target.port))?,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if !waiting_for_key && !matches!(switch.state(), SwitchState::HostKey { .. }) { elapsed += started.elapsed(); }
                if elapsed >= CONNECT_TIMEOUT { bail!("no answer from {}:{} within {}s", target.host, target.port, CONNECT_TIMEOUT.as_secs()); }
            }
        }
    };

    match tokio::time::timeout(STEP_TIMEOUT, authenticate(&mut session, target, budget))
        .await
        .map_err(|_| anyhow!("SSH authentication timed out"))?
    {
        Ok(true) => {}
        Ok(false) => bail!(
            "login failed for user {} — wrong username or password?",
            target.username
        ),
        // Devices that drop the connection after a bad password surface as a
        // transport error rather than a clean rejection.
        Err(e) => bail!(
            "login failed for user {} — wrong username or password? ({e})",
            target.username
        ),
    }

    switch.push(
        LineKind::Info,
        format!("authenticating as {}", target.username),
    );
    let channel = session.channel_open_session().await?;
    channel
        .request_pty(true, "vt100", 200, 48, 0, 0, &[])
        .await?;
    channel.request_shell(true).await?;
    switch.push(LineKind::Info, "waiting for the device prompt");

    let mut shell = Shell {
        channel,
        switch: switch.clone(),
        _session: session,
        tail: String::new(),
        collected: String::new(),
        hostname: None,
        record_output,
        escape: 0,
        raw_cli: false,
    };

    let mut signal = shell.expect(STEP_TIMEOUT).await?;
    // Some devices print a banner and ask for a login password on the line
    // before the prompt appears.
    if signal == Signal::Password {
        shell.send(&target.password, Wire::Secret).await?;
        signal = shell.expect(STEP_TIMEOUT).await?;
    }

    if matches!(signal, Signal::PromptUser(_)) {
        shell.send("enable", Wire::Command).await?;
        signal = shell.expect(STEP_TIMEOUT).await?;
        if signal == Signal::Password {
            if target.enable_password.is_empty() {
                bail!("enable needs a password — reconnect with an enable password");
            }
            shell.send(&target.enable_password, Wire::Secret).await?;
            signal = shell.expect(STEP_TIMEOUT).await?;
        }
        if !matches!(signal, Signal::PromptEnabled(_)) {
            bail!("enable failed — wrong enable password?");
        }
    }
    if !matches!(signal, Signal::PromptEnabled(_)) {
        bail!("unexpected device state: {signal:?}");
    }

    // Pagination off, otherwise every long output stops at --More--.
    shell.run_command("terminal length 0", STEP_TIMEOUT).await?;
    Ok(shell)
}

/// Drive one `copy` until the device is back at its prompt.
async fn run_copy(shell: &mut Shell, command: &str, overwrite: bool) -> Result<String> {
    let switch = shell.switch.clone();
    switch
        .copy_id
        .store(switch.tracked_operation(), Ordering::Release);
    if switch.job_cancelled() {
        switch
            .copy_aborted
            .store(switch.tracked_operation(), Ordering::Release);
        bail!("copy cancelled; prompt recovered — no copy command was sent");
    }
    switch.copy_abort.store(false, Ordering::Release);
    switch.copying.store(true, Ordering::Release);
    shell.take_output();
    shell.send(command, Wire::Command).await?;
    let deadline = Instant::now() + COPY_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("copy did not finish within {}s", COPY_TIMEOUT.as_secs());
        }
        let signal = tokio::select! {
            result = shell.expect(remaining) => result?,
            _ = async { while !switch.copy_abort.load(Ordering::Acquire) && !switch.job_cancelled() { tokio::time::sleep(Duration::from_millis(25)).await; } } => {
                shell.tail.clear();
                shell.channel.data(&b"\x03"[..]).await?;
                switch.push(LineKind::Info, "copy abort requested (Ctrl+C)");
                match shell.expect(Duration::from_secs(10)).await {
                    Ok(Signal::PromptEnabled(_)) => {
                        switch.copy_aborted.store(switch.tracked_operation(), Ordering::Release);
                        bail!("copy cancelled; prompt recovered — inspect the remote file before retrying");
                    }
                    _ => bail!("copy cancelled; remote status unknown — reconnect and inspect the destination"),
                }
            }
        };
        switch.copying.store(false, Ordering::Release);
        match signal {
            Signal::Question(q) => match deploy::classify_prompt(&q, overwrite) {
                PromptAction::Accept => {
                    shell.send("", Wire::Answer).await?;
                    switch.copying.store(true, Ordering::Release);
                }
                PromptAction::Decline => {
                    shell.send("n", Wire::Answer).await?;
                    switch.copying.store(true, Ordering::Release);
                }
                PromptAction::Abort(why) => bail!("{why} — session closed without answering"),
            },
            Signal::PromptEnabled(_) => return deploy::verdict(&shell.take_output()),
            Signal::PromptUser(_) => bail!("the device dropped back to user EXEC mode"),
            Signal::Password => bail!("the device asked for another password during the copy"),
            Signal::Closed => bail!("the device closed the session during the copy"),
        }
    }
}

/// Only the deletion section is checked: "File is in use, will not delete"
/// lines in the preceding inventory must never be mistaken for candidates.
fn cleanup_candidates(output: &str) -> Vec<String> {
    let mut deleting = false;
    let mut files = Vec::new();
    for line in output.lines().map(str::trim) {
        let lower = line.to_ascii_lowercase();
        if lower.contains("following files will be deleted") {
            deleting = true;
            continue;
        }
        if !deleting {
            continue;
        }
        if lower.contains("do you want") {
            break;
        }
        if lower.contains("will not delete") || lower.contains("file is in use") {
            continue;
        }
        for token in line.split_whitespace() {
            let token = token.trim_matches(['\r', ',']);
            let is_file = token.contains('/')
                || token.contains(':')
                || [".pkg", ".bin", ".conf"]
                    .iter()
                    .any(|ext| token.ends_with(ext));
            if is_file
                && !token.starts_with('[')
                && !token.ends_with(':')
                && !files.iter().any(|f| f == token)
            {
                files.push(token.to_string());
            }
        }
    }
    files
}

fn release_tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '.')
        .flat_map(|token| {
            let fields: Vec<_> = token.split('.').collect();
            fields
                .windows(3)
                .filter_map(|fields| {
                    let major: u32 = fields[0].parse().ok()?;
                    let minor: u32 = fields[1].parse().ok()?;
                    let digits = fields[2].chars().take_while(char::is_ascii_digit).count();
                    if digits == 0 {
                        return None;
                    }
                    let patch: u32 = fields[2][..digits].parse().ok()?;
                    Some(format!(
                        "{major}.{minor}.{patch}{}",
                        fields[2][digits..].to_ascii_lowercase()
                    ))
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn cleanup_warning(files: &[String], version: &VersionInfo) -> Option<String> {
    let mut running = Vec::new();
    if let Some(v) = &version.version {
        running.extend(release_tokens(v));
    }
    for member in &version.members {
        running.extend(release_tokens(&member.version));
    }
    if running.is_empty() {
        return Some("WARNING: running IOS version could not be verified. Review every file before confirming!".into());
    }
    if files.is_empty() {
        return Some("WARNING: the device's deletion list could not be parsed. Review the transcript before confirming!".into());
    }
    let active_image = version
        .image
        .as_deref()
        .and_then(|p| p.rsplit(['/', ':']).next());
    let risky: Vec<_> = files
        .iter()
        .filter(|file| {
            release_tokens(file).iter().any(|v| running.contains(v))
                || active_image.is_some_and(|image| {
                    !image.is_empty() && file.rsplit(['/', ':']).next() == Some(image)
                })
        })
        .cloned()
        .collect();
    if risky.is_empty() {
        None
    } else {
        Some(format!("DANGER: the switch proposes deleting files for the RUNNING IOS / active image ({}): {}. Confirming may prevent the switch from booting!", version.version.as_deref().unwrap_or("stack versions"), risky.join(", ")))
    }
}

async fn run_remove_inactive(switch: &Arc<Switch>, shell: &mut Shell) -> Result<String> {
    shell.take_output();
    shell.send("install remove inactive", Wire::Command).await?;
    let deadline = Instant::now() + Duration::from_secs(30 * 60);
    let mut declined = false;
    loop {
        match shell
            .expect(deadline.saturating_duration_since(Instant::now()))
            .await?
        {
            Signal::Question(question) => {
                let lower = question.to_ascii_lowercase();
                if !(lower.contains("do you want to remove")
                    && (lower.contains("[y/n]") || lower.contains("[yes/no]")))
                {
                    bail!("unexpected cleanup prompt: {question} — cancelled; session closed without answering");
                }
                let files = cleanup_candidates(&shell.collected);
                let version = switch.facts().version.unwrap_or_default();
                let warning = cleanup_warning(&files, &version);
                if let Some(warning) = &warning {
                    switch.push(LineKind::Error, warning.clone());
                }
                let (tx, rx) = oneshot::channel();
                *switch.cleanup_reply.lock().unwrap() = Some(tx);
                switch.set_state(SwitchState::CleanupConfirm { files, warning });
                let accepted = tokio::select! {
                    answer = rx => answer.unwrap_or(false),
                    _ = cancelled(switch) => false,
                    _ = tokio::time::sleep(deadline.saturating_duration_since(Instant::now())) => false,
                };
                switch.cleanup_reply.lock().unwrap().take();
                switch.set_state(SwitchState::Busy {
                    what: "install remove inactive".into(),
                });
                shell
                    .send(if accepted { "y" } else { "n" }, Wire::CleanupAnswer)
                    .await?;
                // Declining is a normal outcome: drain the remaining output to
                // the EXEC prompt so this connection can still be reused.
                declined |= !accepted;
            }
            Signal::PromptEnabled(_) => {
                let output = shell.take_output();
                if declined {
                    return Ok("cleanup aborted — no deletion confirmed".into());
                }
                if let Some(error) = output.lines().find(|line| {
                    let lower = line.to_ascii_lowercase();
                    lower.contains("failed")
                        || lower.contains("error:")
                        || lower.starts_with("% invalid")
                        || lower.starts_with("%error")
                }) {
                    bail!("cleanup failed: {}", error.trim());
                }
                if output.contains("Nothing to clean") {
                    return Ok("nothing to clean — no inactive files found".into());
                }
                if output.contains("SUCCESS:") {
                    return Ok("inactive files removed; flash usage refreshed".into());
                }
                bail!("cleanup ended without a SUCCESS result");
            }
            Signal::Closed => bail!("the device closed the session during cleanup"),
            other => bail!("unexpected cleanup state: {other:?} — cancelled; session closed"),
        }
    }
}

fn install_boot_ready(output: &str) -> bool {
    let mut boot_found = false;
    let mut manual_disabled = false;
    for line in output.lines() {
        let lower = line.trim().to_ascii_lowercase();
        if lower.starts_with("boot variable") {
            let Some((_, value)) = lower.split_once('=').or_else(|| lower.split_once(':')) else {
                return false;
            };
            let paths: Vec<_> = value
                .split([';', ',', ' '])
                .filter(|part| !part.is_empty())
                .collect();
            if paths.is_empty() || paths.iter().any(|path| !path.ends_with("packages.conf")) {
                return false;
            }
            boot_found = true;
        }
        if lower.starts_with("manual boot") {
            let Some((_, value)) = lower.split_once('=').or_else(|| lower.split_once(':')) else {
                return false;
            };
            if value.trim() != "no" {
                return false;
            }
            manual_disabled = true;
        }
    }
    boot_found && manual_disabled
}

/// Install only the exact image whose device checksum matched the local file.
async fn run_install(
    switch: &Arc<Switch>,
    shell: &mut Shell,
    yolo: bool,
    logger: &Logger,
) -> Result<String> {
    use crate::upgrade::{normalize_version, Progress};
    let Progress::Verified {
        remote,
        md5,
        version: Some(expected),
    } = switch.upgrade()
    else {
        bail!("deploy and verify a full release .bin first");
    };
    collect_facts(switch, shell).await?;
    let info = switch.facts().version.unwrap_or_default();
    if let Some(reason) = crate::upgrade::install_blocker(&switch.upgrade(), &info) {
        bail!("{reason}");
    }

    if info
        .image
        .as_ref()
        .is_none_or(|image| !image.to_ascii_lowercase().contains("packages.conf"))
        || info
            .members
            .iter()
            .any(|m| !m.mode.eq_ignore_ascii_case("INSTALL"))
    {
        bail!("automatic upgrades support existing INSTALL mode only; configure packages.conf boot in Connect CLI first");
    }
    let boot = shell.run_command("show boot", STEP_TIMEOUT).await?;
    if !install_boot_ready(&boot) {
        bail!("Install mode requires a packages.conf boot variable and manual boot disabled; correct boot settings in Connect CLI first");
    }
    // Recheck the device checksum immediately before installing, including after manual CLI use.
    let output = shell
        .run_command(&format!("verify /md5 {remote}"), COPY_TIMEOUT)
        .await?;
    if crate::upgrade::remote_md5(&output).as_deref() != Some(md5.as_str()) {
        bail!("verified image has changed; deploy again before installing");
    }
    switch.set_upgrade(Progress::Installing);
    let saved = shell
        .run_command("write memory", Duration::from_secs(120))
        .await?;
    if saved.lines().any(|l| {
        l.trim().starts_with('%')
            || l.to_ascii_lowercase().contains("error")
            || l.to_ascii_lowercase().contains("failed")
    }) {
        bail!("write memory failed: {saved}");
    }
    if !saved.contains("[OK]") && !saved.to_ascii_lowercase().contains("success") {
        bail!("write memory did not confirm success: {saved}");
    }
    shell.take_output();
    shell
        .send(
            &format!(
                "install add file {remote} activate commit{}",
                if yolo { " prompt-level none" } else { "" }
            ),
            Wire::Command,
        )
        .await?;
    let deadline = Instant::now() + COPY_TIMEOUT;
    let mut reload_accepted = yolo;
    loop {
        match shell
            .expect(deadline.saturating_duration_since(Instant::now()))
            .await?
        {
            Signal::Question(prompt) => {
                let lower = prompt.to_ascii_lowercase();
                if !(lower.contains("reload")
                    || lower.contains("proceed")
                    || lower.contains("activate"))
                {
                    bail!("unexpected install prompt: {prompt}; not confirmed");
                }
                let accepted = if yolo {
                    true
                } else {
                    let (tx, rx) = oneshot::channel();
                    *switch.reload_reply.lock().unwrap() = Some(tx);
                    switch.set_upgrade(Progress::AwaitingReload {
                        prompt: prompt.clone(),
                    });
                    switch.set_state(SwitchState::ReloadConfirm { prompt });
                    let accepted = tokio::select! { reply = rx => reply.unwrap_or(false), _ = cancelled(switch) => false };
                    switch.reload_reply.lock().unwrap().take();
                    switch.set_state(SwitchState::Busy {
                        what: "installing".into(),
                    });
                    accepted
                };
                shell
                    .send(if accepted { "y" } else { "n" }, Wire::UpgradeAnswer)
                    .await?;
                if !accepted {
                    if !matches!(shell.expect(STEP_TIMEOUT).await?, Signal::PromptEnabled(_)) {
                        bail!("device did not return to its prompt after declining reload");
                    }
                    switch.set_upgrade(Progress::Verified {
                        remote,
                        md5,
                        version: Some(expected),
                    });
                    return Ok("reload declined — installation was not activated".into());
                }
                reload_accepted = true;
            }
            Signal::Closed if reload_accepted => break,
            Signal::PromptEnabled(_) => {
                let output = shell.take_output();
                if output.to_ascii_lowercase().contains("failed")
                    || output.lines().any(|l| l.trim().starts_with('%'))
                {
                    bail!("installation failed: {output}");
                }
                if reload_accepted
                    && (output.to_ascii_lowercase().contains("reload")
                        || output.contains("SUCCESS"))
                {
                    break;
                }
                bail!("installation ended without a reload: {output}");
            }
            other => bail!("unexpected installation state: {other:?}"),
        }
    }
    let since = Instant::now();
    let mut attempts = 0;
    switch.set_state(SwitchState::Rebooting);
    switch.set_upgrade(Progress::Rebooting {
        since,
        attempts,
        last_error: None,
    });
    let target = switch
        .target
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| anyhow!("missing reconnect credentials"))?;
    let login_budget = LoginBudget::default();
    let mut login_failures = 0u32;
    let mut next_login = Instant::now();
    // Give the reboot time to start before trusting a still-running SSH service.
    tokio::select! { _ = tokio::time::sleep(REBOOT_GRACE) => {}, _ = cancelled(switch) => bail!("cancelled while rebooting") }
    loop {
        if switch.cancel_requested() {
            bail!("cancelled while rebooting");
        }
        // ICMP may be filtered even when TCP/SSH is available.
        let tcp_ready = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::TcpStream::connect((switch.host.as_str(), switch.port)),
        )
        .await
        .is_ok_and(|r| r.is_ok());
        if tcp_ready && Instant::now() >= next_login {
            attempts += 1;
            switch.set_upgrade(Progress::Rebooting {
                since,
                attempts,
                last_error: None,
            });
            let result = tokio::select! { result = open_shell_with_budget(switch, &target, true, Some(&login_budget)) => result, _ = cancelled(switch) => bail!("cancelled while reconnecting") };
            match result {
                Ok(mut reopened) => match collect_facts(switch, &mut reopened).await {
                    Ok(()) => {
                        let info = switch.facts().version.unwrap_or_default();
                        let success = info.image.as_ref().is_some_and(|image| {
                            image.to_ascii_lowercase().contains("packages.conf")
                        }) && info
                            .version
                            .as_ref()
                            .is_some_and(|v| normalize_version(v) == expected)
                            && info.members.iter().all(|m| {
                                normalize_version(&m.version) == expected
                                    && m.mode.eq_ignore_ascii_case("INSTALL")
                            });
                        if !success && since.elapsed() < Duration::from_secs(30) {
                            switch.push(
                                LineKind::Info,
                                "previous release still answering; waiting for reload",
                            );
                            let _ = reopened.channel.close().await;
                            tokio::time::sleep(RECONNECT_INTERVAL).await;
                            continue;
                        }
                        if !success {
                            bail!("upgrade verification failed: expected {expected}, running {} (check all stack members)", info.version.unwrap_or_default());
                        }
                        *shell = reopened;
                        switch.set_upgrade(Progress::Complete {
                            version: expected.clone(),
                        });
                        return Ok(format!(
                            "upgrade successful: {expected}; cleanup is available"
                        ));
                    }
                    Err(error) => switch.push(
                        LineKind::Error,
                        format!("reconnect facts not ready: {error:#}; retrying"),
                    ),
                },
                Err(error) => {
                    let error = format!("{error:#}");
                    let failure = crate::engine::Failure::from_message(&error);
                    if failure.kind == crate::engine::FailureKind::Authentication {
                        login_failures += 1;
                        if login_budget.used.load(Ordering::Acquire) >= 3 {
                            bail!("SSH authentication limit reached after reboot; reconnect manually ({error})");
                        }
                        #[cfg(not(test))]
                        let delay = Duration::from_secs(if login_failures == 1 { 30 } else { 60 });
                        #[cfg(test)]
                        let delay =
                            Duration::from_millis(if login_failures == 1 { 100 } else { 200 });
                        next_login = Instant::now() + delay;
                    }
                    switch.push(
                        LineKind::Info,
                        format!("reconnect attempt {attempts}: {error}; retrying"),
                    );
                    logger.log(
                        Event::new(
                            LogLevel::Warning,
                            "switch",
                            format!(
                                "{}: upgrade reconnect attempt {attempts}; retrying",
                                switch.host
                            ),
                        )
                        .result(error.clone()),
                    );
                    switch.set_upgrade(Progress::Rebooting {
                        since,
                        attempts,
                        last_error: Some(error),
                    });
                }
            }
            switch.set_state(SwitchState::Rebooting);
        }
        tokio::select! { _ = tokio::time::sleep(RECONNECT_INTERVAL) => {}, _ = cancelled(switch) => bail!("cancelled while rebooting") }
    }
}

/// Password first, keyboard-interactive second — Cisco devices offer one or
/// the other depending on how the vty lines are configured.
async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    target: &Target,
    budget: Option<&LoginBudget>,
) -> Result<bool> {
    if let Some(budget) = budget {
        budget.reserve()?;
    }
    if session
        .authenticate_password(target.username.clone(), target.password.clone())
        .await?
    {
        return Ok(true);
    }
    let mut response = session
        .authenticate_keyboard_interactive_start(target.username.clone(), None)
        .await?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { ref prompts, .. } => {
                if let Some(budget) = budget {
                    budget.reserve()?;
                }
                let answers = prompts.iter().map(|_| target.password.clone()).collect();
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::upgrade;

    #[test]
    fn negotiation_error_names_the_failed_category_and_device_offer() {
        let error = russh::Error::NoCommonAlgo {
            kind: russh::AlgorithmKind::Kex,
            ours: vec!["curve25519-sha256".into()],
            theirs: vec!["diffie-hellman-group-exchange-sha1".into()],
        };
        let reason = describe_connect_error(error.into(), "192.0.2.5", 22).to_string();
        assert!(reason.contains("no matching key exchange"));
        assert!(reason.contains("device offers [diffie-hellman-group-exchange-sha1]"));
        assert!(reason.contains("192.0.2.5:22"));
        assert!(!reason.contains("login failed"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unreachable_host_keeps_the_os_error_and_has_a_conditional_privacy_hint() {
        let error = std::io::Error::from_raw_os_error(libc::EHOSTUNREACH);
        let reason = describe_connect_error(error.into(), "192.0.2.5", 22).to_string();
        assert!(reason.contains("before SSH negotiation"));
        assert!(reason.contains("os error 65"));
        assert!(macos_network_hint(&reason)
            .unwrap()
            .contains("If this host works in Terminal"));
        assert!(macos_network_hint("login failed").is_none());
        assert!(macos_network_hint("no matching cipher").is_none());
    }

    #[tokio::test]
    #[ignore = "Optional real-host handshake diagnostic; requires TB_SSH_DIAGNOSTIC_HOST; never logs in"]
    async fn ssh_handshake_diagnostic() {
        let host = std::env::var("TB_SSH_DIAGNOSTIC_HOST").expect("set TB_SSH_DIAGNOSTIC_HOST");
        test_ssh_transport(&host, 22).await.unwrap();
        println!(
            "SSH handshake succeeded for {host}; no authentication attempted or host key saved."
        );
    }

    #[test]
    fn install_checks_current_and_next_boot_variables() {
        assert!(install_boot_ready(
            "BOOT variable = flash:packages.conf;\nManual Boot = no"
        ));
        for boot in ["BOOT variable = flash:old.bin;\nManual Boot = no", "BOOT variable = flash:packages.conf;\nManual Boot = yes", "BOOT variable = flash:packages.conf;\nBOOT variable = flash:old.bin;\nManual Boot = no", "BOOT variable = flash:packages.conf;", "System image flash:packages.conf"] { assert!(!install_boot_ready(boot),"{boot}"); }
    }

    #[test]
    fn ansi_sequences_split_between_packets_do_not_hide_prompts() {
        let mut escape = 0;
        assert_eq!(terminal_text(b"\x1b[3", &mut escape), "");
        assert_eq!(terminal_text(b"2mSwitch#\x1b[0m", &mut escape), "Switch#");
        assert_eq!(escape, 0);
        assert!(matches!(
            tail_signal(&terminal_text(b"\x1b[31mSwitch#\x1b[0m", &mut escape)),
            Some(Signal::PromptEnabled(_))
        ));
        assert_eq!(terminal_text(b"\x1b]title", &mut escape), "");
        assert_eq!(terminal_text(b"\x07OK", &mut escape), "OK");
    }

    #[test]
    fn recognises_device_prompts() {
        assert_eq!(
            tail_signal("cat9k-1#"),
            Some(Signal::PromptEnabled("cat9k-1#".into()))
        );
        assert_eq!(
            tail_signal("Switch>"),
            Some(Signal::PromptUser("Switch>".into()))
        );
        assert_eq!(tail_signal("Password: "), Some(Signal::Password));
        assert_eq!(
            tail_signal("Destination filename [img.bin]?"),
            Some(Signal::Question("Destination filename [img.bin]?".into()))
        );
        assert_eq!(tail_signal("!!!!!!!!!!"), None);
        assert_eq!(tail_signal(""), None);
        // Not a prompt: a sentence that happens to end in '>'.
        assert_eq!(tail_signal("a > b"), None);
    }

    #[test]
    fn reads_the_round_trip_time_from_ping() {
        let output = "PING 10.0.0.1 (10.0.0.1): 56 data bytes\n\
                      64 bytes from 10.0.0.1: icmp_seq=0 ttl=254 time=1.234 ms\n";
        assert_eq!(parse_ping_rtt(output), Some(Duration::from_micros(1234)));
        assert_eq!(parse_ping_rtt("Request timeout for icmp_seq 0\n"), None);
        assert_eq!(parse_ping_rtt(""), None);
    }

    #[test]
    fn subnet_ranges_validate_and_exclude_only_real_network_and_broadcast_addresses() {
        let hosts = subnet_hosts("192.168.10.42/24").unwrap();
        assert_eq!(hosts.len(), 254);
        assert_eq!(hosts.first().unwrap(), "192.168.10.1");
        assert_eq!(hosts.last().unwrap(), "192.168.10.254");
        assert_eq!(
            subnet_hosts("192.168.10.0/31").unwrap(),
            ["192.168.10.0", "192.168.10.1"]
        );
        assert_eq!(
            subnet_hosts("255.255.255.255/32").unwrap(),
            ["255.255.255.255"]
        );
        for invalid in [
            "192.168.1.0",
            "bad/24",
            "192.168.1.0/33",
            "192.168.1.0/0",
            "::1/128",
        ] {
            assert!(subnet_hosts(invalid).is_err(), "{invalid}");
        }
    }

    #[tokio::test]
    async fn sweep_is_parallel_bounded_and_connects_only_to_replying_hosts() {
        let scan = Arc::new(SubnetScan {
            progress: Mutex::new(ScanProgress {
                total: 64,
                ..Default::default()
            }),
            cancel: AtomicBool::new(false),
        });
        let active = Arc::new(AtomicU64::new(0));
        let peak = Arc::new(AtomicU64::new(0));
        let replies = Mutex::new(Vec::new());
        let probe = {
            let active = active.clone();
            let peak = peak.clone();
            move |host: String| {
                let active = active.clone();
                let peak = peak.clone();
                async move {
                    let concurrent = active.fetch_add(1, Ordering::Relaxed) + 1;
                    peak.fetch_max(concurrent, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    active.fetch_sub(1, Ordering::Relaxed);
                    let i: usize = host.parse().unwrap();
                    if i == 63 {
                        Err("cannot run ping".into())
                    } else {
                        Ok(i.is_multiple_of(2))
                    }
                }
            }
        };
        sweep_subnet(
            (0..64).map(|i| i.to_string()).collect(),
            scan.clone(),
            probe,
            |host| replies.lock().unwrap().push(host),
        )
        .await;
        assert_eq!(peak.load(Ordering::Relaxed), 32);
        let progress = scan.progress();
        assert_eq!((progress.checked, progress.reachable), (64, 32));
        assert!(progress.finished);
        assert_eq!(progress.error.as_deref(), Some("cannot run ping"));
        assert!(replies
            .lock()
            .unwrap()
            .iter()
            .all(|host| host.parse::<usize>().unwrap().is_multiple_of(2)));
        assert_eq!(replies.lock().unwrap().len(), 32);
    }

    #[test]
    fn cleanup_checks_only_candidates_and_all_running_stack_versions() {
        let output = "cat9k_lite-rpbase.17.12.06.SPA.pkg\nFile is in use, will not delete.\nThe following files will be deleted:\n[switch 1]:\n/flash/cat9k_lite-rpbase.17.12.05.SPA.pkg\n/flash/cat9k_lite_iosxe.17.12.05.SPA.bin\nDo you want to remove the above files? [y/n]";
        let files = cleanup_candidates(output);
        assert_eq!(files.len(), 2);
        let mut version = VersionInfo {
            version: Some("17.12.06".into()),
            image: Some("flash:packages.conf".into()),
            ..Default::default()
        };
        assert!(cleanup_warning(&files, &version).is_none());
        assert!(cleanup_warning(
            &["/flash/cat9k_lite_iosxe.17.12.6.SPA.bin".into()],
            &version
        )
        .unwrap()
        .contains("DANGER"));
        assert!(cleanup_warning(
            &["/flash/cat9k_lite_iosxe.17.12.060.SPA.bin".into()],
            &version
        )
        .is_none());
        assert!(cleanup_warning(&["/flash/packages.conf".into()], &version).is_some());
        version.members.push(crate::cisco::StackMember {
            number: 2,
            model: "C9200L".into(),
            version: "17.12.05".into(),
            image: "CAT9K_LITE_IOSXE".into(),
            mode: "INSTALL".into(),
            active: false,
        });
        assert!(cleanup_warning(&files, &version).is_some());
        assert!(cleanup_warning(&files, &VersionInfo::default())
            .unwrap()
            .contains("could not be verified"));
        assert!(cleanup_warning(&[], &version).is_some());
        let nothing = "[R0]: /flash/cat9k_lite-rpbase.17.12.06.SPA.pkg File is in use, will not delete.\nSUCCESS: No extra package or provisioning files found on media. Nothing to clean.\nSUCCESS: Files deleted.";
        assert!(cleanup_candidates(nothing).is_empty());
        assert_eq!(tail_signal("Do you want to remove the above files?"), None);
        assert!(matches!(
            tail_signal("Do you want to remove the above files? [y/n] "),
            Some(Signal::Question(_))
        ));
    }

    #[test]
    fn bulk_replaces_a_key_without_losing_other_devices_or_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let old = russh_keys::key::KeyPair::generate_ed25519()
            .clone_public_key()
            .unwrap();
        let replacement = russh_keys::key::KeyPair::generate_ed25519()
            .clone_public_key()
            .unwrap();
        store_host_key("10.0.0.1", 22, &old, &path, false).unwrap();
        store_host_key("10.0.0.2", 22, &old, &path, false).unwrap();
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("10.0.0.1 ", "10.0.0.1,alias ");
        std::fs::write(&path, text).unwrap();
        store_host_key("10.0.0.1", 22, &replacement, &path, true).unwrap();
        store_host_key("10.0.0.1", 22, &replacement, &path, true).unwrap();
        assert!(russh_keys::check_known_hosts_path("10.0.0.1", 22, &replacement, &path).unwrap());
        assert!(russh_keys::check_known_hosts_path("10.0.0.2", 22, &old, &path).unwrap());
        assert!(russh_keys::check_known_hosts_path("alias", 22, &old, &path).unwrap());
        assert_eq!(
            russh_keys::known_hosts::known_host_keys_path("10.0.0.1", 22, &path)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn flash_sizes_are_shown_in_megabytes() {
        assert_eq!(fmt_mb(234_979_328), "235 MB");
        assert_eq!(fmt_mb(1_956_839_424), "1957 MB");
        assert_eq!(fmt_mb(0), "0 MB");
    }

    #[test]
    fn destination_device_comes_from_the_copy_command() {
        assert_eq!(
            copy_destination_device("copy http://10.0.0.1/a.bin flash:"),
            Some("flash:".into())
        );
        assert_eq!(
            copy_destination_device("copy http://10.0.0.1/a.bin bootflash:new.bin"),
            Some("bootflash:".into())
        );
        assert_eq!(copy_destination_device("show version"), None);
    }

    /// A minimal IOS impersonator: password login, one shell channel, and the
    /// prompts a `copy` walks through. It answers `dir` and `show version`
    /// with the recorded output of a real C9200L and records every line it
    /// received, so the tests can assert what transferbuddy typed.
    mod fake_ios {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use russh::server::{Auth, Handler, Msg, Session};
        use russh::{Channel, ChannelId, CryptoVec, MethodSet};

        const SHOW_VERSION: &str = include_str!("../../../testdata/show_version_c9200l.txt");
        const DIR_FLASH: &str = include_str!("../../../testdata/dir_flash.txt");

        /// Strip the command echo and the trailing prompt from a recording:
        /// the device sends those itself.
        fn body(fixture: &str) -> String {
            let lines: Vec<&str> = fixture.lines().collect();
            lines[1..lines.len().saturating_sub(1)].join("\r\n")
        }

        pub struct Transcript {
            pub received: Vec<String>,
            pub connections: usize,
            pub cleanup_output: Option<String>,
            pub md5: String,
            pub running_version: String,
            pub boot: String,
            pub bundle: bool,
            pub auth_failures: usize,
            pub fail_after_reload: usize,
            pub pending_version: String,
            pub deleted: Vec<String>,
            pub delete_question: Option<String>,
            pub hold_copy: bool,
        }

        impl Transcript {
            pub fn new() -> Arc<Mutex<Self>> {
                Arc::new(Mutex::new(Self {
                    received: Vec::new(),
                    connections: 0,
                    md5: "78805a221a988e79ef3f42d7c5bfd418".into(),
                    running_version: "17.15.03".into(),
                    boot: "BOOT variable = flash:packages.conf;\r\nManual Boot = no".into(),
                    bundle: false,
                    auth_failures: 0,
                    fail_after_reload: 0,
                    pending_version: "17.15.06".into(),
                    deleted: Vec::new(), delete_question: None, hold_copy: false,
                    cleanup_output: Some("\r\nThe following files will be deleted:\r\n[switch 1]:\r\n/flash/cat9k_lite-rpbase.17.15.02.SPA.pkg\r\nDo you want to remove the above files? [y/n]".into()),
                }))
            }
        }

        pub struct Device {
            log: Arc<Mutex<Transcript>>,
            enable_password: String,
            /// Delay before the first prompt, to make the window between
            /// "host key accepted" and "session ready" observable.
            greet_delay: std::time::Duration,
            copy_delay: std::time::Duration,
            line: String,
            enabled: bool,
            expect_enable_password: bool,
            in_copy: bool,
            in_cleanup: bool,
            in_install: bool,
            in_delete: Option<String>,
            channels: HashMap<ChannelId, Channel<Msg>>,
        }

        impl Device {
            fn say(&self, session: &mut Session, id: ChannelId, text: &str) {
                session.data(id, CryptoVec::from_slice(text.as_bytes()));
            }

            fn prompt(&self) -> &'static str {
                if self.enabled {
                    "\r\ncat9k-1#"
                } else {
                    "\r\ncat9k-1>"
                }
            }

            fn reload(&self, session: &mut Session, id: ChannelId) {
                let mut log = self.log.lock().unwrap();
                log.running_version = log.pending_version.clone();
                log.auth_failures = log.fail_after_reload;
                drop(log);
                session.eof(id);
                session.close(id);
            }

            fn on_line(&mut self, session: &mut Session, id: ChannelId, line: &str) {
                self.log.lock().unwrap().received.push(line.to_string());
                if let Some(path) = self.in_delete.take() {
                    self.log.lock().unwrap().deleted.push(path);
                    self.say(session, id, "\r\ncat9k-1#");
                    return;
                }
                if self.in_install {
                    self.in_install = false;
                    if line == "y" {
                        self.reload(session, id);
                    } else {
                        self.say(session, id, "\r\nActivation cancelled\r\ncat9k-1#");
                    }
                    return;
                }
                if self.in_cleanup {
                    self.in_cleanup = false;
                    self.say(session, id, "\r\nSUCCESS: install_remove\r\ncat9k-1#");
                    return;
                }

                if self.expect_enable_password {
                    self.expect_enable_password = false;
                    if line == self.enable_password {
                        self.enabled = true;
                    } else {
                        self.say(session, id, "\r\n% Access denied");
                    }
                    let p = self.prompt();
                    self.say(session, id, p);
                    return;
                }
                if self.in_copy {
                    if self.log.lock().unwrap().hold_copy {
                        return;
                    }
                    // The answer to "Destination filename [...]?".
                    self.in_copy = false;
                    self.say(
                        session,
                        id,
                        "\r\n!!!!!!!!!!!!!!!!!!!!\r\n\
                         504057659 bytes copied in 728.176 secs (692220 bytes/sec)\r\ncat9k-1#",
                    );
                    return;
                }
                match line {
                    "install remove inactive" => {
                        let output = self.log.lock().unwrap().cleanup_output.clone();
                        if let Some(output) = output {
                            self.in_cleanup = true;
                            self.say(session, id, &output);
                        } else {
                            self.say(session, id, "\r\n[R0]: /flash/cat9k_lite-rpbase.17.15.03.SPA.pkg File is in use, will not delete.\r\nSUCCESS: No extra package or provisioning files found on media. Nothing to clean.\r\nSUCCESS: Files deleted.\r\ncat9k-1#");
                        }
                    }
                    "enable" => {
                        if self.enable_password.is_empty() {
                            self.enabled = true;
                            self.say(session, id, self.prompt());
                        } else {
                            self.expect_enable_password = true;
                            self.say(session, id, "\r\nPassword: ");
                        }
                    }
                    "terminal length 0" => {
                        let p = self.prompt();
                        self.say(session, id, p);
                    }
                    "show version" => {
                        let log = self.log.lock().unwrap();
                        let mut version =
                            body(SHOW_VERSION).replace("17.15.03", &log.running_version);
                        if log.bundle {
                            version = version
                                .replace("INSTALL", "BUNDLE")
                                .replace("packages.conf", "old.bin");
                        }
                        let text = format!("\r\n{version}\r\ncat9k-1#");
                        self.say(session, id, &text);
                    }
                    l if l.starts_with("dir ") || l == "dir" => {
                        let log = self.log.lock().unwrap();
                        let listing = body(DIR_FLASH)
                            .lines()
                            .filter(|line| {
                                !log.deleted.iter().any(|path| {
                                    line.ends_with(path.rsplit(['/', ':']).next().unwrap())
                                })
                            })
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                            .join("\r\n");
                        let text = format!("\r\n{listing}\r\ncat9k-1#");
                        self.say(session, id, &text);
                    }
                    "exit" => {
                        session.eof(id);
                        session.close(id);
                    }
                    cmd if cmd.starts_with("delete /force ") => {
                        let path = cmd.split_whitespace().last().unwrap().to_string();
                        let question = self.log.lock().unwrap().delete_question.clone();
                        if let Some(question) = question {
                            self.in_delete = Some(path);
                            self.say(session, id, &question);
                        } else {
                            self.log.lock().unwrap().deleted.push(path);
                            self.say(session, id, "\r\ncat9k-1#");
                        }
                    }
                    "show boot" => {
                        let boot = self.log.lock().unwrap().boot.clone();
                        self.say(session, id, &format!("\r\n{boot}\r\ncat9k-1#"));
                    }
                    "write memory" => self.say(
                        session,
                        id,
                        "\r\nBuilding configuration...\r\n[OK]\r\ncat9k-1#",
                    ),
                    cmd if cmd.starts_with("verify /md5 ") => {
                        let md5 = self.log.lock().unwrap().md5.clone();
                        self.say(
                            session,
                            id,
                            &format!(
                                "\r\n{cmd}\r\nverify /md5 (flash:image.bin) = {md5}\r\ncat9k-1#"
                            ),
                        );
                    }
                    cmd if cmd.starts_with("install add file ") => {
                        if cmd.ends_with("prompt-level none") {
                            self.reload(session, id);
                        } else {
                            self.in_install = true;
                            self.say(session, id, "\r\nThis operation may require a reload. Do you want to proceed? [y/n] ");
                        }
                    }
                    cmd if cmd.starts_with("copy ") => {
                        self.in_copy = true;
                        self.say(
                            session,
                            id,
                            "\r\nDestination filename [cat9k_lite_iosxe.17.15.06.SPA.bin]? ",
                        );
                    }
                    _ => {
                        let text = format!("\r\n% Invalid input detected{}", self.prompt());
                        self.say(session, id, &text);
                    }
                }
            }
        }

        #[async_trait::async_trait]
        impl Handler for Device {
            type Error = russh::Error;

            async fn auth_password(
                &mut self,
                user: &str,
                password: &str,
            ) -> Result<Auth, Self::Error> {
                {
                    let mut log = self.log.lock().unwrap();
                    if log.auth_failures > 0 {
                        log.auth_failures -= 1;
                        return Ok(Auth::Reject {
                            proceed_with_methods: None,
                        });
                    }
                }
                if user == "netadmin" && password == "letmein" {
                    self.log.lock().unwrap().connections += 1;
                    Ok(Auth::Accept)
                } else {
                    Ok(Auth::Reject {
                        proceed_with_methods: None,
                    })
                }
            }

            async fn channel_open_session(
                &mut self,
                channel: Channel<Msg>,
                _session: &mut Session,
            ) -> Result<bool, Self::Error> {
                self.channels.insert(channel.id(), channel);
                Ok(true)
            }

            async fn pty_request(
                &mut self,
                id: ChannelId,
                _term: &str,
                _cw: u32,
                _rh: u32,
                _pw: u32,
                _ph: u32,
                _modes: &[(russh::Pty, u32)],
                session: &mut Session,
            ) -> Result<(), Self::Error> {
                session.channel_success(id);
                Ok(())
            }

            async fn shell_request(
                &mut self,
                id: ChannelId,
                session: &mut Session,
            ) -> Result<(), Self::Error> {
                session.channel_success(id);
                if !self.greet_delay.is_zero() {
                    tokio::time::sleep(self.greet_delay).await;
                }
                self.say(session, id, "\r\ncat9k-1>");
                Ok(())
            }

            async fn data(
                &mut self,
                id: ChannelId,
                data: &[u8],
                session: &mut Session,
            ) -> Result<(), Self::Error> {
                for ch in String::from_utf8_lossy(data).chars() {
                    match ch {
                        '\u{3}' => {
                            self.line.clear();
                            self.in_copy = false;
                            self.log.lock().unwrap().received.push("<Ctrl+C>".into());
                            self.say(session, id, "\r\nCopy aborted\r\ncat9k-1#");
                        }
                        '\r' | '\n' => {
                            let line = std::mem::take(&mut self.line);
                            if self.in_copy && !self.copy_delay.is_zero() {
                                tokio::time::sleep(self.copy_delay).await;
                            }
                            self.on_line(session, id, &line);
                        }
                        c => self.line.push(c),
                    }
                }
                Ok(())
            }
        }

        /// Start the fake device on a loopback port and return the port.
        pub async fn spawn(log: Arc<Mutex<Transcript>>, enable_password: &str) -> u16 {
            spawn_with_delay(log, enable_password, std::time::Duration::ZERO).await
        }

        /// Same, but slow to greet — see [`Device::greet_delay`].
        pub async fn spawn_with_delay(
            log: Arc<Mutex<Transcript>>,
            enable_password: &str,
            greet_delay: std::time::Duration,
        ) -> u16 {
            spawn_with_delays(log, enable_password, greet_delay, std::time::Duration::ZERO).await
        }

        pub async fn spawn_with_delays(
            log: Arc<Mutex<Transcript>>,
            enable_password: &str,
            greet_delay: std::time::Duration,
            copy_delay: std::time::Duration,
        ) -> u16 {
            let config = russh::server::Config {
                methods: MethodSet::PASSWORD,
                keys: vec![russh_keys::key::KeyPair::generate_ed25519()],
                ..Default::default()
            };
            spawn_with_config(log, enable_password, greet_delay, copy_delay, config).await
        }

        pub async fn spawn_with_config(
            log: Arc<Mutex<Transcript>>,
            enable_password: &str,
            greet_delay: std::time::Duration,
            copy_delay: std::time::Duration,
            config: russh::server::Config,
        ) -> u16 {
            let config = Arc::new(config);
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let enable_password = enable_password.to_string();
            tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let handler = Device {
                        log: log.clone(),
                        enable_password: enable_password.clone(),
                        greet_delay,
                        copy_delay,
                        line: String::new(),
                        enabled: false,
                        expect_enable_password: false,
                        in_copy: false,
                        in_cleanup: false,
                        in_install: false,
                        in_delete: None,
                        channels: HashMap::new(),
                    };
                    let config = config.clone();
                    tokio::spawn(async move {
                        let _ = russh::server::run_stream(config, stream, handler).await;
                    });
                }
            });
            port
        }
    }

    fn manager() -> SwitchManager {
        let logger = Arc::new(Logger::new(LogLevel::Debug, None, false));
        SwitchManager::new(tokio::runtime::Handle::current(), logger)
    }

    fn target(port: u16, known_hosts: PathBuf) -> Target {
        Target {
            host: "127.0.0.1".into(),
            port,
            username: "netadmin".into(),
            password: "letmein".into(),
            enable_password: "s3cret".into(),
            known_hosts,
            auto_trust: false,
        }
    }

    #[tokio::test]
    async fn legacy_cisco_sha1_rsa_and_ctr_or_cbc_connect_without_a_downgrade_on_modern_hosts() {
        use russh::{cipher, kex, mac, server, MethodSet, Preferred};
        use russh_keys::key;
        let key = key::KeyPair::generate_rsa(2048, key::SignatureHash::SHA1).unwrap();
        for cipher in [
            cipher::AES_128_CTR,
            cipher::AES_128_CBC,
            cipher::TRIPLE_DES_CBC,
        ] {
            let log = fake_ios::Transcript::new();
            let config = server::Config {
                methods: MethodSet::PASSWORD,
                keys: vec![key.clone()],
                preferred: Preferred {
                    kex: std::borrow::Cow::Owned(vec![kex::DH_G14_SHA1]),
                    key: std::borrow::Cow::Owned(vec![key::SSH_RSA]),
                    cipher: std::borrow::Cow::Owned(vec![cipher]),
                    mac: std::borrow::Cow::Owned(vec![mac::HMAC_SHA1]),
                    ..Default::default()
                },
                ..Default::default()
            };
            let port = fake_ios::spawn_with_config(
                log.clone(),
                "s3cret",
                Duration::ZERO,
                Duration::ZERO,
                config,
            )
            .await;
            let dir = tempfile::tempdir().unwrap();
            let mut t = target(port, dir.path().join("known_hosts"));
            t.auto_trust = true;
            let manager = manager();
            let switch = manager.connect(t);
            wait_for("legacy SSH connection", || {
                switch.state() == SwitchState::Ready || switch.state().is_over()
            })
            .await;
            assert_eq!(
                switch.state(),
                SwitchState::Ready,
                "cipher {cipher:?}: {:?}",
                switch.state()
            );
            assert_eq!(switch.facts().hostname.as_deref(), Some("cat9k-1"));
            switch.cancel();
        }
        let preferred = client_config().preferred;
        assert!(
            preferred
                .kex
                .iter()
                .position(|k| *k == kex::CURVE25519)
                .unwrap()
                < preferred
                    .kex
                    .iter()
                    .position(|k| *k == kex::DH_G14_SHA1)
                    .unwrap()
        );
    }

    /// Poll until `f` holds, with a generous ceiling so a hung session fails
    /// the test instead of the suite.
    async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Connect and accept the host key, leaving a ready session.
    async fn connected(mgr: &SwitchManager, port: u16, dir: &std::path::Path) -> Arc<Switch> {
        let sw = mgr.connect(target(port, dir.join("known_hosts")));
        wait_for("the host key question", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;
        sw.answer_host_key(true);
        // Accepting must clear the question right away. Leaving the state on
        // `HostKey` while the handshake finishes freezes the popup on the
        // question, and every further key press reads as another answer.
        wait_for("the key to be stored", || {
            sw.transcript()
                .iter()
                .any(|l| l.text.contains("accepted and stored"))
        })
        .await;
        assert!(
            !matches!(sw.state(), SwitchState::HostKey { .. }),
            "the host key question outlived the answer"
        );
        wait_for("a ready session", || sw.state().is_live()).await;
        sw
    }

    async fn prepare_upgrade(sw: &Arc<Switch>, dir: &std::path::Path) {
        let name = "cat9k_lite_iosxe.17.15.06.SPA.bin";
        let file = dir.join(name);
        std::fs::write(&file, b"image").unwrap();
        sw.submit(Job::PrepareUpgrade {
            overwrite: false,
            rel_path: name.into(),
            local_path: file,
            remote: format!("flash:{name}"),
            command: format!("copy http://127.0.0.1/{name} flash:{name}"),
            size: 5,
            protocol: Protocol::Http,
        });
        wait_for("verified upgrade", || {
            matches!(sw.upgrade(), crate::upgrade::Progress::Verified { .. })
                && sw.state() == SwitchState::Ready
        })
        .await;
    }

    #[tokio::test]
    async fn existing_image_verifies_without_copy_and_same_version_cannot_install() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("facts", || {
            sw.facts().version.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        prepare_upgrade(&sw, dir.path()).await;
        assert!(!log
            .lock()
            .unwrap()
            .received
            .iter()
            .any(|line| line.starts_with("copy ")));
        log.lock().unwrap().running_version = "17.15.06".into();
        sw.submit(Job::Install { yolo: true });
        wait_for("same-version refusal", || {
            matches!(sw.upgrade(), crate::upgrade::Progress::Failed(_))
        })
        .await;
        assert!(sw.upgrade().label().contains("already installed"));
        assert!(!log
            .lock()
            .unwrap()
            .received
            .iter()
            .any(|line| line == "write memory" || line.starts_with("install add")));
        sw.cancel();
    }

    #[tokio::test]
    async fn existing_image_hash_mismatch_or_missing_hash_never_enables_install() {
        for hash in ["00000000000000000000000000000000", ""] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            log.lock().unwrap().md5 = hash.into();
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("facts", || {
                sw.facts().version.is_some() && sw.state() == SwitchState::Ready
            })
            .await;
            let name = "cat9k_lite_iosxe.17.15.06.SPA.bin";
            let local = dir.path().join(name);
            std::fs::write(&local, b"image").unwrap();
            sw.submit(Job::VerifyUpgrade {
                rel_path: name.into(),
                local_path: local,
                remote: format!("flash:{name}"),
            });
            wait_for("verification rejection", || {
                matches!(sw.upgrade(), crate::upgrade::Progress::Failed(_))
                    && sw.state() == SwitchState::Ready
            })
            .await;
            assert!(!log
                .lock()
                .unwrap()
                .received
                .iter()
                .any(|line| line.starts_with("copy ") || line.starts_with("install add")));
            sw.cancel();
        }
    }

    #[tokio::test]
    async fn confirmed_delete_removes_exact_file_or_recursive_directory_and_refreshes() {
        for (path, recursive, question) in [
            ("flash:packages.conf", false, None),
            (
                "flash:packages.conf",
                false,
                Some("Delete flash:/packages.conf? [confirm]"),
            ),
            (
                "flash:.installer",
                true,
                Some("Delete filename [.installer]? "),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            log.lock().unwrap().delete_question = question.map(str::to_string);
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("facts", || {
                sw.facts().version.is_some() && sw.state() == SwitchState::Ready
            })
            .await;
            let before = sw.jobs_done();
            sw.submit(Job::Delete {
                path: path.into(),
                recursive,
            });
            wait_for("delete complete", || {
                sw.jobs_done() > before && sw.state() == SwitchState::Ready
            })
            .await;
            let received = log.lock().unwrap().received.clone();
            assert!(received.contains(&format!(
                "delete /force {}{path}",
                if recursive { "/recursive " } else { "" }
            )));
            assert!(!sw
                .listing("flash:")
                .unwrap()
                .unwrap()
                .iter()
                .any(|e| e.name == path.split_once(':').unwrap().1));
            assert!(sw.last_result().unwrap().is_ok());
            sw.cancel();
        }
    }

    #[tokio::test]
    async fn unexpected_delete_prompt_is_not_answered() {
        for prompt in [
            "Erase all flash? [confirm]",
            "Delete flash:other? [confirm]",
            "Delete flash:.installer-other? [confirm]",
            "Delete filename [other]? ",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            log.lock().unwrap().delete_question = Some(prompt.into());
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("facts", || {
                sw.facts().version.is_some() && sw.state() == SwitchState::Ready
            })
            .await;
            sw.submit(Job::Delete {
                path: "flash:.installer".into(),
                recursive: true,
            });
            wait_for("delete aborted", || {
                matches!(sw.state(), SwitchState::Offline { .. })
            })
            .await;
            let received = log.lock().unwrap().received.clone();
            let index = received
                .iter()
                .position(|line| line.starts_with("delete /force"))
                .unwrap();
            assert_eq!(index + 1, received.len());
            assert!(log.lock().unwrap().deleted.is_empty());
            sw.cancel();
        }
    }

    #[tokio::test]
    async fn exit_or_escape_in_manual_cli_releases_cli_mode_and_main_session() {
        for escape in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("facts", || {
                sw.facts().version.is_some() && sw.state() == SwitchState::Ready
            })
            .await;
            assert!(sw.start_cli());
            if escape {
                sw.close_cli();
            } else {
                sw.cli_send(b"exit\r".to_vec());
            }
            wait_for("remote exit", || {
                !sw.cli_open() && sw.state() == SwitchState::Ready
            })
            .await;
            let version_reads = log
                .lock()
                .unwrap()
                .received
                .iter()
                .filter(|s| s.as_str() == "show version")
                .count();
            sw.submit(Job::Facts);
            wait_for("main SSH usable", || {
                sw.state() == SwitchState::Ready
                    && log
                        .lock()
                        .unwrap()
                        .received
                        .iter()
                        .filter(|s| s.as_str() == "show version")
                        .count()
                        > version_reads
            })
            .await;
            sw.cancel();
        }
    }

    #[tokio::test]
    async fn mixed_subnet_scan_deduplicates_and_keeps_explicit_ip_out_of_discovery() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager();
        let visited = Arc::new(Mutex::new(Vec::new()));
        let probe = visited.clone();
        mgr.start_scan_many_with_probe(
            target(1, dir.path().join("known_hosts")),
            &["127.0.0.0/30".into(), "127.0.0.2/32".into()],
            &["127.0.0.1".into()],
            Protocol::Http,
            move |host| {
                probe.lock().unwrap().push(host);
                async { Ok(false) }
            },
        )
        .unwrap();
        wait_for("combined scan", || mgr.scan().unwrap().progress().finished).await;
        assert_eq!(*visited.lock().unwrap(), ["127.0.0.2"]);
        assert_eq!(mgr.scan().unwrap().progress().total, 1);
    }

    #[tokio::test]
    async fn upgrade_requires_reload_consent_then_retries_login_and_verifies_all_members() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        log.lock().unwrap().fail_after_reload = 2;
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("initial facts", || {
            sw.facts().version.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        prepare_upgrade(&sw, dir.path()).await;
        sw.submit(Job::Install { yolo: false });
        wait_for("reload question", || {
            matches!(sw.state(), SwitchState::ReloadConfirm { .. })
        })
        .await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!log.lock().unwrap().received.iter().any(|line| line == "y"));
        sw.answer_reload(true);
        wait_for("reboot timer", || {
            matches!(sw.upgrade(), crate::upgrade::Progress::Rebooting { .. })
        })
        .await;
        wait_for("successful upgrade", || {
            matches!(sw.upgrade(), crate::upgrade::Progress::Complete { .. })
                && sw.state() == SwitchState::Ready
        })
        .await;
        let received = log.lock().unwrap().received.clone();
        let saved = received
            .iter()
            .position(|line| line == "write memory")
            .unwrap();
        let installed = received
            .iter()
            .position(|line| line.starts_with("install add file"))
            .unwrap();
        assert!(saved < installed);
        assert_eq!(
            received
                .iter()
                .filter(|line| line.starts_with("verify /md5"))
                .count(),
            2
        );
        assert!(
            sw.transcript()
                .iter()
                .filter(|line| line.text.contains("reconnect attempt"))
                .count()
                >= 2
        );
        assert!(sw
            .facts()
            .version
            .unwrap()
            .members
            .iter()
            .all(|m| m.version == "17.15.06" && m.mode == "INSTALL"));
        assert_eq!(mgr.list().len(), 1);
        sw.submit(Job::RemoveInactive);
        wait_for("cleanup confirmation", || {
            matches!(sw.state(), SwitchState::CleanupConfirm { .. })
        })
        .await;
        sw.answer_cleanup(true);
        wait_for("cleanup complete", || sw.state() == SwitchState::Ready).await;
        sw.cancel();
    }

    #[tokio::test]
    async fn upgrade_decline_keeps_verified_image_and_yolo_uses_prompt_level_none() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("facts", || {
            sw.state() == SwitchState::Ready && sw.facts().version.is_some()
        })
        .await;
        prepare_upgrade(&sw, dir.path()).await;
        sw.submit(Job::Install { yolo: false });
        wait_for("reload question", || {
            matches!(sw.state(), SwitchState::ReloadConfirm { .. })
        })
        .await;
        sw.answer_reload(false);
        wait_for("declined", || {
            sw.state() == SwitchState::Ready
                && matches!(sw.upgrade(), crate::upgrade::Progress::Verified { .. })
        })
        .await;
        assert_eq!(log.lock().unwrap().running_version, "17.15.03");
        sw.submit(Job::Install { yolo: true });
        wait_for("yolo complete", || {
            sw.state() == SwitchState::Ready
                && matches!(sw.upgrade(), crate::upgrade::Progress::Complete { .. })
        })
        .await;
        assert!(log
            .lock()
            .unwrap()
            .received
            .iter()
            .any(|line| line.ends_with("activate commit prompt-level none")));
        sw.cancel();
    }

    #[tokio::test]
    async fn checksum_mismatch_and_bundle_or_wrong_boot_prevent_install() {
        for case in ["checksum", "bundle", "boot"] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("facts", || {
                sw.state() == SwitchState::Ready && sw.facts().version.is_some()
            })
            .await;
            prepare_upgrade(&sw, dir.path()).await;
            match case {
                "checksum" => log.lock().unwrap().md5 = "00000000000000000000000000000000".into(),
                "bundle" => log.lock().unwrap().bundle = true,
                _ => {
                    log.lock().unwrap().boot =
                        "BOOT variable = flash:old.bin;\r\nManual Boot = no".into()
                }
            }
            sw.submit(Job::Install { yolo: true });
            wait_for("rejected preflight", || {
                matches!(sw.upgrade(), crate::upgrade::Progress::Failed(_))
            })
            .await;
            assert!(!log
                .lock()
                .unwrap()
                .received
                .iter()
                .any(|line| line == "write memory" || line.starts_with("install add file")));
            sw.cancel();
        }
    }

    #[tokio::test]
    async fn reconnect_retains_credentials_and_cli_allows_user_commands() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("facts", || {
            sw.state() == SwitchState::Ready && sw.facts().version.is_some()
        })
        .await;
        assert!(sw.start_cli());
        sw.cli_send(b"configure terminal\r".to_vec());
        wait_for("raw CLI command", || {
            log.lock()
                .unwrap()
                .received
                .iter()
                .any(|line| line == "configure terminal")
        })
        .await;
        sw.close_cli();
        wait_for("CLI closed", || sw.state() == SwitchState::Ready).await;
        sw.submit(Job::Disconnect);
        wait_for("disconnected", || sw.state() == SwitchState::Closed).await;
        assert!(mgr.reconnect(sw.clone()));
        wait_for("reconnected", || {
            sw.state() == SwitchState::Ready && log.lock().unwrap().connections >= 3
        })
        .await;
        assert!(Arc::ptr_eq(&sw, &mgr.list()[0]));
        sw.cancel();
    }

    #[tokio::test]
    async fn enable_without_password_can_enter_privileged_mode() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "").await;
        let mgr = manager();
        let mut target = target(port, dir.path().join("known_hosts"));
        target.enable_password.clear();
        target.auto_trust = true;
        let sw = mgr.connect(target);
        wait_for("privileged session", || {
            sw.state() == SwitchState::Ready && sw.facts().version.is_some()
        })
        .await;
        assert!(log
            .lock()
            .unwrap()
            .received
            .iter()
            .any(|line| line == "enable"));
        sw.cancel();
    }

    #[tokio::test]
    async fn failed_subnet_connections_disappear_but_are_logged() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = manager();
        let mut target = target(1, dir.path().join("known_hosts"));
        target.auto_trust = true;
        mgr.start_scan_with_probe(target, "127.0.0.1/32", Protocol::Http, |_| async {
            Ok(true)
        })
        .unwrap();
        wait_for("failed discovered session", || {
            mgr.switches
                .lock()
                .unwrap()
                .iter()
                .any(|sw| matches!(sw.state(), SwitchState::Failed { .. }))
        })
        .await;
        assert!(mgr.list().is_empty());
        assert!(mgr
            .logger
            .entries()
            .iter()
            .any(|entry| entry.level == LogLevel::Error));
    }

    #[tokio::test]
    async fn reads_facts_from_a_fresh_session() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;

        wait_for("device facts", || sw.facts().version.is_some()).await;
        let facts = sw.facts();

        // Hostname comes from the prompt, the rest from show version.
        assert_eq!(facts.hostname.as_deref(), Some("cat9k-1"));
        let version = facts.version.unwrap();
        assert_eq!(version.version.as_deref(), Some("17.15.03"));
        assert_eq!(version.model.as_deref(), Some("C9200L-48P-4X"));
        assert_eq!(version.members.len(), 3);

        // And the free flash space, in MB.
        let flash = facts.flash.expect("dir totals");
        assert_eq!(flash.free, 234_979_328);
        assert_eq!(fmt_mb(flash.free), "235 MB");
        assert!(sw
            .transcript()
            .iter()
            .any(|l| l.text.contains("235 MB free of 1957 MB")));

        // Only whitelisted lines were typed.
        let received = log.lock().unwrap().received.clone();
        assert_eq!(
            received,
            vec![
                "enable",
                "s3cret",
                "terminal length 0",
                "dir flash:",
                "show version"
            ]
        );
    }

    #[tokio::test]
    async fn bulk_connect_trusts_changed_keys_and_reuses_or_replaces_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("known_hosts");
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let old_key = russh_keys::key::KeyPair::generate_ed25519()
            .clone_public_key()
            .unwrap();
        store_host_key("127.0.0.1", port, &old_key, &path, false).unwrap();
        let mgr = manager();
        let mut t = target(port, path.clone());
        t.auto_trust = true;
        let sw = mgr.connect(t.clone());
        let mut other_user = t.clone();
        other_user.username = "someone else".into();
        assert!(Arc::ptr_eq(&sw, &mgr.connect(other_user)));
        wait_for("bulk facts", || {
            sw.facts().updated.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        assert!(sw
            .transcript()
            .iter()
            .any(|line| line.text.contains("automatically trusted")));
        assert_eq!(log.lock().unwrap().connections, 1);
        let keys = russh_keys::known_hosts::known_host_keys_path("127.0.0.1", port, &path).unwrap();
        assert_eq!(keys.len(), 1);
        assert_ne!(keys[0].1, old_key);
        sw.submit(Job::Disconnect);
        wait_for("closed", || sw.state() == SwitchState::Closed).await;
        let again = mgr.connect(t);
        assert_eq!(mgr.list().len(), 1);
        assert_ne!(again.id, sw.id);
        wait_for("reconnected", || again.facts().updated.is_some()).await;
        again.submit(Job::Disconnect);
    }

    #[tokio::test]
    async fn host_key_wait_does_not_consume_the_connection_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let port = fake_ios::spawn(fake_ios::Transcript::new(), "s3cret").await;
        let mgr = manager();
        let sw = mgr.connect(target(port, dir.path().join("known_hosts")));
        wait_for("host key", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;
        tokio::time::sleep(CONNECT_TIMEOUT + Duration::from_secs(1)).await;
        assert!(
            matches!(sw.state(), SwitchState::HostKey { .. }),
            "{:?}",
            sw.state()
        );
        sw.answer_host_key(true);
        wait_for("facts after delayed trust", || sw.facts().updated.is_some()).await;
        sw.submit(Job::Disconnect);
    }

    #[tokio::test]
    async fn cleanup_waits_for_confirmation_and_refreshes_facts_or_reports_nothing_to_clean() {
        for accept in [true, false] {
            let dir = tempfile::tempdir().unwrap();
            let log = fake_ios::Transcript::new();
            let port = fake_ios::spawn(log.clone(), "s3cret").await;
            let mgr = manager();
            let sw = connected(&mgr, port, dir.path()).await;
            wait_for("initial facts", || {
                sw.facts().updated.is_some() && sw.state() == SwitchState::Ready
            })
            .await;
            let before = sw.facts().updated.unwrap();
            sw.submit(Job::RemoveInactive);
            wait_for("cleanup confirmation", || {
                matches!(sw.state(), SwitchState::CleanupConfirm { .. })
            })
            .await;
            assert!(!log
                .lock()
                .unwrap()
                .received
                .iter()
                .any(|s| s == "y" || s == "n"));
            if let SwitchState::CleanupConfirm { files, warning } = sw.state() {
                assert_eq!(files, ["/flash/cat9k_lite-rpbase.17.15.02.SPA.pkg"]);
                assert!(warning.is_none());
            }
            sw.answer_cleanup(accept);
            wait_for("cleanup completion", || {
                sw.jobs_done() == 1 && sw.state() == SwitchState::Ready
            })
            .await;
            assert!(log
                .lock()
                .unwrap()
                .received
                .iter()
                .any(|s| s == if accept { "y" } else { "n" }));
            assert!(sw.facts().updated.unwrap() > before);
            assert!(sw.last_result().unwrap().unwrap().contains(if accept {
                "removed"
            } else {
                "aborted"
            }));
            log.lock().unwrap().cleanup_output = None;
            sw.submit(Job::RemoveInactive);
            wait_for("nothing to clean", || sw.jobs_done() == 2).await;
            assert!(sw
                .last_result()
                .unwrap()
                .unwrap()
                .contains("nothing to clean"));
            sw.submit(Job::Disconnect);
        }
    }

    #[tokio::test]
    async fn cleanup_warns_about_running_release_before_any_yes_is_sent() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        log.lock().unwrap().cleanup_output = Some("\r\nThe following files will be deleted:\r\n/flash/cat9k_lite_iosxe.17.15.03.SPA.bin\r\nDo you want to remove the above files? [y/n]".into());
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("ready", || {
            sw.facts().updated.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        sw.submit(Job::RemoveInactive);
        wait_for("danger warning", || {
            matches!(
                sw.state(),
                SwitchState::CleanupConfirm {
                    warning: Some(_),
                    ..
                }
            )
        })
        .await;
        assert!(sw
            .transcript()
            .iter()
            .any(|line| line.text.contains("DANGER")));
        assert!(!log.lock().unwrap().received.iter().any(|line| line == "y"));
        sw.answer_cleanup(false);
        wait_for("abort", || sw.jobs_done() == 1).await;
        sw.submit(Job::Disconnect);
    }

    #[tokio::test]
    async fn subnet_scan_connects_to_a_pingable_host_and_is_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let port = fake_ios::spawn(fake_ios::Transcript::new(), "s3cret").await;
        let mgr = manager();
        mgr.set_auto_trust(true); // The application defaults to this policy.
        mgr.start_scan_with_probe(
            target(port, dir.path().join("known_hosts")),
            "127.0.0.1/32",
            Protocol::Sftp,
            |_| async { Ok(true) },
        )
        .unwrap();
        wait_for("scan", || mgr.scan().unwrap().progress().finished).await;
        let progress = mgr.scan().unwrap().progress();
        assert_eq!(
            (progress.checked, progress.total, progress.reachable),
            (1, 1, 1)
        );
        let sw = mgr.list().pop().unwrap();
        assert_eq!(sw.protocol(), Protocol::Sftp);
        wait_for("auto trusted SSH", || sw.facts().updated.is_some()).await;
        sw.submit(Job::Disconnect);
        mgr.start_scan(
            target(port, dir.path().join("known_hosts")),
            "127.0.0.0/16",
            Protocol::Http,
        )
        .unwrap();
        assert!(mgr
            .start_scan(
                target(port, dir.path().join("known_hosts")),
                "127.0.0.1/32",
                Protocol::Http
            )
            .is_err());
        mgr.scan().unwrap().cancel();
        wait_for("cancelled scan", || mgr.scan().unwrap().progress().finished).await;
        assert!(mgr.scan().unwrap().progress().cancelled);
        assert_eq!(mgr.scan().unwrap().progress().checked, 0);
    }

    #[tokio::test]
    async fn tracked_copy_abort_recovers_prompt_and_keeps_the_ssh_session_usable() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        log.lock().unwrap().hold_copy = true;
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("initial facts", || {
            sw.facts().version.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(sw.submit_cancellable(
            9001,
            Job::Copy {
                rel_path: "image.bin".into(),
                command: "copy http://10.0.0.1/image.bin flash:".into(),
                overwrite: false,
                platform_check: false
            },
            cancel
        ));
        wait_for("copy is waiting for completion", || {
            log.lock()
                .unwrap()
                .received
                .iter()
                .any(|line| line.is_empty())
        })
        .await;
        assert!(!sw.request_copy_abort(9002));
        assert!(sw.request_copy_abort(9001));
        wait_for("abort recovered prompt", || {
            sw.copy_abort_confirmed(9001) && sw.state() == SwitchState::Ready
        })
        .await;
        let result = sw.take_tracked_result(9001).unwrap();
        assert!(result.unwrap_err().contains("prompt recovered"));
        assert!(log.lock().unwrap().received.contains(&"<Ctrl+C>".into()));
        let before = sw.commands_done();
        assert!(sw.submit_tracked(9003, Job::Facts));
        wait_for("subsequent facts command", || {
            sw.commands_done() > before && sw.state() == SwitchState::Ready
        })
        .await;
        assert!(sw.take_tracked_result(9003).unwrap().is_ok());
    }

    #[tokio::test]
    async fn refresh_during_copy_uses_a_second_ssh_connection() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn_with_delays(
            log.clone(),
            "s3cret",
            Duration::ZERO,
            Duration::from_secs(2),
        )
        .await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("initial facts and ready state", || {
            sw.facts().version.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        let before = sw.facts().updated.unwrap();
        sw.submit(Job::Copy {
            rel_path: "image.bin".into(),
            command: "copy http://10.0.0.1/image.bin flash:".into(),
            overwrite: false,
            platform_check: false,
        });
        wait_for("copy prompt", || {
            sw.transcript().iter().any(|l| l.text == "<Enter>")
        })
        .await;
        assert!(mgr.refresh_facts(sw.clone()));
        wait_for("facts refreshed while copying", || {
            sw.facts().updated.is_some_and(|t| t > before)
        })
        .await;
        assert_eq!(
            sw.jobs_done(),
            0,
            "refresh waited for the primary console's copy to finish"
        );
        assert!(matches!(sw.state(), SwitchState::Busy { .. }));
        assert_eq!(log.lock().unwrap().connections, 2);
        wait_for("copy completion", || sw.jobs_done() == 1).await;
        assert!(sw.last_result().unwrap().is_ok());
        sw.cancel();
        sw.submit(Job::Disconnect);
    }

    #[test]
    fn deploy_metrics_use_matching_server_bytes_and_current_speed() {
        let sessions = SessionManager::new(600);
        let sw = Switch::for_test(
            "192.0.2.1",
            SwitchState::Ready,
            Facts::default(),
            Vec::new(),
        );
        sw.begin_transfer("image.bin".into(), 10_000_000, Protocol::Http);
        let h = sessions.open(Protocol::Http, "192.0.2.1:50000".parse().unwrap(), 8080);
        sessions.update(h.id, |s| {
            s.file = Some("image.bin".into());
            s.total = Some(10_000_000);
            s.direction = Some(crate::session::Direction::Download);
            s.state = crate::session::SessionState::Transferring;
        });
        std::thread::sleep(Duration::from_millis(60));
        h.add_bytes(5_000_000);
        sessions.sample();
        let other = sessions.open(Protocol::Http, "192.0.2.2:50000".parse().unwrap(), 8080);
        sessions.update(other.id, |s| {
            s.file = Some("image.bin".into());
            s.total = Some(10_000_000);
            s.direction = Some(crate::session::Direction::Download);
            s.state = crate::session::SessionState::Transferring;
        });
        let transfer = sw.transfer(&sessions).unwrap();
        assert_eq!(transfer.size, 10_000_000);
        let stats = transfer.session.unwrap();
        assert_eq!(stats.id, h.id);
        assert_eq!(stats.bytes, 5_000_000);
        assert_eq!(stats.progress(), Some(0.5));
        assert!(stats.current_speed > 0.0);
        assert!(stats.eta().is_some());
        h.add_bytes(5_000_000);
        sessions.finish(h.id, crate::session::SessionState::Completed);
        sw.transfer.lock().unwrap().as_mut().unwrap().ended = Some(Instant::now());
        let stats = sw.transfer(&sessions).unwrap().session.unwrap();
        assert_eq!(stats.bytes, 10_000_000);
        assert_eq!(stats.current_speed, 0.0);
        assert!(stats.eta().is_none());
    }

    #[tokio::test]
    async fn remote_listing_and_mismatch_warning_do_not_prevent_copy_or_reverse_copy() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("ready", || {
            sw.facts().updated.is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        sw.submit(Job::List {
            path: "flash:backups".into(),
        });
        wait_for("listing", || {
            sw.listing("flash:backups").is_some() && sw.state() == SwitchState::Ready
        })
        .await;
        assert!(sw
            .listing("flash:backups")
            .unwrap()
            .unwrap()
            .iter()
            .any(|e| e.name == "packages.conf"));
        assert_eq!(sw.jobs_done(), 0);
        sw.submit(Job::Copy {
            rel_path: "cat9k_iosxe.17.15.06.SPA.bin".into(),
            command: "copy ftp://cisco:cisco123@192.0.2.1/cat9k_iosxe.17.15.06.SPA.bin flash:"
                .into(),
            overwrite: false,
            platform_check: true,
        });
        wait_for("warned copy", || {
            sw.jobs_done() == 1 && sw.state() == SwitchState::Ready
        })
        .await;
        assert!(sw.last_result().unwrap().is_ok());
        assert!(sw
            .transcript()
            .iter()
            .any(|l| l.text.contains("plattform mismatch")));
        let before = sw
            .transcript()
            .iter()
            .filter(|l| l.text.contains("plattform mismatch"))
            .count();
        sw.begin_receive("backups/config.txt".into(), 42);
        let sessions = SessionManager::new(600);
        let session = sessions.open(Protocol::Ftp, "127.0.0.1:55000".parse().unwrap(), 2121);
        sessions.update(session.id, |s| {
            s.file = Some("backups/config.txt".into());
            s.direction = Some(crate::session::Direction::Upload);
            s.state = crate::session::SessionState::Transferring;
        });
        session.add_bytes(42);
        sessions.sample();
        assert_eq!(sw.transfer(&sessions).unwrap().session.unwrap().bytes, 42);
        let reverse =
            "copy flash:backups/config.txt ftp://cisco:cisco123@192.0.2.1/backups/config.txt";
        sw.submit(Job::Copy {
            rel_path: "backups/config.txt".into(),
            command: reverse.into(),
            overwrite: false,
            platform_check: false,
        });
        wait_for("reverse copy", || {
            sw.jobs_done() == 2 && sw.state() == SwitchState::Ready
        })
        .await;
        assert!(sw.last_result().unwrap().is_ok());
        assert_eq!(sw.facts().flash_device, "flash:");
        assert!(log.lock().unwrap().received.iter().any(|l| l == reverse));
        assert_eq!(
            sw.transcript()
                .iter()
                .filter(|l| l.text.contains("plattform mismatch"))
                .count(),
            before
        );
        sw.submit(Job::Disconnect);
    }

    #[tokio::test]
    async fn copies_a_file_and_re_reads_the_free_space() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("the first facts pass", || sw.facts().version.is_some()).await;

        let command = "copy http://10.40.40.56:8080/cat9k_lite_iosxe.17.15.06.SPA.bin flash:";
        assert!(sw.submit(Job::Copy {
            rel_path: "cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
            command: command.into(),
            overwrite: false,
            platform_check: false,
        }));
        wait_for("the copy to finish", || sw.last_result().is_some()).await;

        let summary = sw.last_result().unwrap().expect("copy should succeed");
        assert!(
            summary.contains("504057659 bytes copied in"),
            "summary: {summary}"
        );
        // The session is still open and usable afterwards.
        wait_for("the session to settle", || sw.state() == SwitchState::Ready).await;

        let received = log.lock().unwrap().received.clone();
        assert_eq!(
            received,
            vec![
                "enable",
                "s3cret",
                "terminal length 0",
                "dir flash:",
                "show version",
                command,
                "",           // Enter on "Destination filename [...]?"
                "dir flash:", // free space re-read after the copy
                "show version",
            ]
        );
    }

    #[tokio::test]
    async fn a_forbidden_command_never_reaches_the_device() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("the first facts pass", || sw.facts().version.is_some()).await;
        let before = log.lock().unwrap().received.len();

        sw.submit(Job::Copy {
            rel_path: "x".into(),
            command: "copy http://10.0.0.1/x.bin running-config".into(),
            overwrite: false,
            platform_check: false,
        });
        wait_for("the refusal", || sw.last_result().is_some()).await;

        let err = sw.last_result().unwrap().unwrap_err();
        assert!(err.contains("running-config"), "unexpected error: {err}");
        assert_eq!(
            log.lock().unwrap().received.len(),
            before,
            "nothing may reach the device"
        );
    }

    #[tokio::test]
    async fn a_rejected_host_key_ends_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log, "s3cret").await;
        let mgr = manager();
        let sw = mgr.connect(target(port, dir.path().join("known_hosts")));
        wait_for("the host key question", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;

        sw.answer_host_key(false);
        wait_for("the session to end", || sw.state().is_over()).await;
        match sw.state() {
            SwitchState::Failed { reason } => assert!(reason.contains("host key"), "{reason}"),
            other => panic!("unexpected state: {other:?}"),
        }
        // Nothing was written to known_hosts.
        assert!(!dir.path().join("known_hosts").exists());
    }

    /// Accepting the key has to clear the question immediately. While the
    /// state stayed `HostKey` for the rest of the handshake, the popup kept
    /// showing the question and every further key press was read as another
    /// answer to it — the session looked frozen.
    #[tokio::test]
    async fn accepting_the_host_key_clears_the_question_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        // Slow to greet, so the window between "accepted" and "ready" is real.
        let port = fake_ios::spawn_with_delay(log, "s3cret", Duration::from_millis(1500)).await;
        let mgr = manager();
        let sw = mgr.connect(target(port, dir.path().join("known_hosts")));
        wait_for("the host key question", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;

        sw.answer_host_key(true);
        wait_for("the key to be stored", || {
            sw.transcript()
                .iter()
                .any(|l| l.text.contains("accepted and stored"))
        })
        .await;
        // The device has not greeted yet, so the session cannot be ready —
        // but the question must already be gone.
        assert!(
            !sw.state().is_live(),
            "the device greeted too early for this test"
        );
        assert!(
            !matches!(sw.state(), SwitchState::HostKey { .. }),
            "the host key question outlived the answer: {:?}",
            sw.state()
        );
        wait_for("a ready session", || sw.state().is_live()).await;
    }

    /// Cancelling must work in every state, including while the session is
    /// still being set up or parked on the host key question.
    #[tokio::test]
    async fn cancel_gets_out_of_a_pending_host_key_question() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log, "s3cret").await;
        let mgr = manager();
        let sw = mgr.connect(target(port, dir.path().join("known_hosts")));
        wait_for("the host key question", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;

        sw.cancel();
        wait_for("the session to end", || sw.state().is_over()).await;
        assert!(sw.state().is_over(), "cancel left the session hanging");
    }

    /// A connection nobody answers must not park a session forever.
    #[test]
    fn the_host_key_question_has_a_deadline() {
        assert!(HOST_KEY_TIMEOUT <= Duration::from_secs(300));
        assert!(HOST_KEY_TIMEOUT >= Duration::from_secs(30));
    }

    #[tokio::test]
    async fn a_wrong_password_fails_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log, "s3cret").await;
        let mgr = manager();
        let mut t = target(port, dir.path().join("known_hosts"));
        t.password = "wrong".into();
        let sw = mgr.connect(t);

        wait_for("the host key question", || {
            matches!(sw.state(), SwitchState::HostKey { .. })
        })
        .await;
        sw.answer_host_key(true);
        wait_for("the failure", || {
            matches!(sw.state(), SwitchState::Failed { .. })
        })
        .await;
        match sw.state() {
            SwitchState::Failed { reason } => assert!(reason.contains("login failed"), "{reason}"),
            other => panic!("unexpected state: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_session_is_reused_instead_of_reopened() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        let port = fake_ios::spawn(log, "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;

        assert!(mgr.find_live("127.0.0.1", port, "netadmin").is_some());
        assert!(mgr.find_live("127.0.0.1", port, "someone-else").is_none());
        assert_eq!(mgr.list().len(), 1);

        sw.submit(Job::Disconnect);
        wait_for("the session to close", || sw.state() == SwitchState::Closed).await;
        assert!(mgr.find_live("127.0.0.1", port, "netadmin").is_none());
        mgr.forget_closed();
        assert!(mgr.list().is_empty());
    }
    #[test]
    fn reboot_login_budget_is_shared_and_never_exceeds_three_requests() {
        let budget = LoginBudget::default();
        for _ in 0..3 {
            budget.reserve().unwrap();
        }
        assert!(budget.reserve().is_err());
        assert!(budget.reserve().is_err());
        assert_eq!(budget.used.load(Ordering::Acquire), 3);
    }
    #[tokio::test]
    async fn reboot_stops_after_three_rejected_logins_and_never_repeats_install() {
        let dir = tempfile::tempdir().unwrap();
        let log = fake_ios::Transcript::new();
        log.lock().unwrap().fail_after_reload = 100;
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("initial facts", || {
            sw.state() == SwitchState::Ready && sw.facts().version.is_some()
        })
        .await;
        prepare_upgrade(&sw, dir.path()).await;
        sw.submit(Job::Install { yolo: true });
        wait_for("bounded login failure", || matches!(sw.upgrade(), upgrade::Progress::Failed(ref e) if e.contains("authentication limit"))).await;
        assert_eq!(log.lock().unwrap().auth_failures, 97);
        tokio::time::sleep(Duration::from_millis(350)).await;
        assert_eq!(log.lock().unwrap().auth_failures, 97);
        assert_eq!(
            log.lock()
                .unwrap()
                .received
                .iter()
                .filter(|line| line.starts_with("install add file"))
                .count(),
            1
        );
        sw.cancel();
    }
    #[tokio::test]
    async fn destination_inspection_distinguishes_missing_partial_and_verified_without_copying() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("sample.txt");
        std::fs::write(&local, vec![b'x'; 30]).unwrap();
        let log = fake_ios::Transcript::new();
        log.lock().unwrap().md5 = upgrade::local_md5(&local).unwrap();
        let port = fake_ios::spawn(log.clone(), "s3cret").await;
        let mgr = manager();
        let sw = connected(&mgr, port, dir.path()).await;
        wait_for("facts", || {
            sw.state() == SwitchState::Ready && sw.facts().version.is_some()
        })
        .await;
        for (remote, size, result) in [
            (
                "flash:absent.txt",
                30,
                crate::engine::TransferInspection::Missing,
            ),
            (
                "flash:throughput_monitor_params",
                60,
                crate::engine::TransferInspection::Partial {
                    actual: 30,
                    expected: 60,
                },
            ),
            (
                "flash:throughput_monitor_params",
                30,
                crate::engine::TransferInspection::Verified,
            ),
        ] {
            let before = sw.jobs_done();
            sw.submit(Job::InspectTransfer {
                local_path: local.clone(),
                remote: remote.into(),
                expected_size: size,
                receive: false,
                upgrade_image: false,
            });
            wait_for("inspection", || sw.jobs_done() > before).await;
            assert!(sw.last_result().unwrap().is_ok());
            assert_eq!(sw.take_inspection(), Some(result));
        }
        assert!(!log
            .lock()
            .unwrap()
            .received
            .iter()
            .any(|line| line.starts_with("copy ")));
        sw.cancel();
    }
    #[tokio::test]
    async fn subnet_scan_respects_disabled_automatic_host_key_acceptance() {
        let dir = tempfile::tempdir().unwrap();
        let port = fake_ios::spawn(fake_ios::Transcript::new(), "s3cret").await;
        let mgr = manager();
        let mut target = target(port, dir.path().join("known_hosts"));
        target.auto_trust = true;
        mgr.set_auto_trust(false);
        mgr.start_scan_with_probe(target, "127.0.0.1/32", Protocol::Http, |_| async {
            Ok(true)
        })
        .unwrap();
        wait_for("subnet host key question", || {
            mgr.list()
                .iter()
                .any(|sw| matches!(sw.state(), SwitchState::HostKey { .. }))
        })
        .await;
        for sw in mgr.list() {
            sw.cancel();
        }
    }
}
