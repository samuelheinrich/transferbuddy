mod intro;
mod theme;
mod views;
mod workflow;

#[cfg(test)]
mod screenshots;

use std::io::Write as _;
use std::path::PathBuf;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event as CEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;

use crate::logging::{Event, LogLevel};
use crate::services::ServiceId;
use crate::session::Protocol;
use crate::sound::Tone;
use crate::switch::{Switch, SwitchState, Target};
use crate::SharedApp;
use transferbuddy_core::engine::{Command, Credentials, Setting};

#[cfg(test)]
pub const SUPPORTED_ACTIONS: &[transferbuddy_core::engine::Action] = &[
    transferbuddy_core::engine::Action::Workflow,
    transferbuddy_core::engine::Action::Profiles,
    transferbuddy_core::engine::Action::Report,
    transferbuddy_core::engine::Action::Review,
    transferbuddy_core::engine::Action::Services,
    transferbuddy_core::engine::Action::Settings,
    transferbuddy_core::engine::Action::Root,
    transferbuddy_core::engine::Action::Connect,
    transferbuddy_core::engine::Action::BulkConnect,
    transferbuddy_core::engine::Action::CancelScan,
    transferbuddy_core::engine::Action::Reconnect,
    transferbuddy_core::engine::Action::Disconnect,
    transferbuddy_core::engine::Action::RemoveDevice,
    transferbuddy_core::engine::Action::ClearFinished,
    transferbuddy_core::engine::Action::Refresh,
    transferbuddy_core::engine::Action::Cli,
    transferbuddy_core::engine::Action::ListLocal,
    transferbuddy_core::engine::Action::ListRemote,
    transferbuddy_core::engine::Action::Hash,
    transferbuddy_core::engine::Action::CopyCommands,
    transferbuddy_core::engine::Action::Protocol,
    transferbuddy_core::engine::Action::Send,
    transferbuddy_core::engine::Action::Receive,
    transferbuddy_core::engine::Action::Queue,
    transferbuddy_core::engine::Action::Retry,
    transferbuddy_core::engine::Action::Resume,
    transferbuddy_core::engine::Action::Cancel,
    transferbuddy_core::engine::Action::Delete,
    transferbuddy_core::engine::Action::AssignImage,
    transferbuddy_core::engine::Action::Deploy,
    transferbuddy_core::engine::Action::Verify,
    transferbuddy_core::engine::Action::Install,
    transferbuddy_core::engine::Action::Yolo,
    transferbuddy_core::engine::Action::RemoveInactive,
    transferbuddy_core::engine::Action::Confirmation,
    transferbuddy_core::engine::Action::Logs,
    transferbuddy_core::engine::Action::Shutdown,
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    Connect,
    Files,
    Logs,
    Upgrade,
}

impl Tab {
    pub const ALL: [Tab; 5] = [
        Tab::Dashboard,
        Tab::Connect,
        Tab::Files,
        Tab::Upgrade,
        Tab::Logs,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Tab::Dashboard => transferbuddy_core::workspace::WorkspaceView::Dashboard.title(),
            Tab::Connect => transferbuddy_core::workspace::WorkspaceView::Connect.title(),
            Tab::Files => transferbuddy_core::workspace::WorkspaceView::Transfer.title(),
            Tab::Logs => transferbuddy_core::workspace::WorkspaceView::Logs.title(),
            Tab::Upgrade => transferbuddy_core::workspace::WorkspaceView::Upgrade.title(),
        }
    }
}

pub use transferbuddy_core::files::{FileBrowser, FileEntry, SortBy};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    Root,
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
            EditField::Port
                | EditField::Bind
                | EditField::Username
                | EditField::Password
                | EditField::UploadDir
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
    Submit,
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
    pub const ADD: [DeployField; 6] = [
        DeployField::Host,
        DeployField::Port,
        DeployField::Username,
        DeployField::Password,
        DeployField::EnablePassword,
        DeployField::Submit,
    ];
    pub const BULK: [DeployField; 6] = Self::ADD;
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
            DeployField::Submit => "",
        }
    }
    /// Rows that take typed text (the others toggle or cycle).
    pub fn is_text(self) -> bool {
        !matches!(
            self,
            DeployField::Protocol | DeployField::Overwrite | DeployField::Submit
        )
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

/// The deploy form for one file. The live session it starts lives in
/// [`crate::switch`] and is shown by [`Modal::Session`].
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DeployMode {
    Copy,
    Add,
    Bulk,
}

pub struct DeployView {
    /// File being deployed, relative to the shared root.
    pub rel_path: String,
    /// Its size, to compare against the free flash of the device.
    pub size: u64,
    pub form: DeployForm,
    /// Problem with the form, shown in red under the fields.
    pub error: Option<String>,
}

/// The live view of one switch session.
pub struct SessionView {
    pub switch: Arc<Switch>,
    /// Rows scrolled away at the top; `None` = follow the tail.
    pub scroll: Option<usize>,
    /// Number of finished jobs already announced, so each one beeps once.
    pub jobs_seen: u64,
}

pub enum Modal {
    Workflow,
    WorkflowLogin {
        device: u64,
        form: DeployForm,
    },
    CoreQuestion(transferbuddy_core::engine::Confirmation),
    Jobs {
        selected: usize,
    },
    Help,
    ConfirmQuit,
    ConfirmUploads,
    Input {
        title: String,
        value: String,
        action: InputAction,
    },
    Cisco {
        rel_path: String,
        commands: Vec<(Protocol, String)>,
        selected: usize,
        copied: bool,
    },
    Message(String),
    /// Per-service edit popup. `editing` holds the in-progress text when an
    /// editable field is being typed into.
    ServiceEdit {
        id: ServiceId,
        field: usize,
        editing: Option<String>,
    },
    /// File hash view; the data lives in `Ui::hashes` so it survives the
    /// detour through the compare-input modal.
    Hashes,
    /// Deploy form; the data lives in `Ui::deploy`.
    Deploy,
    DeployProtocol {
        selected: usize,
    },
    ConfirmStart {
        id: ServiceId,
    },
    UpgradeFiles,
    /// Live transcript of one switch session; the data lives in
    /// `Ui::session_view`.
    Session,
    Cli,
    TransferProtocol {
        options: Vec<Protocol>,
        selected: usize,
        start: bool,
        upgrade: bool,
    },
    ConfirmDelete {
        request: u64,
        switch: Arc<Switch>,
        path: String,
        recursive: bool,
    },
    UpgradeActions {
        switch: Arc<Switch>,
        selected: usize,
    },
    ConfirmInstall {
        request: u64,
        switch: Arc<Switch>,
        yolo: bool,
    },
}

pub struct TransferUi {
    /// 0 = device overview, 1 = local, 2 = remote.
    pub focus: usize,
    pub remote_cwd: String,
    pub remote_selected: usize,
    pub device: Option<u64>,
    pub requested: Option<String>,
}
impl Default for TransferUi {
    fn default() -> Self {
        Self {
            focus: 1,
            remote_cwd: String::new(),
            remote_selected: 0,
            device: None,
            requested: None,
        }
    }
}
pub struct UpgradeButton {
    pub area: Rect,
    pub switch: Arc<Switch>,
    pub action: usize,
}

pub struct Ui {
    pub app: SharedApp,
    pub workflow: workflow::Editor,
    pub tab: Tab,
    pub service_sel: usize,
    pub dashboard_transfers: bool,
    pub files: FileBrowser,
    pub transfer_ui: TransferUi,
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
    pub hash_request: Option<String>,
    /// Scroll offset of the help modal (it is taller than short terminals).
    pub help_scroll: usize,
    /// Deploy form, kept across modal opens so host and user do not have to
    /// be retyped for the next file.
    pub deploy: Option<DeployView>,
    /// The switch session shown by [`Modal::Session`].
    pub session_view: Option<SessionView>,
    /// Selected row in the switches view.
    pub switch_sel: usize,
    pub upgrade_menu: Option<usize>,
    pub upgrade_file: Option<(String, u64)>,
    pub upgrade_assignments: std::collections::HashMap<u64, (String, u64)>,
    pub upgrade_protocol: Protocol,
    pub upgrade_buttons: Vec<UpgradeButton>,
    pub deploy_mode: DeployMode,
    pub pending_deploy: Option<(ServiceId, Instant)>,
}

impl Ui {
    /// Short beep, unless sound is switched off.
    fn beep(&self, tone: Tone) {
        let enabled = self.app.config.read().unwrap().sound;
        crate::sound::play(enabled, tone);
    }
}

/// Leave the terminal usable even if something panics: without this the
/// alternate screen stays up in raw mode and the terminal looks frozen, with
/// the panic message hidden behind the last frame.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            LeaveAlternateScreen,
            DisableBracketedPaste,
            DisableMouseCapture
        );
        previous(info);
    }));
}

pub fn run(app: SharedApp) -> Result<()> {
    install_panic_hook();
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(
        stdout,
        EnterAlternateScreen,
        EnableBracketedPaste,
        EnableMouseCapture
    )?;
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
    crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: SharedApp,
) -> Result<()> {
    let mut ui = Ui {
        app,
        workflow: workflow::Editor::default(),
        tab: Tab::Dashboard,
        service_sel: 0,
        dashboard_transfers: false,
        files: FileBrowser::new(),
        transfer_ui: TransferUi::default(),
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
        hash_request: None,
        help_scroll: 0,
        deploy: None,
        session_view: None,
        switch_sel: 0,
        upgrade_menu: Some(0),
        upgrade_file: None,
        upgrade_assignments: std::collections::HashMap::new(),
        upgrade_protocol: Protocol::Http,
        upgrade_buttons: Vec::new(),
        deploy_mode: DeployMode::Copy,
        pending_deploy: None,
    };
    let root = ui.app.config.read().unwrap().root.clone();
    ui.files.refresh(&root);

    loop {
        pump_pending_deploy(&mut ui);
        pump_transfer(&mut ui);
        pump_upgrades(&mut ui);
        for switch in ui.app.switches.list() {
            switch.transfer(&ui.app.sessions);
        }
        pump_session(&mut ui);
        if matches!(ui.modal, Some(Modal::Hashes)) && ui.hashes.is_none() {
            if let Some((path, result)) = ui.hash_request.clone().and_then(|path| {
                ui.app
                    .engine()
                    .snapshot()
                    .hashes
                    .remove(&path)
                    .map(|result| (path, result))
            }) {
                match result {
                    Ok(h) => {
                        ui.hashes = Some(HashInfo {
                            rel_path: path,
                            md5: h.md5,
                            sha256: h.sha256,
                            sha512: h.sha512,
                            compare: None,
                        });
                        ui.status_msg = None;
                    }
                    Err(e) => ui.modal = Some(Modal::Message(e)),
                }
            }
        }
        workflow::tick(&mut ui);
        terminal.draw(|f| views::draw(f, &mut ui))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                CEvent::Paste(text) => handle_paste(&mut ui, text),
                CEvent::Mouse(event) => handle_mouse(&mut ui, event),
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
    if matches!(ui.modal, Some(Modal::Cli)) {
        handle_cli_key(ui, key);
        return;
    }
    if matches!(ui.modal, Some(Modal::Session))
        && ui.session_view.as_ref().is_some_and(|v| {
            matches!(
                v.switch.state(),
                SwitchState::CleanupConfirm { .. } | SwitchState::ReloadConfirm { .. }
            )
        })
    {
        handle_session_key(ui, key);
        return;
    }
    if matches!(
        ui.modal,
        Some(Modal::ConfirmDelete { .. } | Modal::ConfirmInstall { .. })
    ) {
        handle_modal_key(ui, key);
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        ui.modal = Some(Modal::ConfirmQuit);
        return;
    }
    if ui.modal.is_some() {
        handle_modal_key(ui, key);
        return;
    }
    if key.code == KeyCode::Char('W') {
        workflow::open(ui);
        return;
    }
    if let KeyCode::Char(number @ '1'..='5') = key.code {
        ui.tab = Tab::ALL[(number as u8 - b'1') as usize];
        on_tab_changed(ui);
        return;
    }
    match key.code {
        KeyCode::Char('J') => {
            ui.modal = Some(Modal::Jobs { selected: 0 });
        }
        KeyCode::Char('q') => {
            if ServiceId::ALL
                .iter()
                .any(|id| ui.app.services.status(*id).is_running())
                || ui.app.switches.list().iter().any(|s| s.state().is_live())
            {
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
            crate::sound::play(enabled, Tone::On);
            ui.status_msg = Some(if enabled { "sound on" } else { "sound off" }.into());
        }
        _ => match ui.tab {
            Tab::Dashboard => {
                if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                    ui.dashboard_transfers = !ui.dashboard_transfers;
                } else if key.code == KeyCode::Char('i') {
                    cycle_advertise(ui);
                } else if ui.dashboard_transfers {
                    handle_sessions_key(ui, key);
                } else {
                    handle_services_key(ui, key);
                }
            }
            Tab::Connect => handle_connect_key(ui, key),
            Tab::Files => handle_transfer_key(ui, key),
            Tab::Upgrade => handle_switches_key(ui, key),
            Tab::Logs => handle_logs_key(ui, key),
        },
    }
}

fn handle_mouse(ui: &mut Ui, event: MouseEvent) {
    if ui.tab != Tab::Upgrade
        || ui.modal.is_some()
        || event.kind != MouseEventKind::Down(MouseButton::Left)
    {
        return;
    }
    let point = Position::new(event.column, event.row);
    if let Some(button) = ui.upgrade_buttons.iter().find(|b| b.area.contains(point)) {
        let sw = button.switch.clone();
        let action = button.action;
        if action == usize::MAX {
            ui.modal = Some(Modal::UpgradeActions {
                switch: sw,
                selected: 0,
            });
        } else {
            upgrade_action(ui, sw, action);
        }
    }
}

fn handle_cli_key(ui: &mut Ui, key: KeyEvent) {
    let Some(view) = ui.session_view.as_ref() else {
        ui.modal = None;
        return;
    };
    if key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(']'))
    {
        let _ = ui.app.engine().submit(Command::CloseCli(view.switch.id));
        ui.modal = None;
        ui.status_msg = Some("CLI closed — back in TransferBuddy".into());
        return;
    }
    let bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() => {
            vec![(c.to_ascii_uppercase() as u8) & 0x1f]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![8],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Delete => b"\x1b[3~".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        _ => return,
    };
    let _ = ui
        .app
        .engine()
        .submit(Command::CliInput(view.switch.id, bytes));
}

fn handle_connect_key(ui: &mut Ui, key: KeyEvent) {
    if key.code == KeyCode::Delete {
        if let Some(sw) = selected_switch(ui) {
            ui.status_msg = Some(match ui.app.engine().submit(Command::RemoveDevice(sw.id)) {
                Ok(_) => "device removed".into(),
                Err(error) => error,
            });
        }
        return;
    }
    match key.code {
        KeyCode::Char('a') => activate_upgrade_action(ui, 1),
        KeyCode::Char('b') => activate_upgrade_action(ui, 2),
        KeyCode::Char('S') => {
            if let Some(scan) = ui.app.switches.scan() {
                scan.cancel();
            }
        }
        KeyCode::Up | KeyCode::Char('k') => ui.switch_sel = ui.switch_sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            ui.switch_sel = (ui.switch_sel + 1).min(ui.app.switches.list().len().saturating_sub(1))
        }
        KeyCode::Enter | KeyCode::Char('v') => {
            if let Some(sw) = selected_switch(ui) {
                open_session_view(ui, sw);
            }
        }
        KeyCode::Char('c') => {
            if let Some(sw) = selected_switch(ui) {
                if ui.app.engine().submit(Command::OpenCli(sw.id)).is_ok() {
                    open_session_view(ui, sw);
                    ui.modal = Some(Modal::Cli);
                } else {
                    ui.status_msg = Some("CLI needs an idle SSH connection".into());
                }
            }
        }
        KeyCode::Char('r') => {
            if let Some(sw) = selected_switch(ui) {
                if sw.state().is_over() {
                    let _ = ui.app.engine().submit(Command::Reconnect(sw.id));
                    open_session_view(ui, sw);
                } else {
                    let _ = ui.app.engine().submit(Command::Refresh(sw.id));
                }
            }
        }
        KeyCode::Char('x') => {
            if let Some(sw) = selected_switch(ui) {
                disconnect_switch(ui, &sw, "disconnected; r reconnects with saved credentials");
            }
        }
        KeyCode::Char('X') => {
            ui.app.switches.forget_closed();
            ui.switch_sel = 0;
        }
        KeyCode::Char('y') => {
            if let Some(sw) = selected_switch(ui) {
                sw.answer_host_key(true);
            }
        }
        _ => {}
    }
}

/// Refresh view-specific state after switching tabs (e.g. re-read the file list).
fn on_tab_changed(ui: &mut Ui) {
    if matches!(ui.tab, Tab::Files | Tab::Upgrade) {
        let root = ui.app.config.read().unwrap().root.clone();
        ui.files.refresh(&root);
    }
    if ui.tab == Tab::Upgrade
        && !ui
            .app
            .services
            .status(crate::cisco::service_of(ui.upgrade_protocol))
            .is_running()
    {
        ui.upgrade_protocol = default_deploy_protocol(ui);
    }
}

/// Step through the local addresses that can be advertised: automatic first,
/// then one entry per interface. The choice is saved, so a machine on cable
/// and Wi-Fi keeps handing out the same address after a restart.
fn cycle_advertise(ui: &mut Ui) {
    let list = crate::netif::candidates();
    if list.is_empty() {
        ui.status_msg = Some("no usable network interface".into());
        return;
    }
    let chosen = {
        let mut cfg = ui.app.config.write().unwrap();
        // 0 = automatic, 1..=n = the interfaces.
        let current = match &cfg.advertise {
            None => 0,
            Some(name) => list
                .iter()
                .position(|i| &i.name == name || i.ip.to_string() == *name)
                .map(|p| p + 1)
                .unwrap_or(0),
        };
        let next = (current + 1) % (list.len() + 1);
        cfg.advertise = if next == 0 {
            None
        } else {
            Some(list[next - 1].name.clone())
        };
        cfg.advertise.clone()
    };
    save_config(ui);
    ui.beep(Tone::Confirm);
    ui.status_msg = Some(match chosen {
        Some(name) => {
            let ip = crate::netif::resolve_advertise(&name)
                .map(|i| i.to_string())
                .unwrap_or_else(|| "?".into());
            format!("URLs now use {name} ({ip})")
        }
        None => {
            let ip = crate::netif::suggest_ip()
                .map(|i| i.to_string())
                .unwrap_or_else(|| "?".into());
            format!("URLs use the automatic choice ({ip})")
        }
    });
}

/// The address generated URLs currently use, and where it came from.
///
/// Takes the config rather than the `Ui`: the draw path already holds the
/// config lock, and taking it a second time on the same thread is how a
/// reader-writer lock deadlocks.
pub fn advertised_now(cfg: &crate::config::Config) -> (Option<std::net::IpAddr>, String) {
    let ip = cfg.advertised_ip("0.0.0.0", None);
    let source = match (&cfg.advertise, ip) {
        (Some(pin), Some(ip)) => match crate::netif::resolve_advertise(pin) {
            Some(_) => format!(
                "{} pinned",
                crate::netif::interface_of(&ip).unwrap_or_else(|| pin.clone())
            ),
            None => format!("{pin} is gone — using automatic"),
        },
        (None, Some(ip)) => format!(
            "{} automatic",
            crate::netif::interface_of(&ip).unwrap_or_else(|| "?".into())
        ),
        (_, None) => "no address".into(),
    };
    (ip, source)
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
        KeyCode::Char('o') => {
            ui.modal = Some(Modal::Input {
                title: "local transfer root".into(),
                value: ui.app.config.read().unwrap().root.display().to_string(),
                action: InputAction::Root,
            })
        }
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
                let _ = ui.app.engine().submit(Command::StopService(id));
            }
            ui.status_msg = Some(format!(
                "{} {}",
                id.display_name(),
                if enabled { "enabled" } else { "disabled" }
            ));
        }
        KeyCode::Char('s') => {
            if ui.app.services.status(id).is_running() {
                ui.beep(Tone::Off);
                let _ = ui.app.engine().submit(Command::StopService(id));
            } else {
                ui.beep(Tone::On);
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).enabled = true;
                }
                save_config(ui);
                let _ = ui.app.engine().submit(Command::StartService(id));
            }
        }
        KeyCode::Char('r') => {
            ui.beep(Tone::Confirm);
            let _ = ui.app.engine().submit(Command::RestartService(id));
        }
        KeyCode::Char('S') => {
            ui.beep(Tone::On);
            let _ = ui.app.engine().submit(Command::StartEnabled);
        }
        KeyCode::Char('X') => {
            ui.beep(Tone::Off);
            let _ = ui.app.engine().submit(Command::StopAll);
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
            ui.modal = Some(Modal::ServiceEdit {
                id,
                field: 0,
                editing: None,
            });
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
            let entry = ui
                .files
                .visible()
                .get(ui.files.selected)
                .map(|e| (*e).clone());
            if let Some(entry) = entry {
                if entry.is_dir {
                    ui.files.enter(&entry.name);
                    ui.files.refresh(&root);
                } else {
                    request_transfer(ui);
                }
            }
        }
        KeyCode::Backspace => {
            ui.files.up();
            ui.files.refresh(&root);
        }
        KeyCode::Home => {
            ui.files.cwd.clear();
            ui.files.selected = 0;
            ui.files.filter.clear();
            ui.files.refresh(&root);
        }
        KeyCode::Char('H') => {
            let entry = ui
                .files
                .visible()
                .get(ui.files.selected)
                .map(|e| (*e).clone());
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
        KeyCode::Char('d') | KeyCode::Char('D') => request_transfer(ui),
        KeyCode::Char('R') | KeyCode::F(5) => ui.files.refresh(&root),
        KeyCode::Char('y') => {
            let entry = ui
                .files
                .visible()
                .get(ui.files.selected)
                .map(|e| (*e).clone());
            if let Some(entry) = entry {
                if !entry.is_dir {
                    open_cisco_modal(ui, &entry.name);
                }
            }
        }
        _ => {}
    }
}

pub fn remote_path(ui: &Ui) -> String {
    let device = selected_switch(ui)
        .map(|s| s.facts().flash_device)
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "flash:".into());
    format!("{device}{}", ui.transfer_ui.remote_cwd)
}

fn pump_transfer(ui: &mut Ui) {
    if ui.tab != Tab::Files {
        return;
    }
    ui.switch_sel = ui
        .switch_sel
        .min(ui.app.switches.list().len().saturating_sub(1));
    let Some(sw) = selected_switch(ui) else {
        return;
    };
    if ui.transfer_ui.device != Some(sw.id) {
        ui.transfer_ui.device = Some(sw.id);
        ui.transfer_ui.remote_cwd.clear();
        ui.transfer_ui.remote_selected = 0;
        ui.transfer_ui.requested = None;
    }
    let path = remote_path(ui);
    if ui.transfer_ui.requested.as_ref() != Some(&path)
        && sw.state() == SwitchState::Ready
        && !ui.app.engine().busy(sw.id)
        && ui
            .app
            .engine()
            .submit(Command::ListRemote(sw.id, path.clone()))
            .is_ok()
    {
        ui.transfer_ui.requested = Some(path);
    }
}

fn handle_transfer_key(ui: &mut Ui, key: KeyEvent) {
    match key.code {
        KeyCode::Tab => ui.transfer_ui.focus = (ui.transfer_ui.focus + 1) % 3,
        KeyCode::BackTab => ui.transfer_ui.focus = (ui.transfer_ui.focus + 2) % 3,
        KeyCode::Left => ui.transfer_ui.focus = 1,
        KeyCode::Right => ui.transfer_ui.focus = 2,

        KeyCode::Char('v') => {
            if let Some(sw) = selected_switch(ui) {
                open_session_view(ui, sw);
            }
        }
        KeyCode::Char('x') => {
            if let Some(sw) = selected_switch(ui) {
                disconnect_switch(ui, &sw, "disconnecting device");
            }
        }
        KeyCode::Delete | KeyCode::Char('D') if ui.transfer_ui.focus == 2 => {
            confirm_remote_delete(ui)
        }
        KeyCode::Char('t') => request_transfer(ui),
        KeyCode::Char('p') => open_transfer_protocol(ui, false, false),
        _ if ui.transfer_ui.focus == 0 => {
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => ui.switch_sel = ui.switch_sel.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    ui.switch_sel =
                        (ui.switch_sel + 1).min(ui.app.switches.list().len().saturating_sub(1))
                }
                KeyCode::Enter => open_transfer_protocol(ui, false, false),
                _ => {}
            }
            pump_transfer(ui);
        }
        _ if ui.transfer_ui.focus == 2 => {
            let entries = selected_switch(ui)
                .and_then(|s| s.listing(&remote_path(ui)))
                .and_then(Result::ok)
                .unwrap_or_default();
            let has_parent = !ui.transfer_ui.remote_cwd.is_empty();
            let count = entries.len() + usize::from(has_parent);
            match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    ui.transfer_ui.remote_selected =
                        ui.transfer_ui.remote_selected.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    ui.transfer_ui.remote_selected =
                        (ui.transfer_ui.remote_selected + 1).min(count.saturating_sub(1))
                }
                KeyCode::Home => {
                    ui.transfer_ui.remote_cwd.clear();
                    ui.transfer_ui.remote_selected = 0;
                    ui.transfer_ui.requested = None;
                }
                KeyCode::Enter if has_parent && ui.transfer_ui.remote_selected == 0 => {
                    remote_up(ui)
                }
                KeyCode::Enter => {
                    if let Some(entry) = entries.get(
                        ui.transfer_ui
                            .remote_selected
                            .saturating_sub(usize::from(has_parent)),
                    ) {
                        if entry.is_dir {
                            ui.transfer_ui.remote_cwd = if has_parent {
                                format!("{}/{}", ui.transfer_ui.remote_cwd, entry.name)
                            } else {
                                entry.name.clone()
                            };
                            ui.transfer_ui.remote_selected = 0;
                            ui.transfer_ui.requested = None;
                        } else {
                            request_transfer(ui);
                        }
                    }
                }
                KeyCode::Backspace => remote_up(ui),
                KeyCode::Char('R') | KeyCode::F(5) => ui.transfer_ui.requested = None,
                _ => {}
            }
            pump_transfer(ui);
        }
        _ => handle_files_key(ui, key),
    }
}
fn remote_up(ui: &mut Ui) {
    ui.transfer_ui.remote_cwd = ui
        .transfer_ui
        .remote_cwd
        .rsplit_once('/')
        .map(|(p, _)| p.into())
        .unwrap_or_default();
    ui.transfer_ui.remote_selected = 0;
    ui.transfer_ui.requested = None;
}

const TRANSFER_PROTOCOL_PRIORITY: [Protocol; 6] = [
    Protocol::Sftp,
    Protocol::Https,
    Protocol::Ftp,
    Protocol::Http,
    Protocol::Scp,
    Protocol::Tftp,
];
fn protocol_options(ui: &Ui, receive: bool) -> Vec<Protocol> {
    transferbuddy_core::engine::protocol_options(&ui.app, receive)
}

fn confirm_remote_delete(ui: &mut Ui) {
    let Some(sw) = selected_switch(ui) else {
        return;
    };
    if sw.state() != SwitchState::Ready {
        ui.status_msg = Some("deletion requires an idle SSH session".into());
        return;
    }
    let path = remote_path(ui);
    let has_parent = !ui.transfer_ui.remote_cwd.is_empty();
    if has_parent && ui.transfer_ui.remote_selected == 0 {
        ui.status_msg = Some("select a file or directory, not ..".into());
        return;
    }
    let Some(entry) = sw.listing(&path).and_then(Result::ok).and_then(|entries| {
        entries
            .get(
                ui.transfer_ui
                    .remote_selected
                    .saturating_sub(usize::from(has_parent)),
            )
            .cloned()
    }) else {
        return;
    };
    let path = format!(
        "{}{}{}",
        path,
        if ui.transfer_ui.remote_cwd.is_empty() {
            ""
        } else {
            "/"
        },
        entry.name
    );
    if let Err(error) = crate::deploy::check_delete_path(&path) {
        ui.status_msg = Some(error);
        return;
    }
    match ui.app.engine().submit(Command::RequestDelete {
        device: sw.id,
        remote: path.clone(),
        recursive: entry.is_dir,
    }) {
        Ok(request) => {
            ui.modal = Some(Modal::ConfirmDelete {
                request,
                switch: sw,
                path,
                recursive: entry.is_dir,
            })
        }
        Err(e) => ui.status_msg = Some(e),
    }
}

fn open_transfer_protocol(ui: &mut Ui, start: bool, upgrade: bool) {
    let receive = !upgrade && ui.transfer_ui.focus == 2;
    let options = protocol_options(ui, receive);
    let selected = if upgrade {
        options
            .iter()
            .position(|p| *p == ui.upgrade_protocol)
            .unwrap_or(0)
    } else {
        selected_switch(ui)
            .filter(|s| s.protocol_chosen())
            .and_then(|s| options.iter().position(|p| *p == s.protocol()))
            .unwrap_or(0)
    };
    ui.modal = Some(Modal::TransferProtocol {
        options,
        selected,
        start,
        upgrade,
    });
}
fn request_transfer(ui: &mut Ui) {
    let Some(sw) = selected_switch(ui) else {
        ui.status_msg = Some("connect a device in 2 Connect first".into());
        return;
    };
    if !sw.protocol_chosen()
        || !ui
            .app
            .services
            .status(crate::cisco::service_of(sw.protocol()))
            .is_running()
        || (ui.transfer_ui.focus == 2 && sw.protocol() != Protocol::Ftp)
    {
        open_transfer_protocol(ui, true, false);
    } else {
        prepare_transfer(ui);
    }
}

fn prepare_transfer(ui: &mut Ui) {
    let Some(sw) = selected_switch(ui) else {
        ui.status_msg = Some("connect a device in 2 Connect first".into());
        return;
    };
    let receive = ui.transfer_ui.focus == 2;
    let name = if receive {
        let parent = usize::from(!ui.transfer_ui.remote_cwd.is_empty());
        if parent == 1 && ui.transfer_ui.remote_selected == 0 {
            return;
        }
        sw.listing(&remote_path(ui))
            .and_then(Result::ok)
            .and_then(|e| {
                e.get(ui.transfer_ui.remote_selected.saturating_sub(parent))
                    .filter(|e| !e.is_dir)
                    .map(|e| e.name.clone())
            })
    } else if ui.transfer_ui.focus == 1 {
        ui.files
            .visible()
            .get(ui.files.selected)
            .filter(|e| !e.is_dir)
            .map(|e| e.name.clone())
    } else {
        None
    };
    let Some(name) = name else {
        ui.status_msg = Some("select a file".into());
        return;
    };
    let local = if ui.files.cwd.is_empty() {
        name.clone()
    } else {
        format!("{}/{name}", ui.files.cwd)
    };
    let remote = format!(
        "{}{}{name}",
        remote_path(ui),
        if ui.transfer_ui.remote_cwd.is_empty() {
            ""
        } else {
            "/"
        }
    );
    match ui.app.engine().submit(Command::Transfer {
        device: sw.id,
        local,
        remote,
        receive,
    }) {
        Ok(_) => {
            ui.transfer_ui.requested = None;
            open_session_view(ui, sw);
        }
        Err(e) => ui.status_msg = Some(e),
    }
}
fn open_cisco_modal(ui: &mut Ui, name: &str) {
    let rel = if ui.files.cwd.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", ui.files.cwd, name)
    };
    let commands = match transferbuddy_core::engine::copy_commands(&ui.app, &rel) {
        Ok(commands) => commands,
        Err(e) => {
            ui.status_msg = Some(e);
            return;
        }
    };
    ui.modal = Some(Modal::Cisco {
        rel_path: rel,
        commands,
        selected: 0,
        copied: false,
    });
}

fn open_hashes_modal(ui: &mut Ui, _root: &PathBuf, name: &str) {
    let rel = if ui.files.cwd.is_empty() {
        name.into()
    } else {
        format!("{}/{name}", ui.files.cwd)
    };
    ui.hash_request = Some(rel.clone());
    match ui.app.engine().submit(Command::Hash(rel)) {
        Ok(_) => {
            ui.hashes = None;
            ui.modal = Some(Modal::Hashes);
            ui.status_msg = Some("hashing in background…".into());
        }
        Err(e) => ui.status_msg = Some(e),
    }
}

/// Stream a file once through all three digests. Cisco images can be hundreds
/// of megabytes, so read in chunks rather than loading the whole file.
#[cfg(test)]
fn compute_hashes(path: &std::path::Path) -> std::io::Result<(String, String, String)> {
    transferbuddy_core::files::compute_hashes(path)
}

/// Compare a user-supplied digest against the computed ones. The hash type is
/// detected from its length (32/64/128 hex chars = MD5/SHA-256/SHA-512) so the
/// user only has to paste a value.
fn compare_hash(info: &HashInfo, input: &str) -> HashCompare {
    let norm: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_lowercase();
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
    HashCompare {
        input: norm,
        kind,
        matched,
    }
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
#[cfg(test)]
fn open_deploy_modal(ui: &mut Ui, name: &str, size: u64) {
    ui.deploy_mode = DeployMode::Copy;
    ui.pending_deploy = None;
    let rel = if ui.files.cwd.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", ui.files.cwd, name)
    };
    let mut form = ui
        .deploy
        .as_ref()
        .map(|d| d.form.clone())
        .unwrap_or_default();
    form.password.clear();
    form.enable_password.clear();
    form.field = 0;
    if form.username.is_empty() {
        form.proto = default_deploy_protocol(ui);
    }
    ui.deploy = Some(DeployView {
        rel_path: rel,
        size,
        form,
        error: None,
    });
    ui.modal = Some(Modal::Deploy);
}

/// Prefer a protocol whose service is already running, then an enabled one.
fn default_deploy_protocol(ui: &Ui) -> Protocol {
    let cfg = ui.app.config.read().unwrap();
    for proto in TRANSFER_PROTOCOL_PRIORITY {
        if ui
            .app
            .services
            .status(crate::cisco::service_of(proto))
            .is_running()
        {
            return proto;
        }
    }
    for proto in TRANSFER_PROTOCOL_PRIORITY {
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
    let ip = cfg
        .advertised_ip(&cfg.service(id).bind, peer.as_ref())
        .ok_or_else(|| "no local IP address to advertise to the switch".to_string())?;
    crate::cisco::deploy_command(form.proto, &cfg, &ip, rel, &form.dest)
}

/// Show a switch session, following its tail.
fn open_session_view(ui: &mut Ui, switch: Arc<Switch>) {
    ui.session_view = Some(SessionView {
        jobs_seen: switch.jobs_done(),
        switch,
        scroll: None,
    });
    ui.modal = Some(Modal::Session);
}

/// Validate the form, open (or reuse) the session and queue the copy.
fn start_deploy(ui: &mut Ui) {
    if ui.deploy_mode != DeployMode::Copy {
        add_devices(ui);
        return;
    }
    let Some(view) = ui.deploy.as_ref() else {
        return;
    };
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
    let username = form.username.trim().to_string();
    if username.is_empty() {
        return fail(ui, "enter the username for the switch".into());
    }
    // An open session already holds the credentials.
    let existing = ui.app.switches.find_live(&host, port, &username);
    if existing.is_none() && form.password.is_empty() {
        return fail(ui, "enter the password for the switch".into());
    }

    let id = crate::cisco::service_of(form.proto);
    if !ui.app.services.status(id).is_running() {
        // The form remains intact while the user decides and the listener binds.
        ui.modal = Some(Modal::ConfirmStart { id });
        return;
    }
    if existing.as_ref().is_some_and(|s| {
        matches!(
            s.state(),
            SwitchState::Busy { .. } | SwitchState::CleanupConfirm { .. }
        )
    }) {
        return fail(
            ui,
            "this device is busy — wait for its current job to finish".into(),
        );
    }

    let command = match deploy_command_for(ui, &form, &rel) {
        Ok(c) => c,
        Err(e) => return fail(ui, e),
    };

    let switch = match existing {
        Some(sw) => sw,
        None => {
            let known_hosts = ui
                .app
                .config
                .read()
                .unwrap()
                .config_dir
                .join("state")
                .join("known_hosts");
            ui.app.switches.connect(Target {
                host: host.clone(),
                port,
                username,
                password: form.password.clone(),
                enable_password: form.enable_password.clone(),
                known_hosts,
                auto_trust: ui.app.config.read().unwrap().auto_accept_host_keys,
            })
        }
    };
    switch.choose_protocol(form.proto);
    let remote = if form.dest.ends_with(':') || form.dest.ends_with('/') {
        format!("{}{}", form.dest, rel.rsplit('/').next().unwrap_or(&rel))
    } else {
        form.dest.clone()
    };
    if let Err(error) = ui.app.engine().submit(Command::QueueTransfers(vec![
        transferbuddy_core::engine::TransferRequest {
            device: switch.id,
            local: rel.clone(),
            remote,
            receive: false,
            protocol: form.proto,
            overwrite: form.overwrite,
            platform_check: ui.tab == Tab::Upgrade,
        },
    ])) {
        return fail(ui, error);
    }

    let mut ev = Event::new(LogLevel::Info, "deploy", format!("start on {host}"))
        .path(rel)
        .result(command.clone());
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        ev = ev.ip(ip);
    }
    ui.app.logger.log(ev);

    if let Some(v) = ui.deploy.as_mut() {
        v.error = None;
        // The session holds them now; the form must not.
        v.form.password.clear();
        v.form.enable_password.clear();
    }
    open_session_view(ui, switch);
}

/// Announce finished jobs of the session on screen once each.
fn pump_session(ui: &mut Ui) {
    let Some(view) = ui.session_view.as_mut() else {
        return;
    };
    if matches!(ui.modal, Some(Modal::Cli)) && !view.switch.cli_open() {
        ui.modal = None;
        ui.status_msg = Some("CLI session ended — back in TransferBuddy".into());
    }
    let done = view.switch.jobs_done();
    if done == view.jobs_seen {
        return;
    }
    view.jobs_seen = done;
    let (result, host, name) = (
        view.switch.last_result(),
        view.switch.host.clone(),
        view.switch.display_name(),
    );
    let Some(result) = result else { return };
    let (level, tone) = match &result {
        Ok(_) => (LogLevel::Info, Tone::Confirm),
        Err(_) => (LogLevel::Error, Tone::Error),
    };
    let ev = Event::new(level, "deploy", format!("finished on {name}")).path(host);
    let ev = match &result {
        Ok(summary) => ev.result(summary.clone()),
        Err(e) => ev.error(e.clone()),
    };
    ui.app.logger.log(ev);
    ui.beep(tone);
    if ui.tab == Tab::Files {
        let root = ui.app.config.read().unwrap().root.clone();
        ui.files.refresh(&root);
        ui.transfer_ui.requested = None;
    }
}

pub fn deploy_fields(mode: DeployMode) -> &'static [DeployField] {
    if mode == DeployMode::Copy {
        &DeployField::ALL
    } else if mode == DeployMode::Bulk {
        &DeployField::BULK
    } else {
        &DeployField::ADD
    }
}

fn start_deploy_service(ui: &mut Ui, id: ServiceId) {
    if ui.app.services.status(id).is_running() {
        return;
    }
    ui.app.config.write().unwrap().service_mut(id).enabled = true;
    save_config(ui);
    let _ = ui.app.engine().submit(Command::StartService(id));
    ui.status_msg = Some(format!("{} starting", id.display_name()));
}

fn pump_pending_deploy(ui: &mut Ui) {
    let Some((id, started)) = ui.pending_deploy else {
        return;
    };
    if !matches!(ui.modal, Some(Modal::Deploy)) {
        ui.pending_deploy = None;
        return;
    }
    match ui.app.services.status(id) {
        crate::services::ServiceStatus::Running => {
            ui.pending_deploy = None;
            start_deploy(ui);
        }
        crate::services::ServiceStatus::Failed(error) => {
            ui.pending_deploy = None;
            if let Some(view) = ui.deploy.as_mut() {
                view.error = Some(format!("server could not start: {error}"));
            }
        }
        _ if started.elapsed() > Duration::from_secs(15) => {
            ui.pending_deploy = None;
            if let Some(view) = ui.deploy.as_mut() {
                view.error = Some("server did not start within 15s — retry with Ctrl+s".into());
            }
        }
        _ => {}
    }
}

/// Validate every entry before opening any connection. Keep the input order
/// while removing duplicates so bulk pastes cannot add the same device twice.
#[derive(Debug, PartialEq, Eq)]
#[cfg(test)]
struct BulkTargets {
    direct: Vec<String>,
    subnets: Vec<String>,
}
#[cfg(test)]
fn bulk_targets(input: &str) -> Result<BulkTargets, String> {
    let targets = transferbuddy_core::engine::bulk_targets(input)?;
    Ok(BulkTargets {
        direct: targets.direct,
        subnets: targets.subnets,
    })
}
#[cfg(test)]
fn bulk_hosts(input: &str) -> Result<Vec<String>, String> {
    Ok(bulk_targets(input)?.direct)
}

fn add_devices(ui: &mut Ui) {
    let Some(view) = ui.deploy.as_ref() else {
        return;
    };
    let form = view.form.clone();
    let port = match form.port.trim().parse::<u16>() {
        Ok(p) if p > 0 => p,
        _ => {
            ui.deploy.as_mut().unwrap().error = Some("invalid SSH port".into());
            return;
        }
    };
    let credentials = Credentials {
        username: form.username,
        password: form.password,
        enable_password: form.enable_password,
    };
    let command = if ui.deploy_mode == DeployMode::Bulk {
        Command::BulkConnect {
            targets: form.host,
            port,
            credentials,
        }
    } else {
        Command::Connect {
            host: form.host,
            port,
            credentials,
            bulk: false,
        }
    };
    match ui.app.engine().submit(command) {
        Ok(_) => {
            ui.deploy.as_mut().unwrap().form.password.clear();
            ui.deploy.as_mut().unwrap().form.enable_password.clear();
            ui.modal = None;
            ui.tab = Tab::Connect;
            if ui.deploy_mode == DeployMode::Add {
                if let Some(sw) = ui.app.switches.list().last().cloned() {
                    open_session_view(ui, sw);
                }
            }
            ui.status_msg =
                Some("connecting — failures appear in Logs; S cancels subnet discovery".into());
        }
        Err(error) => ui.deploy.as_mut().unwrap().error = Some(error),
    }
}

fn handle_paste(ui: &mut Ui, text: String) {
    if matches!(ui.modal, Some(Modal::Workflow)) {
        workflow::paste(ui, text);
        return;
    }
    if let Some(Modal::WorkflowLogin { form, .. }) = &mut ui.modal {
        match form.field {
            1 => &mut form.password,
            2 => &mut form.enable_password,
            _ => &mut form.username,
        }
        .push_str(&text.replace(['\r', '\n'], ""));
        return;
    }
    if matches!(ui.modal, Some(Modal::CoreQuestion(_))) {
        ui.status_msg = Some("Paste cannot approve a confirmation".into());
        return;
    }
    if matches!(
        ui.modal,
        Some(Modal::ConfirmDelete { .. } | Modal::ConfirmInstall { .. })
    ) {
        ui.modal = None;
        return;
    }

    if matches!(ui.modal, Some(Modal::Cli)) {
        if let Some(view) = ui.session_view.as_ref() {
            view.switch.cli_send(text.replace('\n', "\r").into_bytes());
        }
        return;
    }
    if matches!(ui.modal, Some(Modal::Session)) {
        if let Some(view) = &ui.session_view {
            if matches!(
                view.switch.state(),
                SwitchState::CleanupConfirm { .. } | SwitchState::ReloadConfirm { .. }
            ) {
                view.switch.answer_cleanup(false);
                view.switch.answer_reload(false);
                return;
            }
        }
    }
    match &ui.modal {
        Some(Modal::Deploy) => {
            if let Some(view) = ui.deploy.as_mut() {
                let field = deploy_fields(ui.deploy_mode)[view.form.field];
                if field.is_text() {
                    let text = if ui.deploy_mode == DeployMode::Bulk && field == DeployField::Host {
                        text
                    } else {
                        text.replace(['\n', '\r'], "")
                    };
                    field_buffer(&mut view.form, field).push_str(&text);
                    view.error = None;
                }
            }
        }
        Some(Modal::Input { .. }) => {
            if let Some(Modal::Input { value, .. }) = ui.modal.as_mut() {
                value.push_str(&text.replace(['\n', '\r'], ""));
            }
        }
        Some(Modal::ServiceEdit { .. }) => {
            if let Some(Modal::ServiceEdit {
                editing: Some(value),
                ..
            }) = ui.modal.as_mut()
            {
                value.push_str(&text.replace(['\n', '\r'], ""));
            }
        }
        _ => {}
    }
}

fn handle_upgrade_files_key(ui: &mut Ui, key: KeyEvent) {
    let root = ui.app.config.read().unwrap().root.clone();
    match key.code {
        KeyCode::Esc => ui.modal = None,
        KeyCode::Enter => {
            let entry = ui
                .files
                .visible()
                .get(ui.files.selected)
                .map(|e| (e.name.clone(), e.is_dir, e.size));
            if let Some((name, is_dir, size)) = entry {
                if is_dir {
                    ui.files.enter(&name);
                    ui.files.refresh(&root);
                } else {
                    let rel = if ui.files.cwd.is_empty() {
                        name
                    } else {
                        format!("{}/{name}", ui.files.cwd)
                    };
                    ui.upgrade_file = Some((rel, size));
                    ui.modal = None;
                    ui.status_msg =
                        Some("file selected — d deploys it to the selected device".into());
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => ui.files.selected = ui.files.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            ui.files.selected =
                (ui.files.selected + 1).min(ui.files.visible().len().saturating_sub(1))
        }
        KeyCode::Backspace => {
            ui.files.up();
            ui.files.refresh(&root);
        }
        KeyCode::Home => {
            ui.files.cwd.clear();
            ui.files.selected = 0;
            ui.files.filter.clear();
            ui.files.refresh(&root);
        }
        KeyCode::Char('R') => ui.files.refresh(&root),
        _ => {}
    }
}

fn handle_deploy_key(ui: &mut Ui, key: KeyEvent) {
    let Some(view) = ui.deploy.as_mut() else {
        ui.modal = None;
        return;
    };
    if ui.pending_deploy.is_some() && key.code != KeyCode::Esc {
        return;
    }
    let fields = deploy_fields(ui.deploy_mode);
    let cur = fields[view.form.field.min(fields.len() - 1)];
    match key.code {
        KeyCode::Esc => {
            ui.modal = None;
            ui.pending_deploy = None;
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => start_deploy(ui),
        KeyCode::Enter if cur == DeployField::Protocol => {
            ui.modal = Some(Modal::DeployProtocol {
                selected: DEPLOY_PROTOCOLS
                    .iter()
                    .position(|p| *p == view.form.proto)
                    .unwrap_or(0),
            });
        }
        KeyCode::Enter if ui.deploy_mode != DeployMode::Copy && cur != DeployField::Submit => {
            view.form.field = (view.form.field + 1).min(fields.len() - 1);
        }
        KeyCode::Enter => start_deploy(ui),
        KeyCode::Char('s')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && ui.deploy_mode == DeployMode::Copy =>
        {
            let id = crate::cisco::service_of(view.form.proto);
            start_deploy_service(ui, id);
        }
        KeyCode::Up | KeyCode::BackTab => view.form.field = view.form.field.saturating_sub(1),
        KeyCode::Down | KeyCode::Tab => {
            view.form.field = (view.form.field + 1).min(fields.len() - 1)
        }
        KeyCode::Left | KeyCode::Right | KeyCode::Char(' ')
            if matches!(cur, DeployField::Protocol | DeployField::Overwrite) =>
        {
            match cur {
                DeployField::Overwrite => view.form.overwrite = !view.form.overwrite,
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

fn handle_session_key(ui: &mut Ui, key: KeyEvent) {
    let Some(view) = ui.session_view.as_mut() else {
        ui.modal = None;
        return;
    };
    let switch = view.switch.clone();
    if let SwitchState::HostKey { .. } = switch.state() {
        // Anything other than yes is a no — and a no must reach the waiting
        // session, not just close the popup.
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => switch.answer_host_key(true),
            KeyCode::Esc | KeyCode::Char('q') => {
                switch.cancel();
                ui.modal = None;
                ui.status_msg = Some("host key rejected — session aborted".into());
            }
            _ => {
                switch.answer_host_key(false);
                ui.status_msg = Some("host key rejected — session aborted".into());
            }
        }
        return;
    }
    if matches!(switch.state(), SwitchState::ReloadConfirm { .. }) {
        switch.answer_reload(key.code == KeyCode::Char('y') && key.modifiers.is_empty());
        return;
    }
    if matches!(switch.state(), SwitchState::CleanupConfirm { .. }) {
        switch.answer_cleanup(key.code == KeyCode::Char('y') && key.modifiers.is_empty());
        return;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => ui.modal = None,
        KeyCode::Char('R') if switch.state().is_over() => {
            let _ = ui.app.engine().submit(Command::Reconnect(switch.id));
        }
        KeyCode::Char('i') => start_cleanup(ui, switch),
        KeyCode::Char('c') => {
            disconnect_switch(ui, &switch, "cancelled — the session is closed with it");
        }
        KeyCode::Char('r') => {
            if ui.app.engine().submit(Command::Refresh(switch.id)).is_ok() {
                ui.status_msg = Some("re-reading dir and show version".into());
            }
        }
        KeyCode::Char('x') => disconnect_switch(ui, &switch, "disconnecting"),
        _ => scroll_session(view, key),
    }
}

/// Stop a session whatever it is doing: a queued `exit` only helps a session
/// that is idle, so the abort flag is set as well.
fn disconnect_switch(ui: &mut Ui, switch: &Arc<Switch>, msg: &str) {
    match ui.app.engine().submit(Command::Disconnect(switch.id)) {
        Ok(_) => ui.status_msg = Some(msg.into()),
        Err(e) => ui.status_msg = Some(e),
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
        DeployField::Protocol | DeployField::Overwrite | DeployField::Submit => &mut form.dest,
    }
}

fn scroll_session(view: &mut SessionView, key: KeyEvent) {
    let len = view.switch.transcript().len();
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
            view.scroll = if cur + 15 >= len {
                None
            } else {
                Some(cur + 15)
            };
        }
        KeyCode::End | KeyCode::Char('G') => view.scroll = None,
        _ => {}
    }
}

/// The switch the cursor is on in the switches view.
fn selected_switch(ui: &Ui) -> Option<Arc<Switch>> {
    let list = ui.app.switches.list();
    list.get(ui.switch_sel.min(list.len().saturating_sub(1)))
        .cloned()
}

fn start_cleanup(ui: &mut Ui, switch: Arc<Switch>) {
    match ui.app.engine().submit(Command::RemoveInactive(switch.id)) {
        Ok(_) => open_session_view(ui, switch),
        Err(e) => ui.status_msg = Some(e),
    }
}
fn activate_upgrade_action(ui: &mut Ui, action: usize) {
    match action {
        0 => {
            let root = ui.app.config.read().unwrap().root.clone();
            ui.files.refresh(&root);
            ui.modal = Some(Modal::UpgradeFiles);
        }
        1 | 2 => {
            ui.deploy_mode = if action == 1 {
                DeployMode::Add
            } else {
                DeployMode::Bulk
            };
            let mut form = DeployForm::default();
            if let Some(previous) = &ui.deploy {
                form.username = previous.form.username.clone();
            }
            ui.deploy = Some(DeployView {
                rel_path: String::new(),
                size: 0,
                form,
                error: None,
            });
            ui.modal = Some(Modal::Deploy);
        }
        3 => deploy_upgrade(ui),
        4 => {
            if let Some(sw) = selected_switch(ui) {
                start_cleanup(ui, sw);
            }
        }
        _ => {}
    }
}
fn selected_local_file(ui: &Ui) -> Option<(String, u64)> {
    let entries = ui.files.visible();
    let entry = entries.get(ui.files.selected)?;
    if entry.is_dir {
        return None;
    }
    Some((
        if ui.files.cwd.is_empty() {
            entry.name.clone()
        } else {
            format!("{}/{}", ui.files.cwd, entry.name)
        },
        entry.size,
    ))
}
pub const UPGRADE_JOB_ACTIONS: [&str; 5] = [
    "d: Deploy + MD5",
    "V: Verify existing image",
    "u: Upgrade",
    "Y: YOLO upgrade",
    "i: Remove inactive",
];
fn confirm_install(ui: &mut Ui, sw: Arc<Switch>, yolo: bool) {
    match ui.app.engine().submit(Command::RequestInstall {
        device: sw.id,
        yolo,
    }) {
        Ok(request) => {
            ui.modal = Some(Modal::ConfirmInstall {
                request,
                switch: sw,
                yolo,
            })
        }
        Err(e) => ui.status_msg = Some(e),
    }
}
fn upgrade_action(ui: &mut Ui, sw: Arc<Switch>, action: usize) {
    if let Some(index) = ui.app.switches.list().iter().position(|s| s.id == sw.id) {
        ui.switch_sel = index;
    } else {
        return;
    }
    match action {
        0 => deploy_upgrade(ui),
        1 => verify_existing_upgrade(ui),
        2 => confirm_install(ui, sw, false),
        3 => confirm_install(ui, sw, true),
        4 => start_cleanup(ui, sw),
        _ => {}
    }
}
fn verify_existing_upgrade(ui: &mut Ui) {
    let Some(sw) = selected_switch(ui) else {
        return;
    };
    let Some(file) = ui
        .upgrade_assignments
        .get(&sw.id)
        .cloned()
        .or_else(|| ui.upgrade_file.clone())
        .or_else(|| selected_local_file(ui))
    else {
        ui.status_msg = Some("assign a local image for comparison first".into());
        return;
    };
    assign_upgrade(ui, &sw, file);
    match ui.app.engine().submit(Command::Verify(sw.id)) {
        Ok(_) => open_session_view(ui, sw),
        Err(e) => ui.status_msg = Some(e),
    }
}

fn handle_switches_key(ui: &mut Ui, key: KeyEvent) {
    match key.code {
        KeyCode::Tab | KeyCode::BackTab => {
            ui.upgrade_menu = if ui.upgrade_menu.is_some() {
                None
            } else {
                Some(0)
            }
        }
        KeyCode::Char('p') => open_transfer_protocol(ui, false, true),
        KeyCode::Char('f') => activate_upgrade_action(ui, 0),
        KeyCode::Char('A') => {
            if let Some(file) = ui.upgrade_file.clone().or_else(|| selected_local_file(ui)) {
                for sw in ui.app.switches.list() {
                    assign_upgrade(ui, &sw, file.clone());
                }
                ui.status_msg = Some("file assigned to all devices".into());
            }
        }
        KeyCode::Char('a') => {
            if let (Some(file), Some(sw)) = (
                ui.upgrade_file.clone().or_else(|| selected_local_file(ui)),
                selected_switch(ui),
            ) {
                assign_upgrade(ui, &sw, file);
                ui.status_msg = Some("file assigned to selected device".into());
            }
        }
        KeyCode::Char('d') => deploy_upgrade(ui),
        KeyCode::Char('i') => {
            if let Some(sw) = selected_switch(ui) {
                start_cleanup(ui, sw);
            }
        }
        KeyCode::Char('V') => verify_existing_upgrade(ui),
        KeyCode::Char('u') | KeyCode::Char('Y') => {
            if let Some(sw) = selected_switch(ui) {
                confirm_install(ui, sw, key.code == KeyCode::Char('Y'));
            }
        }
        _ if ui.upgrade_menu.is_some() => {
            if key.code == KeyCode::Enter {
                if let Some(file) = selected_local_file(ui) {
                    ui.upgrade_file = Some(file);
                    ui.upgrade_menu = None;
                } else {
                    handle_upgrade_browser(ui, key);
                }
            } else {
                handle_upgrade_browser(ui, key);
            }
        }
        KeyCode::Up | KeyCode::Char('k') => ui.switch_sel = ui.switch_sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            ui.switch_sel = (ui.switch_sel + 1).min(ui.app.switches.list().len().saturating_sub(1))
        }
        KeyCode::Enter => {
            if let Some(sw) = selected_switch(ui) {
                let selected = if crate::upgrade::install_blocker(
                    &sw.upgrade(),
                    &sw.facts().version.unwrap_or_default(),
                )
                .is_none()
                {
                    2
                } else {
                    0
                };
                ui.modal = Some(Modal::UpgradeActions {
                    switch: sw,
                    selected,
                });
            }
        }
        KeyCode::Char('v') => {
            if let Some(sw) = selected_switch(ui) {
                open_session_view(ui, sw);
            }
        }
        _ => {}
    }
}
fn handle_upgrade_browser(ui: &mut Ui, key: KeyEvent) {
    let root = ui.app.config.read().unwrap().root.clone();
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => ui.files.selected = ui.files.selected.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => {
            ui.files.selected =
                (ui.files.selected + 1).min(ui.files.visible().len().saturating_sub(1))
        }
        KeyCode::Enter => {
            let entry = ui
                .files
                .visible()
                .get(ui.files.selected)
                .map(|e| (*e).clone());
            if let Some(entry) = entry {
                if entry.is_dir {
                    ui.files.enter(&entry.name);
                    ui.files.refresh(&root);
                }
            }
        }
        KeyCode::Backspace => {
            ui.files.up();
            ui.files.refresh(&root);
        }
        KeyCode::Home => {
            ui.files.cwd.clear();
            ui.files.selected = 0;
            ui.files.filter.clear();
            ui.files.refresh(&root);
        }
        KeyCode::Char('R') | KeyCode::F(5) => ui.files.refresh(&root),
        KeyCode::Char('/') => {
            ui.modal = Some(Modal::Input {
                title: "filter files".into(),
                value: ui.files.filter.clone(),
                action: InputAction::FileFilter,
            })
        }
        _ => {}
    }
}
fn assign_upgrade(ui: &mut Ui, switch: &Arc<Switch>, file: (String, u64)) {
    match ui.app.engine().submit(Command::AssignImage {
        devices: vec![switch.id],
        local: file.0.clone(),
    }) {
        Ok(_) => {
            ui.upgrade_assignments.insert(switch.id, file);
        }
        Err(e) => ui.status_msg = Some(e),
    }
}

fn deploy_upgrade(ui: &mut Ui) {
    let Some(sw) = selected_switch(ui) else {
        return;
    };
    let Some(file) = ui
        .upgrade_assignments
        .get(&sw.id)
        .cloned()
        .or_else(|| ui.upgrade_file.clone())
        .or_else(|| selected_local_file(ui))
    else {
        ui.status_msg = Some("assign a local image first".into());
        return;
    };
    assign_upgrade(ui, &sw, file);
    match ui
        .app
        .engine()
        .submit(Command::Deploy(sw.id, ui.upgrade_protocol))
    {
        Ok(_) => open_session_view(ui, sw),
        Err(e) => ui.status_msg = Some(e),
    }
}
fn pump_upgrades(_ui: &mut Ui) { /* Jobs are driven by the shared async engine. */
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
            let i = LogLevel::ALL
                .iter()
                .position(|l| *l == ui.log_min_level)
                .unwrap_or(0);
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
        Some(Modal::Workflow) => {
            ui.modal = modal;
            workflow::key(ui, key);
        }
        Some(Modal::WorkflowLogin { device, form }) => {
            if workflow::login_key(ui, *device, form, key) {
                ui.modal = modal;
            } else {
                ui.modal = Some(Modal::Workflow);
            }
        }
        Some(Modal::CoreQuestion(question)) => {
            let _ = ui.app.engine().submit(Command::Reply {
                request: question.id,
                input: if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                    "y"
                } else {
                    "n"
                }
                .into(),
            });
        }
        Some(Modal::Jobs { selected }) => {
            let jobs: Vec<_> = ui
                .app
                .engine()
                .snapshot()
                .operations
                .into_iter()
                .filter(|o| o.transfer.is_some())
                .collect();
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return,
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(jobs.len().saturating_sub(1)),
                KeyCode::Char('r' | 'c' | 's' | 'v' | 'S' | 'o') => {
                    if let Some(job) = jobs.get(*selected) {
                        let command = match key.code {
                            KeyCode::Char('r') => Some(Command::RetryOperation(job.id)),
                            KeyCode::Char('c') => Some(Command::CancelOperation(job.id)),
                            KeyCode::Char('v') => Some(Command::InspectOperation(job.id)),
                            KeyCode::Char('s') => {
                                job.device.map(|d| Command::ReviewPending(vec![d]))
                            }
                            KeyCode::Char('S') => {
                                job.device.map(|d| Command::ResumeReviewed(vec![d]))
                            }
                            KeyCode::Char('o') => Some(Command::RequestOverwriteRetry(job.id)),
                            _ => None,
                        };
                        if let Some(command) = command {
                            match ui.app.engine().submit(command) {
                                Err(error) => ui.status_msg = Some(error),
                                Ok(id) if key.code == KeyCode::Char('o') => {
                                    if let Some(question) = ui
                                        .app
                                        .engine()
                                        .pending_confirmations()
                                        .into_iter()
                                        .find(|c| c.id == id)
                                    {
                                        ui.modal = Some(Modal::CoreQuestion(question));
                                        return;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
            ui.modal = modal;
        }
        Some(Modal::TransferProtocol {
            options,
            selected,
            start,
            upgrade,
        }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                ui.modal = modal;
            }
            KeyCode::Down => {
                *selected = (*selected + 1).min(options.len().saturating_sub(1));
                ui.modal = modal;
            }
            KeyCode::Enter => {
                let protocol = options[*selected];
                if *upgrade {
                    ui.upgrade_protocol = protocol;
                } else if let Some(sw) = selected_switch(ui) {
                    let _ = ui
                        .app
                        .engine()
                        .submit(Command::ChooseProtocol(sw.id, protocol));
                }
                if *start {
                    prepare_transfer(ui);
                }
            }
            _ => ui.modal = modal,
        },
        Some(Modal::ConfirmDelete {
            request, switch, ..
        }) => {
            let yes = key.code == KeyCode::Char('y') && key.modifiers.is_empty();
            let answer = if yes { "y" } else { "n" };
            let sw = switch.clone();
            match ui.app.engine().submit(Command::Reply {
                request: *request,
                input: answer.into(),
            }) {
                Ok(_) if yes => {
                    ui.transfer_ui.requested = None;
                    open_session_view(ui, sw);
                }
                Ok(_) => ui.status_msg = Some("deletion cancelled".into()),
                Err(e) => ui.status_msg = Some(e),
            }
        }
        Some(Modal::UpgradeActions { switch, selected }) => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                ui.modal = modal;
            }
            KeyCode::Down => {
                *selected = (*selected + 1).min(UPGRADE_JOB_ACTIONS.len() - 1);
                ui.modal = modal;
            }
            KeyCode::Enter => {
                let sw = switch.clone();
                let action = *selected;
                upgrade_action(ui, sw, action);
            }
            KeyCode::Char('d') => {
                let sw = switch.clone();
                upgrade_action(ui, sw, 0);
            }
            KeyCode::Char('V') => {
                let sw = switch.clone();
                upgrade_action(ui, sw, 1);
            }
            KeyCode::Char('u') => {
                let sw = switch.clone();
                upgrade_action(ui, sw, 2);
            }
            KeyCode::Char('Y') => {
                let sw = switch.clone();
                upgrade_action(ui, sw, 3);
            }
            KeyCode::Char('i') => {
                let sw = switch.clone();
                upgrade_action(ui, sw, 4);
            }
            _ => ui.modal = modal,
        },
        Some(Modal::ConfirmInstall {
            request, switch, ..
        }) => {
            let yes = key.code == KeyCode::Char('y') && key.modifiers.is_empty();
            let sw = switch.clone();
            match ui.app.engine().submit(Command::Reply {
                request: *request,
                input: if yes { "y" } else { "n" }.into(),
            }) {
                Ok(_) if yes => open_session_view(ui, sw),
                Ok(_) => ui.status_msg = Some("upgrade cancelled".into()),
                Err(e) => ui.status_msg = Some(e),
            }
        }
        Some(Modal::Cli) => {
            ui.modal = modal;
            handle_cli_key(ui, key);
        }
        Some(Modal::ConfirmStart { id }) => {
            let id = *id;
            ui.modal = Some(Modal::Deploy);
            if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                start_deploy_service(ui, id);
                ui.pending_deploy = Some((id, Instant::now()));
            }
        }
        Some(Modal::DeployProtocol { selected }) => {
            match key.code {
                KeyCode::Up | KeyCode::Left => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Right => {
                    *selected = (*selected + 1).min(DEPLOY_PROTOCOLS.len() - 1)
                }
                KeyCode::Char('s') => {
                    let id = crate::cisco::service_of(DEPLOY_PROTOCOLS[*selected]);
                    start_deploy_service(ui, id);
                }
                KeyCode::Enter => {
                    if let Some(view) = ui.deploy.as_mut() {
                        view.form.proto = DEPLOY_PROTOCOLS[*selected];
                        if ui.deploy_mode == DeployMode::Copy {
                            view.form.field = 6;
                        } else {
                            view.form.field = deploy_fields(ui.deploy_mode).len() - 1;
                        }
                        view.error = None;
                    }
                    ui.modal = Some(Modal::Deploy);
                    return;
                }
                KeyCode::Esc => {
                    ui.modal = Some(Modal::Deploy);
                    return;
                }
                _ => {}
            }
            ui.modal = modal;
        }
        Some(Modal::UpgradeFiles) => {
            ui.modal = modal;
            handle_upgrade_files_key(ui, key);
        }
        Some(Modal::Help) => match key.code {
            // The two key tables can be taller than the terminal.
            KeyCode::Up | KeyCode::Char('k') => {
                ui.help_scroll = ui.help_scroll.saturating_sub(1);
                ui.modal = modal;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                ui.help_scroll += 1;
                ui.modal = modal;
            }
            KeyCode::PageUp => {
                ui.help_scroll = ui.help_scroll.saturating_sub(10);
                ui.modal = modal;
            }
            KeyCode::PageDown => {
                ui.help_scroll += 10;
                ui.modal = modal;
            }
            // Any other key closes.
            _ => {}
        },
        Some(Modal::Message(_)) => {
            // Any key closes.
        }
        Some(Modal::ConfirmQuit) => match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                let _ = ui.app.engine().submit(Command::StopAll);
                ui.app.switches.disconnect_all();
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
            }
            KeyCode::Backspace => {
                value.pop();
                ui.modal = modal;
            }
            KeyCode::Enter => {
                let (action, value) = (*action, value.clone());
                apply_input(ui, action, value);
            }
            _ => {
                ui.modal = modal;
            }
        },
        Some(Modal::Cisco {
            commands,
            selected,
            copied,
            ..
        }) => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                *copied = false;
                ui.modal = modal;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(commands.len().saturating_sub(1));
                *copied = false;
                ui.modal = modal;
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
            }
            KeyCode::Char('i') => {
                let rel = match &modal {
                    Some(Modal::Cisco { rel_path, .. }) => rel_path.clone(),
                    _ => String::new(),
                };
                cycle_advertise(ui);
                // Rebuild with the new address; the file is unchanged.
                let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
                let keep_cwd = std::mem::replace(
                    &mut ui.files.cwd,
                    rel.rsplit_once('/')
                        .map(|(d, _)| d.to_string())
                        .unwrap_or_default(),
                );
                open_cisco_modal(ui, &name);
                ui.files.cwd = keep_cwd;
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => {
                ui.modal = modal;
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
        }
        Some(Modal::Deploy) => {
            ui.modal = modal;
            handle_deploy_key(ui, key);
        }
        Some(Modal::Session) => {
            ui.modal = modal;
            handle_session_key(ui, key);
        }
        Some(Modal::Hashes) => match key.code {
            KeyCode::Char('c') => {
                ui.modal = Some(Modal::Input {
                    title: "compare hash (paste MD5/SHA-256/SHA-512)".into(),
                    value: String::new(),
                    action: InputAction::HashCompare,
                });
            }
            KeyCode::Esc | KeyCode::Char('q') => {}
            _ => {
                ui.modal = modal;
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
                let _ = ui.app.engine().submit(Command::StopService(id));
                ui.status_msg = Some(format!("{} stopping", id.display_name()));
            } else {
                {
                    let mut cfg = ui.app.config.write().unwrap();
                    cfg.service_mut(id).enabled = true;
                }
                save_config(ui);
                ui.beep(Tone::On);
                let _ = ui.app.engine().submit(Command::StartService(id));
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
                let _ = ui.app.engine().submit(Command::StopService(id));
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
fn commit_service_edit(
    ui: &mut Ui,
    id: ServiceId,
    field: EditField,
    value: String,
) -> Option<String> {
    let cfg = ui.app.config.read().unwrap().clone();
    let setting = match field {
        EditField::Port => {
            let mut service = cfg.service(id).clone();
            service.port = match value.trim().parse::<u16>() {
                Ok(p) if p > 0 => p,
                _ => return Some(format!("invalid port: {value}")),
            };
            Setting::Service(id, service)
        }
        EditField::Bind => {
            let mut service = cfg.service(id).clone();
            service.bind = value.trim().into();
            Setting::Service(id, service)
        }
        EditField::Username => Setting::Credentials(value.trim().into(), cfg.auth.password),
        EditField::Password => Setting::Credentials(cfg.auth.username, value),
        EditField::UploadDir => {
            let mut upload = cfg.uploads;
            upload.dir = value.trim().trim_matches('/').into();
            Setting::Uploads(upload)
        }
        _ => return None,
    };
    match ui.app.engine().submit(Command::Set(setting)) {
        Ok(_) => {
            ui.status_msg = Some("settings updated".into());
            None
        }
        Err(e) => Some(e),
    }
}

fn apply_input(ui: &mut Ui, action: InputAction, value: String) {
    match action {
        InputAction::Root => {
            match ui
                .app
                .engine()
                .submit(Command::ChangeRoot(PathBuf::from(value)))
            {
                Ok(_) => {
                    ui.files.cwd.clear();
                    let root = ui.app.config.read().unwrap().root.clone();
                    ui.files.refresh(&root);
                    ui.status_msg = Some("root changed; services stopped".into());
                }
                Err(e) => ui.status_msg = Some(e),
            }
        }
        InputAction::Port(id) | InputAction::Bind(id) => {
            let field = if matches!(action, InputAction::Port(_)) {
                EditField::Port
            } else {
                EditField::Bind
            };
            if let Some(err) = commit_service_edit(ui, id, field, value) {
                ui.modal = Some(Modal::Message(err));
            }
        }
        InputAction::Username | InputAction::Password | InputAction::UploadDir => {
            let field = match action {
                InputAction::Username => EditField::Username,
                InputAction::Password => EditField::Password,
                _ => EditField::UploadDir,
            };
            if let Some(err) = commit_service_edit(ui, selected_service(ui), field, value) {
                ui.modal = Some(Modal::Message(err));
            }
        }
        InputAction::FileFilter => {
            ui.files.filter = value;
            ui.files.selected = 0;
        }
        InputAction::LogFilter => {
            ui.log_filter = value;
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
    fn frontend_catalog_covers_shared_actions() {
        assert_eq!(SUPPORTED_ACTIONS, transferbuddy_core::engine::ACTIONS);
    }
    #[test]
    fn bulk_import_accepts_mixed_separators_deduplicates_and_validates() {
        assert_eq!(
            bulk_hosts("10.0.0.1 10.0.0.2,10.0.0.3;10.0.0.4\n10.0.0.1\r\n2001:db8::1").unwrap(),
            vec![
                "10.0.0.1",
                "10.0.0.2",
                "10.0.0.3",
                "10.0.0.4",
                "2001:db8::1"
            ]
        );
        assert!(bulk_hosts(" ;,\n").is_err());
        assert!(bulk_hosts("10.0.0.1;invalid;10.0.0.2").is_err());
    }

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
        assert_eq!(
            compare_hash(&i, "zz0150983cd24fb0d6963f7d28e17f72").kind,
            "?"
        );
    }
}
