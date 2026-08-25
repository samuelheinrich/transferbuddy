mod intro;
mod theme;
mod views;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::prelude::*;

use crate::deploy::{DeployEvent, DeployRequest};
use crate::logging::{Event, LogLevel};
use crate::services::ServiceId;
use crate::session::Protocol;
use crate::sound::Tone;
use crate::SharedApp;

/// How long a deploy waits for the TCP/SSH handshake, for one command to be
/// answered, and for the whole `copy` to finish.
const DEPLOY_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEPLOY_STEP_TIMEOUT: Duration = Duration::from_secs(30);
const DEPLOY_COPY_TIMEOUT: Duration = Duration::from_secs(45 * 60);
/// Transcript lines kept in memory; a long copy prints a lot of bangs.
const DEPLOY_TRANSCRIPT_MAX: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Services,
    Files,
    Sessions,
    Logs,
}

impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Dashboard, Tab::Services, Tab::Files, Tab::Sessions, Tab::Logs];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => "1 Dashboard",
            Tab::Services => "2 Services",
            Tab::Files => "3 Files",
            Tab::Sessions => "4 Sessions",
            Tab::Logs => "5 Logs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortBy {
    Name,
    Size,
    Modified,
}

impl SortBy {
    pub fn label(self) -> &'static str {
        match self {
            SortBy::Name => "name",
            SortBy::Size => "size",
            SortBy::Modified => "modified",
        }
    }
}

#[derive(Clone)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub ext: String,
}

pub struct FileBrowser {
    /// Path relative to root, "" = root.
    pub cwd: String,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
    pub sort: SortBy,
    pub filter: String,
    pub error: Option<String>,
}

impl FileBrowser {
    fn new() -> Self {
        Self {
            cwd: String::new(),
            entries: Vec::new(),
            selected: 0,
            sort: SortBy::Name,
            filter: String::new(),
            error: None,
        }
    }

    pub fn visible(&self) -> Vec<&FileEntry> {
        let f = self.filter.to_lowercase();
        self.entries
            .iter()
            .filter(|e| f.is_empty() || e.name.to_lowercase().contains(&f))
            .collect()
    }

    fn refresh(&mut self, root: &PathBuf) {
        self.error = None;
        let dir = if self.cwd.is_empty() { root.clone() } else { root.join(&self.cwd) };
        let mut entries = Vec::new();
        match std::fs::read_dir(&dir) {
            Ok(rd) => {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    if name.starts_with('.') {
                        continue;
                    }
                    let meta = match e.metadata() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    let ext = std::path::Path::new(&name)
                        .extension()
                        .map(|x| x.to_string_lossy().to_string())
                        .unwrap_or_default();
                    entries.push(FileEntry {
                        is_dir: meta.is_dir(),
                        size: meta.len(),
                        modified: meta.modified().ok(),
                        name,
                        ext,
                    });
                }
            }
            Err(e) => self.error = Some(format!("cannot read directory: {e}")),
        }
        match self.sort {
            SortBy::Name => entries.sort_by(|a, b| {
                b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
            SortBy::Size => {
                entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| b.size.cmp(&a.size)))
            }
            SortBy::Modified => entries
                .sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| b.modified.cmp(&a.modified))),
        }
        self.entries = entries;
        if self.selected >= self.visible().len() {
            self.selected = self.visible().len().saturating_sub(1);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Port(ServiceId),
    Bind(ServiceId),
    Username,
    Password,
    FileFilter,
    LogFilter,
    UploadDir,
    HashCompare,
}

/// The editable rows shown in the per-service edit popup (Enter on a service).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditField {
    StartStop,
    Enabled,
    Port,
    Bind,
    Username,
    Password,
    Uploads,
    UploadDir,
}

impl EditField {
    pub const ALL: [EditField; 8] = [
        EditField::StartStop,
        EditField::Enabled,
        EditField::Port,
        EditField::Bind,
        EditField::Username,
        EditField::Password,
        EditField::Uploads,
        EditField::UploadDir,
    ];
    pub fn label(self) -> &'static str {
        match self {
            EditField::StartStop => "start / stop",
            EditField::Enabled => "enabled",
            EditField::Port => "port",
            EditField::Bind => "bind address",
            EditField::Username => "username",
            EditField::Password => "password",
            EditField::Uploads => "uploads",
            EditField::UploadDir => "upload dir",
        }
    }
    /// Toggle rows react to Enter/Space directly; the rest open an inline editor.
    pub fn is_editable(self) -> bool {
        matches!(
            self,
            EditField::Port | EditField::Bind | EditField::Username | EditField::Password | EditField::UploadDir
        )
    }
}

/// MD5/SHA-256/SHA-512 digests of a file, plus an optional compare result.
pub struct HashInfo {
    pub rel_path: String,
    pub md5: String,
    pub sha256: String,
    pub sha512: String,
    pub compare: Option<HashCompare>,
}

pub struct HashCompare {
    pub input: String,
    /// "MD5" / "SHA-256" / "SHA-512", or "?" when the type could not be detected.
    pub kind: &'static str,
    pub matched: bool,
}

/// One editable row of the deploy form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployField {
    Host,
    Port,
    Username,
    Password,
    EnablePassword,
    Protocol,
    Destination,
    Overwrite,
}

impl DeployField {
    pub const ALL: [DeployField; 8] = [
        DeployField::Host,
        DeployField::Port,
        DeployField::Username,
        DeployField::Password,
        DeployField::EnablePassword,
        DeployField::Protocol,
        DeployField::Destination,
        DeployField::Overwrite,
    ];
    pub fn label(self) -> &'static str {
        match self {
            DeployField::Host => "switch IP / host",
            DeployField::Port => "ssh port",
            DeployField::Username => "username",
            DeployField::Password => "password",
            DeployField::EnablePassword => "enable password",
            DeployField::Protocol => "protocol",
            DeployField::Destination => "destination",
            DeployField::Overwrite => "overwrite",
        }
    }
    /// Rows that take typed text (the others toggle or cycle).
    pub fn is_text(self) -> bool {
        !matches!(self, DeployField::Protocol | DeployField::Overwrite)
    }
}

/// The form values. Host, port, user, protocol and destination survive the
/// modal being closed so the next file can be deployed without retyping;
/// the two passwords are cleared after every run.
#[derive(Debug, Clone)]
pub struct DeployForm {
    pub host: String,
    pub port: String,
    pub username: String,
    pub password: String,
    pub enable_password: String,
    pub proto: Protocol,
    pub dest: String,
    pub overwrite: bool,
    pub field: usize,
}

impl Default for DeployForm {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: "22".into(),
            username: String::new(),
            password: String::new(),
            enable_password: String::new(),
            proto: Protocol::Http,
            dest: "flash:".into(),
            overwrite: false,
            field: 0,
        }
    }
}

/// Where a deploy currently is.
pub enum DeployPhase {
    /// Filling in the form.
    Form,
    /// Waiting for the user to accept an unknown host key.
    HostKey { fingerprint: String },
    /// SSH session is running.
    Running,
    /// Finished — `Ok` carries the device's summary line.
    Done(std::result::Result<String, String>),
}

/// Everything the deploy modal shows and owns.
pub struct DeployView {
    /// File being deployed, relative to the shared root.
    pub rel_path: String,
    pub form: DeployForm,
    pub phase: DeployPhase,
    /// Coarse phase text for the modal header.
    pub stage: String,
    /// The session transcript, oldest first.
    pub transcript: Vec<String>,
    /// The device's current, unfinished output line.
    pub live: String,
    /// Rows scrolled away at the top; `None` = follow the tail.
    pub scroll: Option<usize>,
    /// Problem with the form, shown in red under the fields.
    pub error: Option<String>,
    /// The command that will be typed on the device.
    pub command: Option<String>,
    rx: Option<tokio::sync::mpsc::UnboundedReceiver<DeployEvent>>,
    cancel: Arc<AtomicBool>,
    host_key_reply: Option<tokio::sync::oneshot::Sender<bool>>,
}

impl DeployView {
    fn push(&mut self, line: String) {
        self.transcript.push(line);
        if self.transcript.len() > DEPLOY_TRANSCRIPT_MAX {
            let cut = self.transcript.len() - DEPLOY_TRANSCRIPT_MAX;
            self.transcript.drain(..cut);
            if let Some(s) = self.scroll.as_mut() {
                *s = s.saturating_sub(cut);
            }
        }
    }
    pub fn is_running(&self) -> bool {
        matches!(self.phase, DeployPhase::Running | DeployPhase::HostKey { .. })
    }

    /// A view with no session attached, for rendering tests.
    #[cfg(test)]
    pub fn for_test(rel_path: &str, form: DeployForm) -> Self {
        Self {
            rel_path: rel_path.to_string(),
            form,
            phase: DeployPhase::Form,
            stage: String::new(),
            transcript: Vec::new(),
            live: String::new(),
            scroll: None,
            error: None,
            command: None,
            rx: None,
            cancel: Arc::new(AtomicBool::new(false)),
            host_key_reply: None,
        }
    }
}

pub enum Modal {
    Help,
    ConfirmQuit,
    ConfirmUploads,
    Input { title: String, value: String, action: InputAction },
    Cisco { rel_path: String, commands: Vec<(Protocol, String)>, selected: usize, copied: bool },
    Message(String),
    /// Per-service edit popup. `editing` holds the in-progress text when an
    /// editable field is being typed into.
    ServiceEdit { id: ServiceId, field: usize, editing: Option<String> },
    /// File hash view; the data lives in `Ui::hashes` so it survives the
    /// detour through the compare-input modal.
    Hashes,
    /// Deploy form / live SSH session; the data lives in `Ui::deploy`.
    Deploy,
}

pub struct Ui {
    pub app: SharedApp,
    pub tab: Tab,
    pub service_sel: usize,
    pub files: FileBrowser,
    pub session_sel: usize,
    pub log_scroll: usize,
    pub log_follow: bool,
    pub log_filter: String,
    pub log_min_level: LogLevel,
    pub log_proto_filter: Option<ServiceId>,
    pub modal: Option<Modal>,
    pub should_quit: bool,
    pub status_msg: Option<String>,
    /// Last computed file hashes, shown by the Hashes modal.
    pub hashes: Option<HashInfo>,
    /// Scroll offset of the help modal (it is taller than short terminals).
    pub help_scroll: usize,
    /// Deploy form and running session, kept across modal opens so host and
    /// user do not have to be retyped for the next file.
    pub deploy: Option<DeployView>,
}

impl Ui {
    /// Short beep, unless sound is switched off.
    fn beep(&self, tone: Tone) {
        let enabled = self.app.config.read().unwrap().sound;
        crate::sound::play(enabled, tone);
    }
}

pub fn run(app: SharedApp) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (intro_enabled, sound_enabled) = {
        let cfg = app.config.read().unwrap();
        (cfg.intro, cfg.sound)
    };
    let result = if intro_enabled {
        intro::play(&mut terminal, sound_enabled).and_then(|()| run_loop(&mut terminal, app))
    } else {
        run_loop(&mut terminal, app)
    };

    disable_raw_mode()?;
    crossterm::execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn run_loop(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: SharedApp) -> Result<()> {
    let mut ui = Ui {
        app,
        tab: Tab::Dashboard,
        service_sel: 0,
        files: FileBrowser::new(),
        session_sel: 0,
        log_scroll: 0,
        log_follow: true,
        log_filter: String::new(),
        log_min_level: LogLevel::Debug,
        log_proto_filter: None,
        modal: None,
        should_quit: false,
        status_msg: None,
        hashes: None,
        help_scroll: 0,
        deploy: None,
    };
    let root = ui.app.config.read().unwrap().root.clone();
    ui.files.refresh(&root);

    loop {
        pump_deploy(&mut ui);
        terminal.draw(|f| views::draw(f, &mut ui))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                CEvent::Key(key) if key.kind == KeyEventKind::Press => handle_key(&mut ui, key),
                _ => {}
            }
        }
        if ui.should_quit {
            return Ok(());
        }
    }
}

fn handle_key(ui: &mut Ui, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        ui.modal = Some(Modal::ConfirmQuit);
        return;
    }
    if ui.modal.is_some() {
        handle_modal_key(ui, key);
        return;
    }
    match key.code {
        KeyCode::Char('q') => {
            let any_running =
                ServiceId::ALL.iter().any(|id| ui.app.services.status(*id).is_running());
            if any_running {
                ui.modal = Some(Modal::ConfirmQuit);
            } else {
                ui.should_quit = true;
            }
        }
        KeyCode::Char('?') | KeyCode::Char('h') => {
            ui.help_scroll = 0;
            ui.modal = Some(Modal::Help);
        }
        KeyCode::Char('m') => {
            let enabled = {
                let mut cfg = ui.app.config.write().unwrap();
                cfg.sound = !cfg.sound;
                cfg.sound
            };
            save_config(ui);
            // Beep only when switching on — the off-beep would be the last
            // thing you hear after asking for silence.
            crate::sound::play(enabled, Tone::On);
            ui.status_msg = Some(if enabled { "sound on".into() } else { "sound off".into() });
        }
        KeyCode::Tab | KeyCode::Right => {
            let i = Tab::ALL.iter().position(|t| *t == ui.tab).unwrap_or(0);
            ui.tab = Tab::ALL[(i + 1) % Tab::ALL.len()];
            on_tab_changed(ui);
        }
        KeyCode::BackTab | KeyCode::Left => {
            let i = Tab::ALL.iter().position(|t| *t == ui.tab).unwrap_or(0);
            ui.tab = Tab::ALL[(i + Tab::ALL.len() - 1) % Tab::ALL.len()];
            on_tab_changed(ui);
        }
        KeyCode::Char('1') => ui.tab = Tab::Dashboard,
        KeyCode::Char('2') => ui.tab = Tab::Services,
        KeyCode::Char('c') if ui.tab != Tab::Files && ui.tab != Tab::Logs => ui.tab = Tab::Services,
        KeyCode::Char('3') | KeyCode::Char('f') => {
            ui.tab = Tab::Files;
            let root = ui.app.config.read().unwrap().root.clone();
            ui.files.refresh(&root);
        }
        KeyCode::Char('4') | KeyCode::Char('a') => ui.tab = Tab::Sessions,
        KeyCode::Char('5') | KeyCode::Char('l') => ui.tab = Tab::Logs,
        _ => match ui.tab {
            Tab::Dashboard => {}
            Tab::Services => handle_services_key(ui, key),
            Tab::Files => handle_files_key(ui, key),
            Tab::Sessions => handle_sessions_key(ui, key),
            Tab::Logs => handle_logs_key(ui, key),
        },
    }
}

/// Refresh view-specific state after switching tabs (e.g. re-read the file list).
fn on_tab_changed(ui: &mut Ui) {
    if ui.tab == Tab::Files {
        let root = ui.app.config.read().unwrap().root.clone();
        ui.files.refresh(&root);
    }
}

fn selected_service(ui: &Ui) -> ServiceId {
    ServiceId::ALL[ui.service_sel.min(ServiceId::ALL.len() - 1)]
}

fn save_config(ui: &mut Ui) {
    let result = ui.app.config.read().unwrap().save();
    if let Err(e) = result {
        ui.status_msg = Some(format!("saving config failed: {e}"));
    }
}

fn handle_services_key(ui: &mut Ui, key: KeyEvent) {
    let id = selected_service(ui);
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.service_sel = ui.service_sel.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            ui.service_sel = (ui.service_sel + 1).min(ServiceId::ALL.len() - 1);
        }
        KeyCode::Char(' ') => {
            let enabled = {
                let mut cfg = ui.app.config.write().unwrap();
                let sc = cfg.service_mut(id);
                sc.enabled = !sc.enabled;
                sc.enabled
            };
            save_config(ui);
            ui.beep(if enabled { Tone::On } else { Tone::Off });
            if !enabled && ui.app.services.status(id).is_running() {
                ui.app.services.stop(id);
            }
            ui.status_msg =
                Some(format!("{} {}", id.display_name(), if enabled { "enabled" } else { "disabled" }));
        }
        KeyCode::Char('s') => {
            if ui.app.services.status(id).is_running() {
                ui.beep(Tone::Off);
                ui.app.services.stop(id);
            } else {
                ui.beep(Tone::On);
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).enabled = true;
                }
                save_config(ui);
                ui.app.services.start(id);
            }
        }
        KeyCode::Char('r') => {
            ui.beep(Tone::Confirm);
            ui.app.services.restart(id);
        }
        KeyCode::Char('S') => {
            ui.beep(Tone::On);
            ui.app.services.start_all_enabled();
        }
        KeyCode::Char('X') => {
            ui.beep(Tone::Off);
            ui.app.services.stop_all();
        }
        KeyCode::Char('p') => {
            let port = ui.app.config.read().unwrap().service(id).port;
            ui.modal = Some(Modal::Input {
                title: format!("{} port", id.display_name()),
                value: port.to_string(),
                action: InputAction::Port(id),
            });
        }
        KeyCode::Char('b') => {
            let bind = ui.app.config.read().unwrap().service(id).bind.clone();
            ui.modal = Some(Modal::Input {
                title: format!("{} bind address", id.display_name()),
                value: bind,
                action: InputAction::Bind(id),
            });
        }
        KeyCode::Char('n') => {
            let user = ui.app.config.read().unwrap().auth.username.clone();
            ui.modal = Some(Modal::Input {
                title: "username (FTP/SFTP/SCP)".into(),
                value: user,
                action: InputAction::Username,
            });
        }
        KeyCode::Char('w') => {
            let pass = ui.app.config.read().unwrap().auth.password.clone();
            ui.modal = Some(Modal::Input {
                title: "password (FTP/SFTP/SCP)".into(),
                value: pass,
                action: InputAction::Password,
            });
        }
        KeyCode::Char('u') => {
            let enabled = ui.app.config.read().unwrap().uploads.enabled;
            if enabled {
                ui.app.config.write().unwrap().uploads.enabled = false;
                save_config(ui);
                ui.beep(Tone::Off);
                ui.status_msg = Some("uploads disabled".into());
            } else {
                ui.modal = Some(Modal::ConfirmUploads);
            }
        }
        KeyCode::Char('d') => {
            let dir = ui.app.config.read().unwrap().uploads.dir.clone();
            ui.modal = Some(Modal::Input {
                title: "upload directory (relative to root)".into(),
                value: dir,
                action: InputAction::UploadDir,
            });
        }
        KeyCode::Char('L') => {
            ui.log_proto_filter = Some(id);
            ui.tab = Tab::Logs;
        }
        KeyCode::Enter | KeyCode::Char('e') => {
            ui.modal = Some(Modal::ServiceEdit { id, field: 0, editing: None });
        }
        _ => {}
    }
}

fn handle_files_key(ui: &mut Ui, key: KeyEvent) {
    let root = ui.app.config.read().unwrap().root.clone();
    let count = ui.files.visible().len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => ui.files.selected = ui.files.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            if count > 0 {
                ui.files.selected = (ui.files.selected + 1).min(count - 1);
            }
        }
        KeyCode::Enter => {
            let entry = ui.files.visible().get(ui.files.selected).map(|e| (*e).clone());
            if let Some(entry) = entry {
                if entry.is_dir {
                    if ui.files.cwd.is_empty() {
                        ui.files.cwd = entry.name.clone();
                    } else {
                        ui.files.cwd = format!("{}/{}", ui.files.cwd, entry.name);
                    }
                    ui.files.selected = 0;
                    ui.files.filter.clear();
                    ui.files.refresh(&root);
                } else {
                    open_cisco_modal(ui, &entry.name);
                }
            }
        }
        KeyCode::Backspace => {
            if !ui.files.cwd.is_empty() {
                ui.files.cwd = match ui.files.cwd.rsplit_once('/') {
                    Some((parent, _)) => parent.to_string(),
                    None => String::new(),
                };
                ui.files.selected = 0;
                ui.files.refresh(&root);
            }
        }
        KeyCode::Char('H') => {
            let entry = ui.files.visible().get(ui.files.selected).map(|e| (*e).clone());
            if let Some(entry) = entry {
                if !entry.is_dir {
                    open_hashes_modal(ui, &root, &entry.name);
                }
            }
        }
        KeyCode::Char('s') => {
            ui.files.sort = match ui.files.sort {
                SortBy::Name => SortBy::Size,
                SortBy::Size => SortBy::Modified,
                SortBy::Modified => SortBy::Name,
            };
            ui.files.refresh(&root);
        }
        KeyCode::Char('/') => {
            ui.modal = Some(Modal::Input {
                title: "filter files".into(),
                value: ui.files.filter.clone(),
                action: InputAction::FileFilter,
            });
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            let entry = ui.files.visible().get(ui.files.selected).map(|e| (*e).clone());
            if let Some(entry) = entry {
                if entry.is_dir {
                    ui.status_msg = Some("deploy works on files, not directories".into());
                } else {
                    open_deploy_modal(ui, &entry.name);
                }
            }
        }
        KeyCode::Char('R') | KeyCode::F(5) => ui.files.refresh(&root),
        KeyCode::Char('y') => {
            let entry = ui.files.visible().get(ui.files.selected).map(|e| (*e).clone());
            if let Some(entry) = entry {
                if !entry.is_dir {
                    open_cisco_modal(ui, &entry.name);
                }
            }
        }
        _ => {}
    }
}

fn open_cisco_modal(ui: &mut Ui, name: &str) {
    let rel = if ui.files.cwd.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", ui.files.cwd, name)
    };
    let cfg = ui.app.config.read().unwrap();
    let ip = crate::netif::suggest_ip()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let mut commands = crate::cisco::commands_for_file(&cfg, &ip, &rel);
    if commands.is_empty() {
        // No service enabled yet — show all so the user sees what's possible.
        for proto in
            [Protocol::Http, Protocol::Https, Protocol::Ftp, Protocol::Scp, Protocol::Sftp, Protocol::Tftp]
        {
            commands.push((proto, crate::cisco::copy_command(proto, &cfg, &ip, &rel)));
        }
    }
    drop(cfg);
    ui.modal = Some(Modal::Cisco { rel_path: rel, commands, selected: 0, copied: false });
}

fn open_hashes_modal(ui: &mut Ui, root: &PathBuf, name: &str) {
    let rel = if ui.files.cwd.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", ui.files.cwd, name)
    };
    let abs = root.join(&rel);
    ui.status_msg = Some(format!("hashing {rel} …"));
    match compute_hashes(&abs) {
        Ok((md5, sha256, sha512)) => {
            ui.hashes = Some(HashInfo { rel_path: rel, md5, sha256, sha512, compare: None });
            ui.status_msg = None;
            ui.modal = Some(Modal::Hashes);
        }
        Err(e) => ui.modal = Some(Modal::Message(format!("cannot hash {rel}:\n{e}"))),
    }
}

/// Stream a file once through all three digests. Cisco images can be hundreds
/// of megabytes, so read in chunks rather than loading the whole file.
fn compute_hashes(path: &std::path::Path) -> std::io::Result<(String, String, String)> {
    use md5::Md5;
    use sha2::{Digest, Sha256, Sha512};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut md5 = Md5::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        md5.update(&buf[..n]);
        sha256.update(&buf[..n]);
        sha512.update(&buf[..n]);
    }
    Ok((hex(&md5.finalize()), hex(&sha256.finalize()), hex(&sha512.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Compare a user-supplied digest against the computed ones. The hash type is
/// detected from its length (32/64/128 hex chars = MD5/SHA-256/SHA-512) so the
/// user only has to paste a value.
fn compare_hash(info: &HashInfo, input: &str) -> HashCompare {
    let norm: String = input.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_lowercase();
    let is_hex = !norm.is_empty() && norm.chars().all(|c| c.is_ascii_hexdigit());
    let (kind, expected): (&'static str, Option<&str>) = if !is_hex {
        ("?", None)
    } else {
        match norm.len() {
            32 => ("MD5", Some(info.md5.as_str())),
            64 => ("SHA-256", Some(info.sha256.as_str())),
            128 => ("SHA-512", Some(info.sha512.as_str())),
            _ => ("?", None),
        }
    };
    let matched = expected.map(|e| e == norm).unwrap_or(false);
    HashCompare { input: norm, kind, matched }
}

/// Protocols offered in the deploy form, in the order the selector cycles.
const DEPLOY_PROTOCOLS: [Protocol; 6] = [
    Protocol::Http,
    Protocol::Https,
    Protocol::Tftp,
    Protocol::Ftp,
    Protocol::Scp,
    Protocol::Sftp,
];

/// Open the deploy form for one file. Host, user and destination from the
/// previous deploy are kept; the passwords are not.
fn open_deploy_modal(ui: &mut Ui, name: &str) {
    let rel = if ui.files.cwd.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", ui.files.cwd, name)
    };
    let mut form = ui.deploy.as_ref().map(|d| d.form.clone()).unwrap_or_default();
    form.password.clear();
    form.enable_password.clear();
    form.field = 0;
    if form.username.is_empty() {
        form.proto = default_deploy_protocol(ui);
    }
    ui.deploy = Some(DeployView {
        rel_path: rel,
        form,
        phase: DeployPhase::Form,
        stage: String::new(),
        transcript: Vec::new(),
        live: String::new(),
        scroll: None,
        error: None,
        command: None,
        rx: None,
        cancel: Arc::new(AtomicBool::new(false)),
        host_key_reply: None,
    });
    ui.modal = Some(Modal::Deploy);
}

/// Prefer a protocol whose service is already running, then an enabled one.
fn default_deploy_protocol(ui: &Ui) -> Protocol {
    let cfg = ui.app.config.read().unwrap();
    for proto in DEPLOY_PROTOCOLS {
        if ui.app.services.status(crate::cisco::service_of(proto)).is_running() {
            return proto;
        }
    }
    for proto in DEPLOY_PROTOCOLS {
        if cfg.service(crate::cisco::service_of(proto)).enabled {
            return proto;
        }
    }
    Protocol::Http
}

/// The `copy` command a deploy would type on the device. Used both for the
/// preview in the form and for the session itself, so what is shown is what
/// runs.
pub fn deploy_command_for(ui: &Ui, form: &DeployForm, rel: &str) -> Result<String, String> {
    let id = crate::cisco::service_of(form.proto);
    let cfg = ui.app.config.read().unwrap();
    let peer: Option<std::net::IpAddr> = form.host.trim().parse().ok();
    let ip = crate::netif::advertised_ip(&cfg.service(id).bind, peer.as_ref())
        .ok_or_else(|| "no local IP address to advertise to the switch".to_string())?;
    crate::cisco::deploy_command(form.proto, &cfg, &ip, rel, &form.dest)
}

/// Hide the transfer password that FTP URLs carry, for the log only.
fn redact_url_password(cmd: &str) -> String {
    match (cmd.find("://"), cmd.find('@')) {
        (Some(scheme), Some(at)) if at > scheme => {
            let creds = &cmd[scheme + 3..at];
            match creds.split_once(':') {
                Some((user, _)) => format!("{}{user}:***{}", &cmd[..scheme + 3], &cmd[at..]),
                None => cmd.to_string(),
            }
        }
        _ => cmd.to_string(),
    }
}

/// Validate the form and hand the session to the runtime.
fn start_deploy(ui: &mut Ui) {
    let Some(view) = ui.deploy.as_ref() else { return };
    if view.is_running() {
        return;
    }
    let form = view.form.clone();
    let rel = view.rel_path.clone();

    let fail = |ui: &mut Ui, msg: String| {
        if let Some(v) = ui.deploy.as_mut() {
            v.error = Some(msg);
        }
        ui.beep(Tone::Error);
    };

    let host = form.host.trim().to_string();
    if host.is_empty() {
        return fail(ui, "enter the switch IP or hostname".into());
    }
    let port: u16 = match form.port.trim().parse() {
        Ok(p) if p > 0 => p,
        _ => return fail(ui, format!("invalid ssh port: {:?}", form.port)),
    };
    if form.username.trim().is_empty() {
        return fail(ui, "enter the username for the switch".into());
    }
    if form.password.is_empty() {
        return fail(ui, "enter the password for the switch".into());
    }

    let id = crate::cisco::service_of(form.proto);
    if !ui.app.services.status(id).is_running() {
        return fail(
            ui,
            format!(
                "{} is not running — start it in the services view (2, then s)",
                id.display_name()
            ),
        );
    }

    let command = match deploy_command_for(ui, &form, &rel) {
        Ok(c) => c,
        Err(e) => return fail(ui, e),
    };
    let known_hosts = ui
        .app
        .config
        .read()
        .unwrap()
        .config_dir
        .join("state")
        .join("known_hosts");

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let request = DeployRequest {
        host: host.clone(),
        port,
        username: form.username.trim().to_string(),
        password: form.password.clone(),
        enable_password: form.enable_password.clone(),
        command: command.clone(),
        overwrite: form.overwrite,
        known_hosts,
        connect_timeout: DEPLOY_CONNECT_TIMEOUT,
        step_timeout: DEPLOY_STEP_TIMEOUT,
        copy_timeout: DEPLOY_COPY_TIMEOUT,
    };
    ui.app
        .runtime
        .spawn(crate::deploy::run(request, tx, cancel.clone()));
    let mut ev = Event::new(LogLevel::Info, "deploy", format!("start on {host}"))
        .path(rel.clone())
        .result(redact_url_password(&command));
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        ev = ev.ip(ip);
    }
    ui.app.logger.log(ev);

    if let Some(v) = ui.deploy.as_mut() {
        v.error = None;
        v.command = Some(command);
        v.transcript.clear();
        v.live.clear();
        v.scroll = None;
        v.stage = "starting".into();
        v.phase = DeployPhase::Running;
        v.rx = Some(rx);
        v.cancel = cancel;
        v.host_key_reply = None;
    }
}

/// Drain the deploy channel into the view. Called once per UI frame.
fn pump_deploy(ui: &mut Ui) {
    let Some(view) = ui.deploy.as_mut() else { return };
    let events: Vec<DeployEvent> = {
        let Some(rx) = view.rx.as_mut() else { return };
        let mut out = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            out.push(ev);
            if out.len() >= 2000 {
                break;
            }
        }
        out
    };
    if events.is_empty() {
        return;
    }
    let mut finished = None;
    for ev in events {
        match ev {
            DeployEvent::Stage(text) => view.stage = text,
            DeployEvent::Sent(text) => view.push(format!("> {text}")),
            DeployEvent::Output(line) => view.push(line),
            DeployEvent::Live(line) => view.live = line,
            DeployEvent::HostKey { fingerprint, reply } => {
                view.host_key_reply = Some(reply);
                view.phase = DeployPhase::HostKey { fingerprint };
            }
            DeployEvent::Finished(result) => {
                view.live.clear();
                view.rx = None;
                view.stage = match &result {
                    Ok(_) => "done".into(),
                    Err(_) => "failed".into(),
                };
                view.form.password.clear();
                view.form.enable_password.clear();
                view.phase = DeployPhase::Done(result.clone());
                finished = Some(result);
            }
        }
    }
    if let Some(result) = finished {
        let (level, tone) = match &result {
            Ok(_) => (LogLevel::Info, Tone::Confirm),
            Err(_) => (LogLevel::Error, Tone::Error),
        };
        let (host, rel) = ui
            .deploy
            .as_ref()
            .map(|v| (v.form.host.clone(), v.rel_path.clone()))
            .unwrap_or_default();
        let ev = Event::new(level, "deploy", format!("finished on {host}")).path(rel);
        let ev = match &result {
            Ok(summary) => ev.result(summary.clone()),
            Err(e) => ev.error(e.clone()),
        };
        ui.app.logger.log(ev);
        ui.beep(tone);
    }
}

/// Answer the host-key question and let the SSH handshake continue.
fn answer_host_key(ui: &mut Ui, accept: bool) {
    let Some(view) = ui.deploy.as_mut() else { return };
    if let Some(reply) = view.host_key_reply.take() {
        let _ = reply.send(accept);
    }
    view.phase = DeployPhase::Running;
    if !accept {
        view.push("host key rejected".into());
    }
}

fn handle_deploy_key(ui: &mut Ui, key: KeyEvent) {
    let Some(view) = ui.deploy.as_mut() else {
        ui.modal = None;
        return;
    };
    match &view.phase {
        DeployPhase::HostKey { .. } => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => answer_host_key(ui, true),
            _ => answer_host_key(ui, false),
        },
        DeployPhase::Running => match key.code {
            KeyCode::Char('c') => {
                view.cancel.store(true, Ordering::Relaxed);
                view.push("cancelling …".into());
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                ui.status_msg = Some("deploy is running — c cancels it".into());
            }
            _ => scroll_deploy(view, key),
        },
        DeployPhase::Done(_) => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ui.modal = None,
            KeyCode::Char('r') => {
                view.phase = DeployPhase::Form;
                view.error = None;
            }
            _ => scroll_deploy(view, key),
        },
        DeployPhase::Form => {
            let fields = DeployField::ALL;
            let cur = fields[view.form.field.min(fields.len() - 1)];
            match key.code {
                KeyCode::Esc => ui.modal = None,
                KeyCode::Enter => start_deploy(ui),
                KeyCode::Up | KeyCode::BackTab => {
                    view.form.field = view.form.field.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Tab => {
                    view.form.field = (view.form.field + 1).min(fields.len() - 1)
                }
                KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
                    if !cur.is_text() =>
                {
                    match cur {
                        DeployField::Overwrite => {
                            view.form.overwrite = !view.form.overwrite;
                        }
                        _ => {
                            let i = DEPLOY_PROTOCOLS
                                .iter()
                                .position(|p| *p == view.form.proto)
                                .unwrap_or(0);
                            let n = DEPLOY_PROTOCOLS.len();
                            let step = if key.code == KeyCode::Left { n - 1 } else { 1 };
                            view.form.proto = DEPLOY_PROTOCOLS[(i + step) % n];
                        }
                    }
                    view.error = None;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if cur.is_text() {
                        field_buffer(&mut view.form, cur).push(c);
                        view.error = None;
                    }
                }
                KeyCode::Backspace => {
                    if cur.is_text() {
                        field_buffer(&mut view.form, cur).pop();
                        view.error = None;
                    }
                }
                _ => {}
            }
        }
    }
}

fn field_buffer(form: &mut DeployForm, field: DeployField) -> &mut String {
    match field {
        DeployField::Host => &mut form.host,
        DeployField::Port => &mut form.port,
        DeployField::Username => &mut form.username,
        DeployField::Password => &mut form.password,
        DeployField::EnablePassword => &mut form.enable_password,
        DeployField::Destination => &mut form.dest,
        // Not text fields; never reached, but a buffer has to be returned.
        DeployField::Protocol | DeployField::Overwrite => &mut form.dest,
    }
}

fn scroll_deploy(view: &mut DeployView, key: KeyEvent) {
    let len = view.transcript.len();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            let cur = view.scroll.unwrap_or(len);
            view.scroll = Some(cur.saturating_sub(1));
        }
        KeyCode::Down | KeyCode::Char('j') => {
            let cur = view.scroll.unwrap_or(len);
            view.scroll = if cur + 1 >= len { None } else { Some(cur + 1) };
        }
        KeyCode::PageUp => {
            let cur = view.scroll.unwrap_or(len);
            view.scroll = Some(cur.saturating_sub(15));
        }
        KeyCode::PageDown => {
            let cur = view.scroll.unwrap_or(len);
            view.scroll = if cur + 15 >= len { None } else { Some(cur + 15) };
        }
        KeyCode::End | KeyCode::Char('G') => view.scroll = None,
        _ => {}
    }
}

fn handle_sessions_key(ui: &mut Ui, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => ui.session_sel = ui.session_sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => ui.session_sel += 1,
        KeyCode::Char('B') => {
            let mut cfg = ui.app.config.write().unwrap();
            cfg.speed_in_bits = !cfg.speed_in_bits;
        }
        _ => {}
    }
}

fn handle_logs_key(ui: &mut Ui, key: KeyEvent) {
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => {
            ui.log_follow = false;
            ui.log_scroll = ui.log_scroll.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => ui.log_scroll += 1,
        KeyCode::PageUp => {
            ui.log_follow = false;
            ui.log_scroll = ui.log_scroll.saturating_sub(20);
        }
        KeyCode::PageDown => ui.log_scroll += 20,
        KeyCode::End | KeyCode::Char('G') => ui.log_follow = true,
        KeyCode::Char('/') => {
            ui.modal = Some(Modal::Input {
                title: "filter logs (text, IP, session id, file)".into(),
                value: ui.log_filter.clone(),
                action: InputAction::LogFilter,
            });
        }
        KeyCode::Char('L') => {
            let i = LogLevel::ALL.iter().position(|l| *l == ui.log_min_level).unwrap_or(0);
            ui.log_min_level = LogLevel::ALL[(i + 1) % LogLevel::ALL.len()];
        }
        KeyCode::Char('P') => {
            ui.log_proto_filter = match ui.log_proto_filter {
                None => Some(ServiceId::ALL[0]),
                Some(cur) => {
                    let i = ServiceId::ALL.iter().position(|s| *s == cur).unwrap_or(0);
                    if i + 1 >= ServiceId::ALL.len() {
                        None
                    } else {
                        Some(ServiceId::ALL[i + 1])
                    }
                }
            };
        }
        _ => {}
    }
}

fn handle_modal_key(ui: &mut Ui, key: KeyEvent) {
    let mut modal = ui.modal.take();
    match &mut modal {
        Some(Modal::Help) => match key.code {
            // The two key tables can be taller than the terminal.
            KeyCode::Up | KeyCode::Char('k') => {
                ui.help_scroll = ui.help_scroll.saturating_sub(1);
                ui.modal = modal;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                ui.help_scroll += 1;
                ui.modal = modal;
                return;
            }
            KeyCode::PageUp => {
                ui.help_scroll = ui.help_scroll.saturating_sub(10);
                ui.modal = modal;
                return;
            }
            KeyCode::PageDown => {
                ui.help_scroll += 10;
                ui.modal = modal;
                return;
            }
            // Any other key closes.
            _ => {}
        },
        Some(Modal::Message(_)) => {
            // Any key closes.
        }
        Some(Modal::ConfirmQuit) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                ui.app.services.stop_all();
                ui.should_quit = true;
            }
            _ => {}
        },
        Some(Modal::ConfirmUploads) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                ui.app.config.write().unwrap().uploads.enabled = true;
                save_config(ui);
                ui.beep(Tone::On);
                ui.status_msg = Some("uploads enabled — restart running services to apply".into());
            }
            _ => {}
        },
        Some(Modal::Input { value, action, .. }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                ui.modal = modal;
                return;
            }
            KeyCode::Backspace => {
                value.pop();
                ui.modal = modal;
                return;
            }
            KeyCode::Enter => {
                let (action, value) = (*action, value.clone());
                apply_input(ui, action, value);
            }
            _ => {
                ui.modal = modal;
                return;
            }
        },
        Some(Modal::Cisco { commands, selected, copied, .. }) => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                *copied = false;
                ui.modal = modal;
                return;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(commands.len().saturating_sub(1));
                *copied = false;
                ui.modal = modal;
                return;
            }
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some((_, cmd)) = commands.get(*selected) {
                    match copy_to_clipboard(cmd) {
                        Ok(()) => {
                            *copied = true;
                            ui.status_msg = Some("command copied to clipboard".into());
                        }
                        Err(e) => ui.status_msg = Some(format!("clipboard failed: {e}")),
                    }
                }
                ui.modal = modal;
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => {
                ui.modal = modal;
                return;
            }
        },
        Some(Modal::ServiceEdit { id, field, editing }) => {
            let id = *id;
            let fields = EditField::ALL;
            let cur = fields[(*field).min(fields.len() - 1)];
            if let Some(buf) = editing {
                match key.code {
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        buf.push(c);
                    }
                    KeyCode::Backspace => {
                        buf.pop();
                    }
                    KeyCode::Esc => {
                        *editing = None;
                    }
                    KeyCode::Enter => {
                        let value = buf.clone();
                        *editing = None;
                        match commit_service_edit(ui, id, cur, value) {
                            Some(err) => {
                                ui.beep(Tone::Error);
                                ui.status_msg = Some(err);
                            }
                            None => ui.beep(Tone::Confirm),
                        }
                    }
                    _ => {}
                }
                ui.modal = modal;
                return;
            }
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    *field = field.saturating_sub(1);
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    *field = (*field + 1).min(fields.len() - 1);
                }
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if cur.is_editable() {
                        *editing = Some(current_field_value(ui, id, cur));
                    } else {
                        activate_service_field(ui, id, cur);
                    }
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    ui.modal = None;
                    return;
                }
                _ => {}
            }
            ui.modal = modal;
            return;
        }
        Some(Modal::Deploy) => {
            ui.modal = modal;
            handle_deploy_key(ui, key);
            return;
        }
        Some(Modal::Hashes) => match key.code {
            KeyCode::Char('c') => {
                ui.modal = Some(Modal::Input {
                    title: "compare hash (paste MD5/SHA-256/SHA-512)".into(),
                    value: String::new(),
                    action: InputAction::HashCompare,
                });
                return;
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => {
                ui.modal = modal;
                return;
            }
        },
        None => {}
    }
    // Falling through closes the modal.
}

/// Current value of an editable service field, used to seed its inline editor.
fn current_field_value(ui: &Ui, id: ServiceId, field: EditField) -> String {
    let cfg = ui.app.config.read().unwrap();
    match field {
        EditField::Port => cfg.service(id).port.to_string(),
        EditField::Bind => cfg.service(id).bind.clone(),
        EditField::Username => cfg.auth.username.clone(),
        EditField::Password => cfg.auth.password.clone(),
        EditField::UploadDir => cfg.uploads.dir.clone(),
        _ => String::new(),
    }
}

/// Handle a non-editable (toggle) service field in the edit popup.
fn activate_service_field(ui: &mut Ui, id: ServiceId, field: EditField) {
    match field {
        EditField::StartStop => {
            if ui.app.services.status(id).is_running() {
                ui.beep(Tone::Off);
                ui.app.services.stop(id);
                ui.status_msg = Some(format!("{} stopping", id.display_name()));
            } else {
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).enabled = true;
                }
                save_config(ui);
                ui.beep(Tone::On);
                ui.app.services.start(id);
                ui.status_msg = Some(format!("{} starting", id.display_name()));
            }
        }
        EditField::Enabled => {
            let enabled = {
                let mut cfg = ui.app.config.write().unwrap();
                let sc = cfg.service_mut(id);
                sc.enabled = !sc.enabled;
                sc.enabled
            };
            save_config(ui);
            ui.beep(if enabled { Tone::On } else { Tone::Off });
            if !enabled && ui.app.services.status(id).is_running() {
                ui.app.services.stop(id);
            }
            ui.status_msg = Some(format!(
                "{} {}",
                id.display_name(),
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        EditField::Uploads => {
            let enabled = {
                let mut cfg = ui.app.config.write().unwrap();
                cfg.uploads.enabled = !cfg.uploads.enabled;
                cfg.uploads.enabled
            };
            save_config(ui);
            ui.beep(if enabled { Tone::On } else { Tone::Off });
            ui.status_msg = Some(if enabled {
                "uploads enabled — restart running services to apply".into()
            } else {
                "uploads disabled".into()
            });
        }
        _ => {}
    }
}

/// Apply an edited service field from the popup. Returns an error message on
/// failure (invalid value), otherwise `None`.
fn commit_service_edit(ui: &mut Ui, id: ServiceId, field: EditField, value: String) -> Option<String> {
    match field {
        EditField::Port => match value.trim().parse::<u16>() {
            Ok(port) if port > 0 => {
                if port < 1024 && !ui.app.privileged {
                    return Some(format!("port {port} needs root — run with sudo"));
                }
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).port = port;
                }
                save_config(ui);
                if ui.app.services.status(id).is_running() {
                    ui.app.services.restart(id);
                }
                ui.status_msg = Some(format!("{} port set to {port}", id.display_name()));
                None
            }
            _ => Some(format!("invalid port: {value}")),
        },
        EditField::Bind => match value.trim().parse::<std::net::IpAddr>() {
            Ok(_) => {
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).bind = value.trim().to_string();
                }
                save_config(ui);
                if ui.app.services.status(id).is_running() {
                    ui.app.services.restart(id);
                }
                ui.status_msg = Some(format!("{} bind set to {}", id.display_name(), value.trim()));
                None
            }
            Err(_) => Some(format!("invalid bind address: {value}")),
        },
        EditField::Username => {
            if value.trim().is_empty() {
                return Some("username must not be empty".into());
            }
            ui.app.config.write().unwrap().auth.username = value.trim().to_string();
            save_config(ui);
            restart_auth_services(ui);
            ui.status_msg = Some("username updated".into());
            None
        }
        EditField::Password => {
            if value.is_empty() {
                return Some("password must not be empty".into());
            }
            let secrets = {
                let mut cfg = ui.app.config.write().unwrap();
                cfg.auth.password = value.clone();
                cfg.config_dir.join("state").join("secrets.toml")
            };
            if let Err(e) = crate::auth::store_password(&secrets, &value) {
                return Some(format!("storing password failed: {e}"));
            }
            restart_auth_services(ui);
            ui.status_msg = Some("password updated".into());
            None
        }
        EditField::UploadDir => {
            ui.app.config.write().unwrap().uploads.dir = value.trim().trim_matches('/').to_string();
            save_config(ui);
            ui.status_msg = Some("upload directory updated".into());
            None
        }
        _ => None,
    }
}

fn apply_input(ui: &mut Ui, action: InputAction, value: String) {
    match action {
        InputAction::Port(id) => match value.trim().parse::<u16>() {
            Ok(port) if port > 0 => {
                if port < 1024 && !ui.app.privileged {
                    ui.modal = Some(Modal::Message(format!(
                        "Port {port} needs root privileges.\nRun with sudo, or use e.g. port {}.",
                        id.default_port(false)
                    )));
                    return;
                }
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).port = port;
                }
                save_config(ui);
                if ui.app.services.status(id).is_running() {
                    ui.app.services.restart(id);
                }
                ui.status_msg = Some(format!("{} port set to {port}", id.display_name()));
            }
            _ => ui.modal = Some(Modal::Message(format!("invalid port: {value}"))),
        },
        InputAction::Bind(id) => match value.trim().parse::<std::net::IpAddr>() {
            Ok(_) => {
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).bind = value.trim().to_string();
                }
                save_config(ui);
                if ui.app.services.status(id).is_running() {
                    ui.app.services.restart(id);
                }
                ui.status_msg = Some(format!("{} bind set to {}", id.display_name(), value.trim()));
            }
            Err(_) => {
                ui.modal = Some(Modal::Message(format!(
                    "invalid bind address: {value}\nUse 0.0.0.0, 127.0.0.1 or a local interface IP \
                     (see Dashboard for the list)."
                )))
            }
        },
        InputAction::Username => {
            if value.trim().is_empty() {
                ui.modal = Some(Modal::Message("username must not be empty".into()));
                return;
            }
            ui.app.config.write().unwrap().auth.username = value.trim().to_string();
            save_config(ui);
            restart_auth_services(ui);
            ui.status_msg = Some("username updated".into());
        }
        InputAction::Password => {
            if value.is_empty() {
                ui.modal = Some(Modal::Message("password must not be empty".into()));
                return;
            }
            let secrets = {
                let mut cfg = ui.app.config.write().unwrap();
                cfg.auth.password = value.clone();
                cfg.config_dir.join("state").join("secrets.toml")
            };
            if let Err(e) = crate::auth::store_password(&secrets, &value) {
                ui.status_msg = Some(format!("storing password failed: {e}"));
            }
            restart_auth_services(ui);
            ui.status_msg = Some("password updated".into());
        }
        InputAction::FileFilter => {
            ui.files.filter = value;
            ui.files.selected = 0;
        }
        InputAction::LogFilter => {
            ui.log_filter = value;
        }
        InputAction::UploadDir => {
            ui.app.config.write().unwrap().uploads.dir = value.trim().trim_matches('/').to_string();
            save_config(ui);
            ui.status_msg = Some("upload directory updated".into());
        }
        InputAction::HashCompare => {
            let result = ui.hashes.as_ref().map(|info| compare_hash(info, &value));
            if let (Some(info), Some(r)) = (ui.hashes.as_mut(), result) {
                info.compare = Some(r);
            }
            ui.modal = Some(Modal::Hashes);
        }
    }
}

/// FTP and SSH cache credentials at start time; restart them so changes apply.
fn restart_auth_services(ui: &mut Ui) {
    for id in [ServiceId::Ftp, ServiceId::Ssh] {
        if ui.app.services.status(id).is_running() {
            ui.app.services.restart(id);
        }
    }
    ui.app.logger.log(Event::new(LogLevel::Info, "core", "credentials updated"));
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut child = std::process::Command::new("pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child
        .stdin
        .as_mut()
        .ok_or("no stdin")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    child.wait().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_of_known_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        let (md5, sha256, sha512) = compute_hashes(&path).unwrap();
        assert_eq!(md5, "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha512,
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    fn info() -> HashInfo {
        HashInfo {
            rel_path: "abc.txt".into(),
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into(),
            sha512: "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
                     2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
                .into(),
            compare: None,
        }
    }

    #[test]
    fn ftp_password_is_redacted_for_the_log() {
        assert_eq!(
            redact_url_password("copy ftp://cisco:cisco123@10.0.0.1/img.bin flash:"),
            "copy ftp://cisco:***@10.0.0.1/img.bin flash:"
        );
        // Nothing to hide in the other schemes.
        let plain = "copy http://10.0.0.1:8080/img.bin flash:";
        assert_eq!(redact_url_password(plain), plain);
        let scp = "copy scp://cisco@10.0.0.1/img.bin flash:";
        assert_eq!(redact_url_password(scp), scp);
    }

    #[test]
    fn compare_detects_type_by_length() {
        let i = info();
        // MD5 (32), auto-detected and matched — even with mixed case / spaces.
        let c = compare_hash(&i, "  900150983CD24FB0D6963F7D28E17F72 ");
        assert_eq!(c.kind, "MD5");
        assert!(c.matched);
        // SHA-256 (64).
        assert_eq!(compare_hash(&i, &i.sha256).kind, "SHA-256");
        assert!(compare_hash(&i, &i.sha256).matched);
        // SHA-512 (128).
        assert_eq!(compare_hash(&i, &i.sha512).kind, "SHA-512");
        assert!(compare_hash(&i, &i.sha512).matched);
    }

    #[test]
    fn compare_rejects_wrong_and_malformed() {
        let i = info();
        // Right length, wrong value.
        let c = compare_hash(&i, "00000000000000000000000000000000");
        assert_eq!(c.kind, "MD5");
        assert!(!c.matched);
        // Not a recognised length.
        assert_eq!(compare_hash(&i, "deadbeef").kind, "?");
        // Non-hex characters.
        assert_eq!(compare_hash(&i, "zz0150983cd24fb0d6963f7d28e17f72").kind, "?");
    }
}
