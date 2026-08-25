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
use tokio::sync::{mpsc, oneshot};

use crate::cisco::{FlashUsage, VersionInfo};
use crate::deploy::{self, PromptAction, Wire};
use crate::logging::{Event, LogLevel, Logger};

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

/// Everything needed to open a session.
#[derive(Debug, Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub enable_password: String,
    pub known_hosts: PathBuf,
}

/// Work handed to an open session.
#[derive(Debug, Clone)]
pub enum Job {
    /// Re-read `dir` and `show version`.
    Facts,
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
            Job::Copy { rel_path, .. } => format!("copying {rel_path}"),
            Job::Disconnect => "disconnecting".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchState {
    Connecting,
    /// Waiting for the user to accept an unknown host key.
    HostKey { fingerprint: String },
    /// Logged in, privileged, idle.
    Ready,
    Busy { what: String },
    /// The session is gone but the device may come back — this is the state a
    /// reload leaves behind.
    Offline { reason: String },
    /// The session never came up.
    Failed { reason: String },
    /// Closed on purpose.
    Closed,
}

impl SwitchState {
    pub fn label(&self) -> &'static str {
        match self {
            SwitchState::Connecting => "connecting",
            SwitchState::HostKey { .. } => "host key?",
            SwitchState::Ready => "ready",
            SwitchState::Busy { .. } => "busy",
            SwitchState::Offline { .. } => "offline",
            SwitchState::Failed { .. } => "failed",
            SwitchState::Closed => "closed",
        }
    }
    pub fn is_live(&self) -> bool {
        matches!(self, SwitchState::Ready | SwitchState::Busy { .. })
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

/// One open session to one device.
pub struct Switch {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub opened: Instant,
    state: Mutex<SwitchState>,
    facts: Mutex<Facts>,
    transcript: Mutex<VecDeque<Line>>,
    /// The device's current, not yet newline-terminated output.
    live: Mutex<String>,
    reach: Mutex<Reach>,
    /// Result of the last finished job, for the UI.
    last_result: Mutex<Option<std::result::Result<String, String>>>,
    host_key_reply: Mutex<Option<oneshot::Sender<bool>>>,
    /// Number of finished jobs, so the UI can announce each one once.
    jobs_done: AtomicU64,
    jobs: mpsc::UnboundedSender<Job>,
    cancel: Arc<AtomicBool>,
}

impl Switch {
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
    }

    /// Answer the pending host key question.
    pub fn answer_host_key(&self, accept: bool) {
        if let Some(tx) = self.host_key_reply.lock().unwrap().take() {
            let _ = tx.send(accept);
        }
    }

    /// A detached session with an open host key question, plus the receiver
    /// the answer arrives on.
    #[cfg(test)]
    pub fn for_test_host_key(fingerprint: &str) -> (Arc<Self>, oneshot::Receiver<bool>) {
        let switch = Self::for_test(
            "10.20.30.40",
            SwitchState::HostKey { fingerprint: fingerprint.to_string() },
            Facts::default(),
            Vec::new(),
        );
        let (tx, rx) = oneshot::channel();
        *switch.host_key_reply.lock().unwrap() = Some(tx);
        (switch, rx)
    }

    /// A detached session with a fixed state, for rendering tests.
    #[cfg(test)]
    pub fn for_test(host: &str, state: SwitchState, facts: Facts, transcript: Vec<Line>) -> Arc<Self> {
        let (tx, _rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            id: 1,
            host: host.to_string(),
            port: 22,
            username: "netadmin".into(),
            opened: Instant::now(),
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
        t.push_back(Line { kind, text: text.into() });
        while t.len() > TRANSCRIPT_MAX {
            t.pop_front();
        }
    }
}

/// Every open session, plus the runtime to drive them.
pub struct SwitchManager {
    switches: Mutex<Vec<Arc<Switch>>>,
    runtime: tokio::runtime::Handle,
    logger: Arc<Logger>,
    next_id: AtomicU64,
}

impl SwitchManager {
    pub fn new(runtime: tokio::runtime::Handle, logger: Arc<Logger>) -> Self {
        Self {
            switches: Mutex::new(Vec::new()),
            runtime,
            logger,
            next_id: AtomicU64::new(1),
        }
    }

    pub fn list(&self) -> Vec<Arc<Switch>> {
        self.switches.lock().unwrap().clone()
    }

    pub fn get(&self, id: u64) -> Option<Arc<Switch>> {
        self.switches.lock().unwrap().iter().find(|s| s.id == id).cloned()
    }

    /// An existing, usable session for the same device and user.
    pub fn find_live(&self, host: &str, port: u16, username: &str) -> Option<Arc<Switch>> {
        self.switches
            .lock()
            .unwrap()
            .iter()
            .find(|s| {
                s.host == host && s.port == port && s.username == username && !s.state().is_over()
            })
            .cloned()
    }

    /// Drop finished sessions from the list.
    pub fn forget_closed(&self) {
        self.switches.lock().unwrap().retain(|s| !s.state().is_over());
    }

    pub fn forget(&self, id: u64) {
        self.switches.lock().unwrap().retain(|s| s.id != id);
    }

    pub fn disconnect_all(&self) {
        for sw in self.list() {
            sw.submit(Job::Disconnect);
        }
    }

    /// Open a session and start the driver and the ping monitor.
    pub fn connect(&self, target: Target) -> Arc<Switch> {
        let (tx, rx) = mpsc::unbounded_channel();
        let switch = Arc::new(Switch {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            host: target.host.clone(),
            port: target.port,
            username: target.username.clone(),
            opened: Instant::now(),
            state: Mutex::new(SwitchState::Connecting),
            facts: Mutex::new(Facts { flash_device: "flash:".into(), ..Facts::default() }),
            transcript: Mutex::new(VecDeque::new()),
            live: Mutex::new(String::new()),
            reach: Mutex::new(Reach::default()),
            last_result: Mutex::new(None),
            host_key_reply: Mutex::new(None),
            jobs_done: AtomicU64::new(0),
            jobs: tx,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        self.switches.lock().unwrap().push(switch.clone());

        let logger = self.logger.clone();
        self.runtime.spawn(drive(switch.clone(), target, rx, logger));
        self.runtime.spawn(monitor_reachability(switch.clone()));
        switch
    }
}

// ---------------------------------------------------------------- driver

async fn drive(
    switch: Arc<Switch>,
    target: Target,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    logger: Arc<Logger>,
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

    switch.push(LineKind::Info, format!("connecting to {}:{}", target.host, target.port));
    // Connecting must stay abortable: a device that never answers, or a host
    // key question nobody wants to answer, must not park the session forever.
    let opened = tokio::select! {
        result = open_shell(&switch, &target) => result,
        _ = cancelled(&switch) => Err(anyhow!("cancelled before the session was up")),
    };
    let mut shell = match opened {
        Ok(shell) => shell,
        Err(e) => {
            let reason = format!("{e:#}");
            switch.push(LineKind::Error, reason.clone());
            switch.set_state(SwitchState::Failed { reason: reason.clone() });
            log(LogLevel::Error, format!("connect to {} failed", target.host), Some(reason));
            return;
        }
    };
    switch.set_state(SwitchState::Ready);
    log(LogLevel::Info, format!("connected to {}", switch.display_name()), None);

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
            log(LogLevel::Info, format!("disconnected from {}", switch.display_name()), None);
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
            *switch.last_result.lock().unwrap() =
                Some(outcome.as_ref().map(|s| s.clone()).map_err(|e| format!("{e:#}")));
            switch.jobs_done.fetch_add(1, Ordering::Relaxed);
        }

        // A broken session cannot be reused — say so instead of looking idle.
        if let Err(e) = &outcome {
            let text = format!("{e:#}");
            if text.contains("closed the session") || text.contains("cancelled") {
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
        Job::Copy { command, overwrite, .. } => {
            let summary = run_copy(shell, command, *overwrite).await?;
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
    let listing = shell.run_command(&format!("dir {device}"), STEP_TIMEOUT).await?;
    let usage = crate::cisco::parse_dir_totals(&listing);

    let version_output = shell.run_command("show version", STEP_TIMEOUT).await?;
    let version = crate::cisco::parse_show_version(&version_output);

    let mut facts = switch.facts.lock().unwrap();
    if let Some(usage) = usage {
        facts.flash = Some(usage);
    }
    if version.version.is_some() || !version.members.is_empty() {
        facts.version = Some(version);
    }
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
        if switch.state().is_over() && !matches!(switch.state(), SwitchState::Offline { .. }) {
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
    let output = tokio::process::Command::new("ping")
        .args(["-n", "-c", "1", "-t", "2", host])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_ping_rtt(&String::from_utf8_lossy(&output.stdout))
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
/// recorded in transferbuddy's own `known_hosts`. A key that changed is
/// refused — that is the case a known-hosts file exists for.
struct ClientHandler {
    switch: Arc<Switch>,
    known_hosts: PathBuf,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = anyhow::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh_keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let fingerprint = format!("SHA256:{}", server_public_key.fingerprint());
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

        let (tx, rx) = oneshot::channel();
        *self.switch.host_key_reply.lock().unwrap() = Some(tx);
        self.switch
            .set_state(SwitchState::HostKey { fingerprint: fingerprint.clone() });
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
        russh_keys::known_hosts::learn_known_hosts_path(
            &self.switch.host,
            self.switch.port,
            server_public_key,
            &self.known_hosts,
        )
        .map_err(|e| anyhow!("cannot store the host key: {e}"))?;
        self.switch.push(
            LineKind::Info,
            format!("host key {fingerprint} accepted and stored"),
        );
        Ok(true)
    }
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
    if lower.ends_with("password:") || lower.ends_with("passphrase:") {
        return Some(Signal::Password);
    }
    if t.ends_with('?') || lower.ends_with("[confirm]") {
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
        self.switch.push(LineKind::Sent, shown);
        let line = format!("{text}\r");
        self.channel.data(line.as_bytes()).await?;
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
                    self.switch.push(LineKind::Output, line.trim_end().to_string());
                }
                '\r' => {}
                // Backspace: IOS redraws its line this way.
                '\u{8}' => {
                    self.tail.pop();
                }
                c => self.tail.push(c),
            }
        }
        *self.switch.live.lock().unwrap() = self.tail.clone();
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
async fn open_shell(switch: &Arc<Switch>, target: &Target) -> Result<Shell> {
    let handler = ClientHandler {
        switch: switch.clone(),
        known_hosts: target.known_hosts.clone(),
    };
    let mut session = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client::connect(
            Arc::new(client_config()),
            (target.host.as_str(), target.port),
            handler,
        ),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "no answer from {}:{} within {}s",
            target.host,
            target.port,
            CONNECT_TIMEOUT.as_secs()
        )
    })??;

    match authenticate(&mut session, target).await {
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

    switch.push(LineKind::Info, format!("authenticating as {}", target.username));
    let channel = session.channel_open_session().await?;
    channel.request_pty(true, "vt100", 200, 48, 0, 0, &[]).await?;
    channel.request_shell(true).await?;
    switch.push(LineKind::Info, "waiting for the device prompt");

    let mut shell = Shell {
        channel,
        switch: switch.clone(),
        _session: session,
        tail: String::new(),
        collected: String::new(),
        hostname: None,
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
        assert_eq!(tail_signal("cat9k-1#"), Some(Signal::PromptEnabled("cat9k-1#".into())));
        assert_eq!(tail_signal("Switch>"), Some(Signal::PromptUser("Switch>".into())));
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
        }

        impl Transcript {
            pub fn new() -> Arc<Mutex<Self>> {
                Arc::new(Mutex::new(Self { received: Vec::new() }))
            }
        }

        pub struct Device {
            log: Arc<Mutex<Transcript>>,
            enable_password: String,
            /// Delay before the first prompt, to make the window between
            /// "host key accepted" and "session ready" observable.
            greet_delay: std::time::Duration,
            line: String,
            enabled: bool,
            expect_enable_password: bool,
            in_copy: bool,
            channels: HashMap<ChannelId, Channel<Msg>>,
        }

        impl Device {
            fn say(&self, session: &mut Session, id: ChannelId, text: &str) {
                let _ = session.data(id, CryptoVec::from_slice(text.as_bytes()));
            }

            fn prompt(&self) -> &'static str {
                if self.enabled { "\r\ncat9k-1#" } else { "\r\ncat9k-1>" }
            }

            fn on_line(&mut self, session: &mut Session, id: ChannelId, line: &str) {
                self.log.lock().unwrap().received.push(line.to_string());

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

            async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
                if user == "netadmin" && password == "letmein" {
                    Ok(Auth::Accept)
                } else {
                    Ok(Auth::Reject { proceed_with_methods: None })
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
                        line: String::new(),
                        enabled: false,
                        expect_enable_password: false,
                        in_copy: false,
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
            vec!["enable", "s3cret", "terminal length 0", "dir flash:", "show version"]
        );
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
        assert!(summary.contains("504057659 bytes copied in"), "summary: {summary}");
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
                "",              // Enter on "Destination filename [...]?"
                "dir flash:",    // free space re-read after the copy
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
        let port =
            fake_ios::spawn_with_delay(log, "s3cret", Duration::from_millis(1500)).await;
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
        assert!(!sw.state().is_live(), "the device greeted too early for this test");
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
