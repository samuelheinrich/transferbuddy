//! Deploy: open an SSH session to a Cisco device and let the device pull one
//! file from transferbuddy.
//!
//! The hard rule of this module is the command whitelist. transferbuddy types
//! exactly four kinds of line on a device — `terminal length 0`, `enable`, one
//! `copy <url> <device>:` and `exit` — plus the answers `<Enter>` and `n` to
//! the prompts `copy` asks. Everything that is written to the wire goes
//! through [`vet`]; a configuration command, a `reload`, a `write`, a `delete`
//! or a copy into `running-config` cannot pass it. See [`check_command`] and
//! [`check_destination`].

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use russh::client::{self, KeyboardInteractiveAuthResponse};
use russh::ChannelMsg;
use tokio::sync::{mpsc, oneshot};

/// Exact commands that need no arguments. `copy` is checked separately.
const ALLOWED_COMMANDS: &[&str] = &["terminal length 0", "enable", "exit"];

/// URL schemes a `copy` source may use — all of them served by transferbuddy.
const ALLOWED_SCHEMES: &[&str] =
    &["ftp://", "http://", "https://", "scp://", "sftp://", "tftp://"];

/// Destinations that would change the device's configuration or state rather
/// than write a file. Matched anywhere in the destination.
const FORBIDDEN_DESTINATIONS: &[&str] = &[
    "running-config",
    "startup-config",
    "vlan.dat",
    "nvram:",
    "system:",
    "tmpsys:",
    "null:",
    "cns:",
    "pram:",
    "rcp:",
    "archive:",
];

/// Storage devices a file may be copied to.
const ALLOWED_DEVICES: &[&str] = &[
    "flash",
    "bootflash",
    "usbflash",
    "disk",
    "slot",
    "crashinfo",
    "harddisk",
    "webui",
    "sd",
    "msata",
    "obfl",
    "stby-flash",
    "stby-bootflash",
    "stby-usbflash",
    "stby-disk",
    "stby-harddisk",
];

/// What kind of line is being written to the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    /// A whitelisted EXEC command.
    Command,
    /// An answer to a prompt: Enter (accept the default) or `n` (decline).
    Answer,
    /// A password. Never echoed into the transcript or the log.
    Secret,
}

/// The single choke point: nothing reaches the device without passing here.
pub fn vet(text: &str, wire: Wire) -> Result<(), String> {
    if text.contains('\n') || text.contains('\r') {
        return Err("refused: line breaks are not allowed in a single line".into());
    }
    match wire {
        Wire::Command => check_command(text),
        Wire::Answer => {
            if text.is_empty() || text == "n" {
                Ok(())
            } else {
                Err(format!("refused: {text:?} is not an allowed prompt answer"))
            }
        }
        // Only ever sent in reply to a `Password:` prompt, where IOS reads a
        // secret and not a command. The line-break check above still applies.
        Wire::Secret => Ok(()),
    }
}

/// Whitelist for EXEC commands. Anything not listed here is refused — that
/// includes every configuration command, `reload`, `write`, `erase`,
/// `delete` and `format`.
pub fn check_command(cmd: &str) -> Result<(), String> {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return Err("refused: empty command".into());
    }
    if cmd.chars().any(|c| c.is_control()) {
        return Err("refused: command contains control characters".into());
    }
    // `?` is IOS context help and would leave the session at a half-typed
    // command; `|` and `;` chain further commands.
    if cmd.contains('?') || cmd.contains('|') || cmd.contains(';') {
        return Err(format!("refused: {cmd:?} contains a forbidden character"));
    }
    if ALLOWED_COMMANDS.contains(&cmd) {
        return Ok(());
    }
    match cmd.strip_prefix("copy ") {
        Some(rest) => check_copy(rest),
        None => Err(format!(
            "refused: {cmd:?} is not a command transferbuddy is allowed to run"
        )),
    }
}

fn check_copy(rest: &str) -> Result<(), String> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() != 2 {
        return Err("refused: copy takes exactly one source URL and one destination".into());
    }
    let (src, dst) = (parts[0], parts[1]);
    let lower = src.to_ascii_lowercase();
    if !ALLOWED_SCHEMES.iter().any(|s| lower.starts_with(s)) {
        return Err(format!(
            "refused: copy source {src:?} is not a transferbuddy URL"
        ));
    }
    check_destination(dst)
}

/// A copy destination must be a storage device (`flash:`, `bootflash:`,
/// `usbflash0:`, ...). Configuration targets are refused: copying into
/// `running-config` or `startup-config` is a configuration change.
pub fn check_destination(dst: &str) -> Result<(), String> {
    let lower = dst.to_ascii_lowercase();
    if let Some(bad) = FORBIDDEN_DESTINATIONS.iter().find(|b| lower.contains(**b)) {
        return Err(format!(
            "refused: {dst:?} targets {bad} — transferbuddy never writes the device configuration"
        ));
    }
    let Some((device, path)) = lower.split_once(':') else {
        return Err(format!(
            "refused: {dst:?} does not name a device — use e.g. flash:"
        ));
    };
    if !device_allowed(device) {
        return Err(format!("refused: {device}: is not a known storage device"));
    }
    if path.contains(':') || path.contains(' ') {
        return Err(format!("refused: {dst:?} is not a valid destination path"));
    }
    Ok(())
}

/// `flash`, `flash-1`, `usbflash0`, `disk0`, `stby-bootflash`, ...
fn device_allowed(device: &str) -> bool {
    if device.is_empty() || device.contains('/') {
        return false;
    }
    let base = device.trim_end_matches(|c: char| c.is_ascii_digit());
    let base = base.strip_suffix('-').unwrap_or(base);
    ALLOWED_DEVICES.contains(&base)
}

/// What to do about a prompt the device printed while `copy` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptAction {
    /// Send Enter — accept the default the device offers.
    Accept,
    /// Send `n` — decline.
    Decline,
    /// Something transferbuddy will not answer; the session is torn down.
    Abort(String),
}

/// Decide how to answer one prompt. Only the handful of questions a plain
/// `copy` asks are answered; anything else aborts the session, so a prompt
/// nobody anticipated can never be confirmed by accident.
pub fn classify_prompt(prompt: &str, overwrite: bool) -> PromptAction {
    let p = prompt.trim();
    let lower = p.to_ascii_lowercase();

    // Never erase a filesystem, whatever the wording.
    if lower.contains("erase") || lower.contains("format") || lower.contains("squeeze") {
        return PromptAction::Decline;
    }
    if lower.contains("over write") || lower.contains("overwrite") {
        return if overwrite { PromptAction::Accept } else { PromptAction::Decline };
    }
    for known in [
        "destination filename",
        "source filename",
        "address or name of remote host",
        "source username",
        "source password",
    ] {
        if lower.starts_with(known) {
            return PromptAction::Accept;
        }
    }
    PromptAction::Abort(format!("unexpected prompt from the device: {p:?}"))
}

/// Everything one deploy needs. Built by the TUI, consumed by [`run`].
#[derive(Debug, Clone)]
pub struct DeployRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub enable_password: String,
    /// The full, already validated `copy <url> <device>:` command.
    pub command: String,
    /// Overwrite an existing file of the same name on the device.
    pub overwrite: bool,
    pub known_hosts: PathBuf,
    pub connect_timeout: Duration,
    pub step_timeout: Duration,
    pub copy_timeout: Duration,
}

/// Progress reported back to the TUI.
#[derive(Debug)]
pub enum DeployEvent {
    /// Coarse phase, shown in the modal header.
    Stage(String),
    /// A line transferbuddy sent (passwords already masked).
    Sent(String),
    /// A finished output line from the device.
    Output(String),
    /// The device's current, not yet newline-terminated output — progress
    /// bangs of a running `copy` arrive this way.
    Live(String),
    /// First contact with this host key. A key that no longer matches is not
    /// offered for confirmation — it fails the deploy outright.
    HostKey {
        fingerprint: String,
        reply: oneshot::Sender<bool>,
    },
    /// Terminal event; exactly one is sent per deploy.
    Finished(std::result::Result<String, String>),
}

/// Cisco's SSH implementations are conservative: the modern defaults of
/// russh alone do not reach an IOS device, so the legacy algorithms are
/// offered as well (last, so a modern device still negotiates modern ones).
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

/// Trust on first use: an unknown key is shown to the user and, once
/// accepted, recorded in transferbuddy's own `known_hosts`. A key that
/// changed is refused — that is the case a known-hosts file exists for.
struct ClientHandler {
    host: String,
    port: u16,
    known_hosts: PathBuf,
    tx: mpsc::UnboundedSender<DeployEvent>,
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
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts,
        ) {
            Ok(true) => {
                let _ = self.tx.send(DeployEvent::Output(format!(
                    "host key {fingerprint} is known"
                )));
                return Ok(true);
            }
            Ok(false) => {}
            Err(russh_keys::Error::KeyChanged { line }) => {
                bail!(
                    "the host key of {} changed (known_hosts line {}). Refusing to connect — \
                     if the device was replaced or re-imaged, remove that line from {}",
                    self.host,
                    line,
                    self.known_hosts.display()
                );
            }
            Err(e) => bail!("cannot read {}: {e}", self.known_hosts.display()),
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(DeployEvent::HostKey { fingerprint: fingerprint.clone(), reply: reply_tx })
            .map_err(|_| anyhow!("deploy view closed"))?;
        if !reply_rx.await.unwrap_or(false) {
            bail!("host key rejected");
        }
        russh_keys::known_hosts::learn_known_hosts_path(
            &self.host,
            self.port,
            server_public_key,
            &self.known_hosts,
        )
        .map_err(|e| anyhow!("cannot store the host key: {e}"))?;
        let _ = self.tx.send(DeployEvent::Output(format!(
            "host key {fingerprint} accepted and stored"
        )));
        Ok(true)
    }
}

/// What the device is currently waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Signal {
    /// Privileged EXEC prompt (`Switch#`).
    PromptEnabled,
    /// User EXEC prompt (`Switch>`).
    PromptUser,
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
        return Some(if t.ends_with('#') { Signal::PromptEnabled } else { Signal::PromptUser });
    }
    None
}

/// The interactive shell on the device.
struct Shell {
    channel: russh::Channel<client::Msg>,
    tx: mpsc::UnboundedSender<DeployEvent>,
    cancel: Arc<AtomicBool>,
    /// Output since the last newline — where prompts live.
    tail: String,
    /// Everything the device printed since the last [`Shell::take_output`].
    collected: String,
}

impl Shell {
    fn emit(&self, ev: DeployEvent) {
        let _ = self.tx.send(ev);
    }

    /// Write one line to the device, after [`vet`] approved it.
    async fn send(&mut self, text: &str, wire: Wire) -> Result<()> {
        vet(text, wire).map_err(|e| anyhow!(e))?;
        let shown = match wire {
            Wire::Secret => "********".to_string(),
            Wire::Answer if text.is_empty() => "<Enter>".to_string(),
            _ => text.to_string(),
        };
        self.emit(DeployEvent::Sent(shown));
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
                    let line = line.trim_end().to_string();
                    self.emit(DeployEvent::Output(line));
                }
                '\r' => {}
                // Backspace: IOS redraws the `--More--` line this way.
                '\u{8}' => {
                    self.tail.pop();
                }
                c => self.tail.push(c),
            }
        }
        self.emit(DeployEvent::Live(self.tail.clone()));
    }

    /// Read until the device asks for something, with a deadline.
    async fn expect(&mut self, timeout: Duration) -> Result<Signal> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.cancel.load(Ordering::Relaxed) {
                bail!("cancelled");
            }
            if Instant::now() >= deadline {
                bail!("the device did not answer within {}s", timeout.as_secs());
            }
            let slice = Duration::from_millis(250);
            match tokio::time::timeout(slice, self.channel.wait()).await {
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
                return Ok(sig);
            }
        }
    }

    fn take_output(&mut self) -> String {
        std::mem::take(&mut self.collected)
    }
}

/// Run one deploy. Exactly one [`DeployEvent::Finished`] is sent, whatever
/// happens — the TUI relies on it to leave the "running" state.
pub async fn run(
    req: DeployRequest,
    tx: mpsc::UnboundedSender<DeployEvent>,
    cancel: Arc<AtomicBool>,
) {
    let result = run_inner(&req, &tx, &cancel).await;
    let _ = tx.send(DeployEvent::Finished(match result {
        Ok(summary) => Ok(summary),
        Err(e) => Err(format!("{e:#}")),
    }));
}

async fn run_inner(
    req: &DeployRequest,
    tx: &mpsc::UnboundedSender<DeployEvent>,
    cancel: &Arc<AtomicBool>,
) -> Result<String> {
    // Refuse before opening a socket if the command is not one we may run.
    check_command(&req.command).map_err(|e| anyhow!(e))?;

    let emit = |ev: DeployEvent| {
        let _ = tx.send(ev);
    };
    emit(DeployEvent::Stage(format!("connecting to {}:{}", req.host, req.port)));

    let handler = ClientHandler {
        host: req.host.clone(),
        port: req.port,
        known_hosts: req.known_hosts.clone(),
        tx: tx.clone(),
    };
    let mut session = tokio::time::timeout(
        req.connect_timeout,
        client::connect(
            Arc::new(client_config()),
            (req.host.as_str(), req.port),
            handler,
        ),
    )
    .await
    .map_err(|_| anyhow!("no answer from {}:{} within {}s", req.host, req.port, req.connect_timeout.as_secs()))??;

    emit(DeployEvent::Stage(format!("authenticating as {}", req.username)));
    match authenticate(&mut session, req).await {
        Ok(true) => {}
        Ok(false) => bail!(
            "login failed for user {} — wrong username or password?",
            req.username
        ),
        // Devices that drop the connection after a bad password surface as a
        // transport error rather than a clean rejection.
        Err(e) => bail!(
            "login failed for user {} — wrong username or password? ({e})",
            req.username
        ),
    }

    let channel = session.channel_open_session().await?;
    channel.request_pty(true, "vt100", 200, 48, 0, 0, &[]).await?;
    channel.request_shell(true).await?;

    let mut shell = Shell {
        channel,
        tx: tx.clone(),
        cancel: cancel.clone(),
        tail: String::new(),
        collected: String::new(),
    };

    emit(DeployEvent::Stage("waiting for the device prompt".into()));
    let mut signal = shell.expect(req.step_timeout).await?;
    // Some devices print a banner and ask for a login password on the line
    // before the prompt appears.
    if signal == Signal::Password {
        shell.send(&req.password, Wire::Secret).await?;
        signal = shell.expect(req.step_timeout).await?;
    }

    if signal == Signal::PromptUser {
        if req.enable_password.is_empty() {
            bail!(
                "the device is in user EXEC mode (>) and copy needs privileged EXEC — \
                 enter an enable password"
            );
        }
        emit(DeployEvent::Stage("entering privileged EXEC".into()));
        shell.send("enable", Wire::Command).await?;
        signal = shell.expect(req.step_timeout).await?;
        if signal == Signal::Password {
            shell.send(&req.enable_password, Wire::Secret).await?;
            signal = shell.expect(req.step_timeout).await?;
        }
        if signal != Signal::PromptEnabled {
            bail!("enable failed — wrong enable password?");
        }
    }
    if signal != Signal::PromptEnabled {
        bail!("unexpected device state: {signal:?}");
    }

    // Pagination off, otherwise `copy` output stops at --More--.
    shell.send("terminal length 0", Wire::Command).await?;
    let signal = shell.expect(req.step_timeout).await?;
    if signal != Signal::PromptEnabled {
        bail!("the device did not accept `terminal length 0`");
    }
    shell.take_output();

    emit(DeployEvent::Stage("copying — this can take a few minutes".into()));
    shell.send(&req.command, Wire::Command).await?;
    let outcome = run_copy(&mut shell, req).await;

    // Leave the session tidy when the copy itself went through.
    if outcome.is_ok() {
        let _ = shell.send("exit", Wire::Command).await;
    }
    let _ = shell.channel.close().await;
    outcome
}

/// Drive the `copy` command until the device is back at its prompt.
async fn run_copy(shell: &mut Shell, req: &DeployRequest) -> Result<String> {
    let deadline = Instant::now() + req.copy_timeout;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("copy did not finish within {}s", req.copy_timeout.as_secs());
        }
        match shell.expect(remaining.min(req.copy_timeout)).await? {
            Signal::Question(q) => match classify_prompt(&q, req.overwrite) {
                PromptAction::Accept => shell.send("", Wire::Answer).await?,
                PromptAction::Decline => shell.send("n", Wire::Answer).await?,
                PromptAction::Abort(why) => bail!("{why} — session closed without answering"),
            },
            Signal::PromptEnabled => return verdict(&shell.take_output()),
            Signal::PromptUser => bail!("the device dropped back to user EXEC mode"),
            Signal::Password => bail!("the device asked for another password during the copy"),
            Signal::Closed => bail!("the device closed the session during the copy"),
        }
    }
}

/// Turn the device's `copy` output into a result. IOS reports success as
/// "N bytes copied in M secs" and failures with a `%`-prefixed line.
fn verdict(output: &str) -> Result<String> {
    if let Some(line) = output.lines().find(|l| l.contains("bytes copied in")) {
        return Ok(line.trim().to_string());
    }
    if let Some(err) = output
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("%Error") || l.starts_with("%error") || l.contains("Copy aborted"))
    {
        bail!("{err}");
    }
    if let Some(warn) = output.lines().map(str::trim).find(|l| l.starts_with('%')) {
        bail!("{warn}");
    }
    Ok("copy finished (device reported no byte count)".into())
}

/// Password first, keyboard-interactive second — Cisco devices offer one or
/// the other depending on how the vty lines are configured.
async fn authenticate(
    session: &mut client::Handle<ClientHandler>,
    req: &DeployRequest,
) -> Result<bool> {
    if session
        .authenticate_password(req.username.clone(), req.password.clone())
        .await?
    {
        return Ok(true);
    }
    let mut response = session
        .authenticate_keyboard_interactive_start(req.username.clone(), None)
        .await?;
    loop {
        match response {
            KeyboardInteractiveAuthResponse::Success => return Ok(true),
            KeyboardInteractiveAuthResponse::Failure => return Ok(false),
            KeyboardInteractiveAuthResponse::InfoRequest { ref prompts, .. } => {
                let answers = prompts.iter().map(|_| req.password.clone()).collect();
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
    fn allows_exactly_the_four_commands() {
        assert!(check_command("terminal length 0").is_ok());
        assert!(check_command("enable").is_ok());
        assert!(check_command("exit").is_ok());
        assert!(check_command("copy http://10.0.0.1:8080/img.bin flash:").is_ok());
    }

    #[test]
    fn refuses_every_configuration_command() {
        for cmd in [
            "configure terminal",
            "conf t",
            "config-transaction",
            "write memory",
            "write erase",
            "copy running-config startup-config",
            "reload",
            "reload in 5",
            "delete flash:image.bin",
            "erase startup-config",
            "format flash:",
            "archive download-sw",
            "license boot level",
            "show running-config",
            "dir flash:",
            "terminal length 0; configure terminal",
            "enable | configure",
            "copy http://10.0.0.1/x.bin flash: ! and more",
        ] {
            assert!(check_command(cmd).is_err(), "must be refused: {cmd}");
        }
    }

    #[test]
    fn copy_destination_must_be_a_storage_device() {
        for dst in ["flash:", "flash:img.bin", "bootflash:", "flash-1:", "usbflash0:", "disk0:img.bin"] {
            assert!(
                check_command(&format!("copy tftp://10.0.0.1/img.bin {dst}")).is_ok(),
                "must be allowed: {dst}"
            );
        }
        for dst in [
            "running-config",
            "startup-config",
            "system:running-config",
            "nvram:startup-config",
            "null:",
            "tftp://10.0.0.1/x",
            "flash",
            "unknowndev:",
        ] {
            assert!(
                check_command(&format!("copy tftp://10.0.0.1/img.bin {dst}")).is_err(),
                "must be refused: {dst}"
            );
        }
    }

    #[test]
    fn copy_source_must_be_a_transferbuddy_url() {
        assert!(check_command("copy flash:a.bin flash:b.bin").is_err());
        assert!(check_command("copy startup-config tftp://10.0.0.1/cfg").is_err());
        assert!(check_command("copy rcp://10.0.0.1/x.bin flash:").is_err());
    }

    #[test]
    fn answers_are_restricted_to_enter_and_no() {
        assert!(vet("", Wire::Answer).is_ok());
        assert!(vet("n", Wire::Answer).is_ok());
        assert!(vet("y", Wire::Answer).is_err());
        assert!(vet("configure terminal", Wire::Answer).is_err());
        assert!(vet("secret\rconfigure terminal", Wire::Secret).is_err());
        assert!(vet("terminal length 0\nreload", Wire::Command).is_err());
    }

    #[test]
    fn prompts_that_erase_are_always_declined() {
        assert_eq!(
            classify_prompt("Erase flash: before copying? [confirm]", true),
            PromptAction::Decline
        );
        assert_eq!(
            classify_prompt("Format flash: [confirm]", true),
            PromptAction::Decline
        );
    }

    #[test]
    fn overwrite_follows_the_setting() {
        let q = "%Warning:There is a file already existing with this name. \
                 Do you want to over write? [confirm]";
        assert_eq!(classify_prompt(q, false), PromptAction::Decline);
        assert_eq!(classify_prompt(q, true), PromptAction::Accept);
    }

    #[test]
    fn known_copy_prompts_are_accepted_unknown_ones_abort() {
        assert_eq!(
            classify_prompt("Destination filename [img.bin]?", false),
            PromptAction::Accept
        );
        assert_eq!(
            classify_prompt("Address or name of remote host [10.0.0.1]?", false),
            PromptAction::Accept
        );
        assert!(matches!(
            classify_prompt("Proceed with reload? [confirm]", true),
            PromptAction::Abort(_)
        ));
        assert!(matches!(
            classify_prompt("Continue? [yes/no]:", true),
            PromptAction::Abort(_)
        ));
    }

    #[test]
    fn recognises_device_prompts() {
        assert_eq!(tail_signal("cat9k-1#"), Some(Signal::PromptEnabled));
        assert_eq!(tail_signal("Switch>"), Some(Signal::PromptUser));
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

    /// A minimal IOS impersonator: password login, one shell channel, and
    /// the prompts a `copy` walks through. It records every line it received
    /// so the test can assert what transferbuddy typed.
    mod fake_ios {
        use std::collections::HashMap;
        use std::sync::{Arc, Mutex};

        use russh::server::{Auth, Handler, Msg, Session};
        use russh::{Channel, ChannelId, CryptoVec, MethodSet};

        pub struct Transcript {
            /// Every line transferbuddy sent, in order.
            pub received: Vec<String>,
        }

        pub struct Device {
            pub log: Arc<Mutex<Transcript>>,
            pub enable_password: String,
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

            /// One complete line of input from the client.
            fn on_line(&mut self, session: &mut Session, id: ChannelId, line: &str) {
                self.log.lock().unwrap().received.push(line.to_string());

                if self.expect_enable_password {
                    self.expect_enable_password = false;
                    if line == self.enable_password {
                        self.enabled = true;
                        self.say(session, id, "\r\ncat9k-1#");
                    } else {
                        self.say(session, id, "\r\n% Access denied\r\ncat9k-1>");
                    }
                    return;
                }
                if self.in_copy {
                    // The answer to "Destination filename [...]?".
                    self.in_copy = false;
                    self.say(
                        session,
                        id,
                        "\r\n!!!!!!!!!!!!!!!!!!!!\r\n\
                         521510912 bytes copied in 231.402 secs (2253266 bytes/sec)\r\ncat9k-1#",
                    );
                    return;
                }
                match line {
                    "enable" => {
                        self.expect_enable_password = true;
                        self.say(session, id, "\r\nPassword: ");
                    }
                    "terminal length 0" => self.say(session, id, "\r\ncat9k-1#"),
                    "exit" => {
                        session.eof(id);
                        session.close(id);
                    }
                    cmd if cmd.starts_with("copy ") => {
                        self.in_copy = true;
                        self.say(session, id, "\r\nDestination filename [img.bin]? ");
                    }
                    _ => self.say(
                        session,
                        id,
                        &format!("\r\n% Invalid input detected\r\ncat9k-1{}", if self.enabled { "#" } else { ">" }),
                    ),
                }
            }
        }

        pub fn device(log: Arc<Mutex<Transcript>>, enable_password: &str) -> Device {
            Device {
                log,
                enable_password: enable_password.to_string(),
                line: String::new(),
                enabled: false,
                expect_enable_password: false,
                in_copy: false,
                channels: HashMap::new(),
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
                    let handler = device(log.clone(), &enable_password);
                    let config = config.clone();
                    tokio::spawn(async move {
                        let _ = russh::server::run_stream(config, stream, handler).await;
                    });
                }
            });
            port
        }
    }

    fn request(port: u16, known_hosts: PathBuf, command: &str) -> DeployRequest {
        DeployRequest {
            host: "127.0.0.1".into(),
            port,
            username: "netadmin".into(),
            password: "letmein".into(),
            enable_password: "s3cret".into(),
            command: command.to_string(),
            overwrite: false,
            known_hosts,
            connect_timeout: Duration::from_secs(10),
            step_timeout: Duration::from_secs(10),
            copy_timeout: Duration::from_secs(20),
        }
    }

    /// Drive one deploy against the fake device, accepting its host key.
    async fn deploy_against_fake(
        req: DeployRequest,
    ) -> (std::result::Result<String, String>, Vec<String>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = Arc::new(AtomicBool::new(false));
        tokio::spawn(run(req, tx, cancel));

        let mut finished = None;
        let mut sent = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                DeployEvent::HostKey { reply, .. } => {
                    let _ = reply.send(true);
                }
                DeployEvent::Sent(line) => sent.push(line),
                DeployEvent::Finished(result) => finished = Some(result),
                _ => {}
            }
        }
        (finished.expect("a deploy always reports a result"), sent)
    }

    #[tokio::test]
    async fn deploys_against_a_fake_ios_device() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(std::sync::Mutex::new(fake_ios::Transcript { received: Vec::new() }));
        let port = fake_ios::spawn(log.clone(), "s3cret").await;

        let command = "copy http://127.0.0.1:8080/img.bin flash:";
        let req = request(port, dir.path().join("known_hosts"), command);
        let (result, sent) = tokio::time::timeout(
            Duration::from_secs(30),
            deploy_against_fake(req),
        )
        .await
        .expect("deploy timed out");

        let summary = result.expect("deploy should succeed");
        assert!(summary.contains("521510912 bytes copied in"), "summary: {summary}");

        // Exactly the whitelisted lines reached the device, in order — the
        // enable password is one of them, and it never appears in the UI
        // transcript.
        // `exit` is sent after the copy is already reported as finished, so
        // give the device a moment to see it.
        let expected = vec!["enable", "s3cret", "terminal length 0", command, "", "exit"];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if log.lock().unwrap().received.len() >= expected.len() || Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(log.lock().unwrap().received.clone(), expected);
        assert!(!sent.iter().any(|l| l.contains("s3cret")), "password leaked: {sent:?}");
        assert!(sent.iter().any(|l| l == "> ********" || l == "********"));

        // The host key was learned, so the next deploy would not ask again.
        let known = std::fs::read_to_string(dir.path().join("known_hosts")).unwrap();
        assert!(known.contains("127.0.0.1") || known.contains("[127.0.0.1]"));
    }

    #[tokio::test]
    async fn wrong_login_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let log = Arc::new(std::sync::Mutex::new(fake_ios::Transcript { received: Vec::new() }));
        let port = fake_ios::spawn(log, "s3cret").await;

        let mut req = request(
            port,
            dir.path().join("known_hosts"),
            "copy http://127.0.0.1:8080/img.bin flash:",
        );
        req.password = "wrong".into();
        let (result, _) =
            tokio::time::timeout(Duration::from_secs(30), deploy_against_fake(req))
                .await
                .expect("deploy timed out");
        let err = result.unwrap_err();
        assert!(err.contains("login failed"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn a_refused_command_never_opens_a_connection() {
        let dir = tempfile::tempdir().unwrap();
        let req = request(
            1, // nothing listens here; the command must be refused first
            dir.path().join("known_hosts"),
            "copy http://127.0.0.1:8080/img.bin running-config",
        );
        let (result, sent) =
            tokio::time::timeout(Duration::from_secs(30), deploy_against_fake(req))
                .await
                .expect("deploy timed out");
        let err = result.unwrap_err();
        assert!(err.contains("running-config"), "unexpected error: {err}");
        assert!(sent.is_empty(), "nothing may be sent: {sent:?}");
    }

    #[test]
    fn verdict_reads_the_device_output() {
        assert!(verdict("...\n521510912 bytes copied in 231.402 secs\ncat9k#").is_ok());
        let e = verdict("%Error opening tftp://10.0.0.1/x.bin (Timed out)\n").unwrap_err();
        assert!(e.to_string().contains("Timed out"));
        assert!(verdict("Copy aborted.\n").is_err());
    }
}
