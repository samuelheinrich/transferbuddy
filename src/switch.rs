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
    /// Bulk import and subnet discovery explicitly trust replacement keys.
    pub auto_trust: bool,
}

/// Work handed to an open session.
#[derive(Debug, Clone)]
pub enum Job {
    /// Re-read `dir` and `show version`.
    Facts,
    RemoveInactive,
    /// Let the device pull one file.
    Copy {
        rel_path: String,
        command: String,
        overwrite: bool,
    },
    /// `exit` and close the session.
    Disconnect,
}

impl Job {
    fn label(&self) -> String {
        match self {
            Job::Facts => "reading device facts".into(),
            Job::RemoveInactive => "install remove inactive".into(),
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
            SwitchState::Ready | SwitchState::Busy { .. } | SwitchState::CleanupConfirm { .. }
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
    target: Option<Target>,
    protocol: Mutex<Protocol>,
    transfer: Mutex<Option<Transfer>>,
    peer_ips: Mutex<Vec<std::net::IpAddr>>,
    refreshing: AtomicBool,
    state: Mutex<SwitchState>,
    facts: Mutex<Facts>,
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
    jobs: mpsc::UnboundedSender<Job>,
    cancel: Arc<AtomicBool>,
}

impl Switch {
    pub fn protocol(&self) -> Protocol {
        *self.protocol.lock().unwrap()
    }
    pub fn set_protocol(&self, protocol: Protocol) {
        *self.protocol.lock().unwrap() = protocol;
    }
    pub fn begin_transfer(&self, rel_path: String, size: u64, protocol: Protocol) {
        self.set_protocol(protocol);
        *self.transfer.lock().unwrap() = Some(Transfer {
            rel_path,
            size,
            protocol,
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
                    && s.direction == Some(crate::session::Direction::Download)
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
        self.jobs.send(job).is_ok()
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
    }

    /// Answer the pending host key question.
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
    #[cfg(test)]
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

    #[cfg(test)]
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

    /// A detached session with a fixed state, for rendering tests.
    #[cfg(test)]
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
            target: None,
            protocol: Mutex::new(Protocol::Http),
            transfer: Mutex::new(None),
            peer_ips: Mutex::new(Vec::new()),
            refreshing: AtomicBool::new(false),
            state: Mutex::new(state),
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
            jobs: tx,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Adjust the facts of a detached test session.
    #[cfg(test)]
    pub fn facts_for_test(&self, f: impl FnOnce(&mut Facts)) {
        f(&mut self.facts.lock().unwrap());
    }

    fn set_state(&self, state: SwitchState) {
        *self.state.lock().unwrap() = state;
    }

    fn push(&self, kind: LineKind, text: impl Into<String>) {
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
}

impl SwitchManager {
    pub fn scan(&self) -> Option<Arc<SubnetScan>> {
        self.scan.lock().unwrap().clone()
    }

    pub fn start_scan(&self, target: Target, cidr: &str, protocol: Protocol) -> Result<(), String> {
        self.start_scan_with_probe(target, cidr, protocol, |host| async move {
            ping_probe(&host)
                .await
                .map(|rtt| rtt.is_some())
                .map_err(|e| e.to_string())
        })
    }

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
        let hosts = subnet_hosts(cidr)?;
        let mut current = self.scan.lock().unwrap();
        if current.as_ref().is_some_and(|s| !s.progress().finished) {
            return Err("a subnet scan is already running (S cancels it)".into());
        }
        let scan = Arc::new(SubnetScan {
            progress: Mutex::new(ScanProgress {
                subnet: cidr.trim().into(),
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
                target.auto_trust = true;
                manager.connect(target).set_protocol(protocol);
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
        }
    }

    pub fn list(&self) -> Vec<Arc<Switch>> {
        self.switches.lock().unwrap().clone()
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
                    && s.username == username
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
        let Some(target) = switch.target.clone() else {
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

    /// Open a session and start the driver and the ping monitor.
    pub fn connect(&self, target: Target) -> Arc<Switch> {
        let mut switches = self.switches.lock().unwrap();
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
            target: Some(target.clone()),
            protocol: Mutex::new(Protocol::Http),
            transfer: Mutex::new(None),
            peer_ips: Mutex::new(Vec::new()),
            refreshing: AtomicBool::new(false),
            state: Mutex::new(SwitchState::Connecting),
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
            jobs: tx,
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
        self.runtime.spawn(monitor_reachability(switch.clone()));
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
        let mut ev = Event::new(level, "switch", action);
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
        let outcome = run_job(&switch, &mut shell, &job).await;
        match &outcome {
            Ok(summary) => {
                if !summary.is_empty() {
                    switch.push(LineKind::Info, summary.clone());
                }
                if !matches!(job, Job::Facts) {
                    log(LogLevel::Info, job.label(), Some(summary.clone()));
                }
            }
            Err(e) => {
                let reason = format!("{e:#}");
                switch.push(LineKind::Error, reason.clone());
                log(LogLevel::Error, job.label(), Some(reason));
            }
        }
        if !matches!(job, Job::Facts) {
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
            if text.contains("closed the session")
                || text.contains("cancelled")
                || matches!(job, Job::RemoveInactive)
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

async fn run_job(switch: &Arc<Switch>, shell: &mut Shell, job: &Job) -> Result<String> {
    match job {
        Job::Facts => {
            collect_facts(switch, shell).await?;
            Ok(String::new())
        }
        Job::RemoveInactive => {
            collect_facts(switch, shell).await?;
            let summary = run_remove_inactive(switch, shell).await?;
            collect_facts(switch, shell).await?;
            Ok(summary)
        }
        Job::Copy {
            command, overwrite, ..
        } => {
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

/// `copy <url> flash:img.bin` -> `flash:`
fn copy_destination_device(command: &str) -> Option<String> {
    let dst = command.split_whitespace().nth(2)?;
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
async fn monitor_reachability(switch: Arc<Switch>) {
    loop {
        let state = switch.state();
        if state.is_over()
            && (!matches!(state, SwitchState::Offline { .. }) || switch.cancel_requested())
        {
            return;
        }
        let rtt = ping_once(&switch.host).await;
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
        if self.auto_trust {
            store_host_key(
                &self.switch.host,
                self.switch.port,
                server_public_key,
                &self.known_hosts,
                true,
            )?;
            self.switch.push(LineKind::Info, format!("bulk import: host key {fingerprint} automatically trusted and stored (replacement keys allowed)"));
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
    if lower.contains("do you want to remove")
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
        let text = String::from_utf8_lossy(data);
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
async fn open_shell(switch: &Arc<Switch>, target: &Target, record_output: bool) -> Result<Shell> {
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
            result = &mut connection => break result?,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if !waiting_for_key && !matches!(switch.state(), SwitchState::HostKey { .. }) { elapsed += started.elapsed(); }
                if elapsed >= CONNECT_TIMEOUT { bail!("no answer from {}:{} within {}s", target.host, target.port, CONNECT_TIMEOUT.as_secs()); }
            }
        }
    };

    match tokio::time::timeout(STEP_TIMEOUT, authenticate(&mut session, target))
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
    };

    let mut signal = shell.expect(STEP_TIMEOUT).await?;
    // Some devices print a banner and ask for a login password on the line
    // before the prompt appears.
    if signal == Signal::Password {
        shell.send(&target.password, Wire::Secret).await?;
        signal = shell.expect(STEP_TIMEOUT).await?;
    }

    if matches!(signal, Signal::PromptUser(_)) {
        if target.enable_password.is_empty() {
            bail!(
                "the device is in user EXEC mode (>) and copy needs privileged EXEC — \
                 enter an enable password"
            );
        }
        shell.send("enable", Wire::Command).await?;
        signal = shell.expect(STEP_TIMEOUT).await?;
        if signal == Signal::Password {
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
    shell.take_output();
    shell.send(command, Wire::Command).await?;
    let deadline = Instant::now() + COPY_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("copy did not finish within {}s", COPY_TIMEOUT.as_secs());
        }
        match shell.expect(remaining).await? {
            Signal::Question(q) => match deploy::classify_prompt(&q, overwrite) {
                PromptAction::Accept => shell.send("", Wire::Answer).await?,
                PromptAction::Decline => shell.send("n", Wire::Answer).await?,
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

/// Password first, keyboard-interactive second — Cisco devices offer one or
/// the other depending on how the vty lines are configured.
async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    target: &Target,
) -> Result<bool> {
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

        const SHOW_VERSION: &str = include_str!("../testdata/show_version_c9200l.txt");
        const DIR_FLASH: &str = include_str!("../testdata/dir_flash.txt");

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
        }

        impl Transcript {
            pub fn new() -> Arc<Mutex<Self>> {
                Arc::new(Mutex::new(Self {
                    received: Vec::new(),
                    connections: 0,
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
            channels: HashMap<ChannelId, Channel<Msg>>,
        }

        impl Device {
            fn say(&self, session: &mut Session, id: ChannelId, text: &str) {
                let _ = session.data(id, CryptoVec::from_slice(text.as_bytes()));
            }

            fn prompt(&self) -> &'static str {
                if self.enabled {
                    "\r\ncat9k-1#"
                } else {
                    "\r\ncat9k-1>"
                }
            }

            fn on_line(&mut self, session: &mut Session, id: ChannelId, line: &str) {
                self.log.lock().unwrap().received.push(line.to_string());
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
                        self.expect_enable_password = true;
                        self.say(session, id, "\r\nPassword: ");
                    }
                    "terminal length 0" => {
                        let p = self.prompt();
                        self.say(session, id, p);
                    }
                    "show version" => {
                        let text = format!("\r\n{}\r\ncat9k-1#", body(SHOW_VERSION));
                        self.say(session, id, &text);
                    }
                    l if l.starts_with("dir ") || l == "dir" => {
                        let text = format!("\r\n{}\r\ncat9k-1#", body(DIR_FLASH));
                        self.say(session, id, &text);
                    }
                    "exit" => {
                        session.eof(id);
                        session.close(id);
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
            let config = Arc::new(russh::server::Config {
                methods: MethodSet::PASSWORD,
                keys: vec![russh_keys::key::KeyPair::generate_ed25519()],
                ..Default::default()
            });
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
}
