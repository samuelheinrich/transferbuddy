mod assistance;
mod chrome;
mod console;
mod links;
mod submission;
mod views;
mod wizard;
mod workspace;
use crate::design;
use crate::native::{Activity, NativeAction, NativeShell};
use crate::preferences::Preferences;
pub(crate) use chrome::Intent;
use eframe::egui::{self, Color32, RichText};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;
#[cfg(test)]
use transferbuddy_core::engine::Action;
use transferbuddy_core::{
    cisco,
    config::Config,
    engine::{
        self, AppSnapshot, Command, Confirmation, ConfirmationKind, Credentials, DeviceSnapshot,
        Setting,
    },
    files::FileEntry,
    services::{ServiceId, ServiceStatus},
    session::{fmt_bytes, fmt_duration, fmt_speed, Protocol},
    switch::SwitchState,
    upgrade, App, SharedApp, StartupOptions, VERSION,
};
#[cfg(test)]
pub const SUPPORTED_ACTIONS: &[Action] = &[
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
use crate::design::{AMBER, CYAN, GREEN, RED};
pub use transferbuddy_core::workspace::WorkspaceView as Tab;
pub enum UiEvent {
    Preflight {
        spec: engine::WorkflowSpec,
        revision: u64,
        checks: Vec<engine::Preflight>,
    },
    Root(Option<PathBuf>),
    Boot(Result<SharedApp, String>),
    Native(NativeAction),
    Operation(engine::Operation),
    Submitted(submission::Outcome),
    CopyCommands(Result<Vec<(Protocol, String)>, String>),
    Interfaces(Vec<transferbuddy_core::netif::NetInterface>),
    Status(String),
    Dropped {
        root: PathBuf,
        result: Result<(String, bool), String>,
    },
}
#[derive(Default, Clone)]
struct ConnectionForm {
    targets: String,
    port: String,
    user: String,
    password: String,
    enable: String,
    bulk: bool,
    error: String,
}
#[derive(Clone)]
struct ServiceForm {
    id: ServiceId,
    port: String,
    bind: String,
    user: String,
    password: String,
    uploads: bool,
    overwrite: bool,
    upload_dir: String,
    max_upload: u64,
    enabled: bool,
}
pub struct Desktop {
    pub app: Option<SharedApp>,
    pub snapshot: AppSnapshot,
    pub tab: Tab,
    pub device: Option<u64>,
    runtime: tokio::runtime::Handle,
    config_path: Option<PathBuf>,
    config_dir: PathBuf,
    prefs: Preferences,
    events: mpsc::Receiver<UiEvent>,
    submissions: submission::Worker,
    pending_submissions: usize,
    snapshot_token: String,
    local_cache_key: Option<(String, String, usize, u64)>,
    local_cache: std::sync::Arc<Vec<FileEntry>>,
    log_cache_key: Option<(u64, String, usize, String)>,
    log_cache: std::sync::Arc<Vec<transferbuddy_core::logging::LogEntry>>,
    log_total: usize,
    list_widgets: std::collections::HashSet<egui::Id>,
    pending_service: Option<ServiceForm>,
    pending_transfers: Option<Vec<engine::TransferRequest>>,
    tx: mpsc::Sender<UiEvent>,
    pub status: String,
    native: Option<NativeShell>,
    activity: Activity,
    booting: bool,
    local_dir: String,
    remote_dir: String,
    local_selection: Option<FileEntry>,
    remote_selection: Option<cisco::RemoteFile>,
    local_selected: std::collections::BTreeSet<String>,
    remote_selected: std::collections::BTreeSet<String>,
    local_anchor: Option<String>,
    remote_anchor: Option<String>,
    device_workspaces: std::collections::HashMap<u64, workspace::DeviceWorkspace>,
    jobs_open: bool,
    small_devices_open: bool,
    job_selected: Option<u64>,
    jobs_last_selection: Option<u64>,
    confirmation_text: String,
    style_key: Option<(Option<bool>, bool, u32, bool)>,
    upgrade_selected: std::collections::BTreeSet<u64>,
    upgrade_images_open: bool,
    remote_loaded: Option<(u64, String)>,
    filter: String,
    remote_filter: String,
    sort: usize,
    image: Option<String>,
    upgrade_protocol: Protocol,
    connect: Option<ConnectionForm>,
    service: Option<ServiceForm>,
    console: Option<u64>,
    cli: Option<u64>,
    cli_focus: bool,
    cli_new: bool,
    protocol_picker: Option<(u64, bool, bool)>,
    protocol_index: Option<usize>,
    picker_options: Vec<Protocol>,
    protocol_shown: Option<u64>,
    focus: usize,
    armed_confirmation: Option<u64>,
    hash_view: Option<String>,
    file_details: Option<(String, u64, bool)>,
    commands_view: Option<Vec<(Protocol, String)>>,
    hash_compare: String,
    pub confirmation: Option<Confirmation>,
    handled_confirmation: Option<u64>,
    quit_question: bool,
    quitting: bool,
    log_filter: String,
    log_level: usize,
    log_protocol: String,
    log_follow: bool,
    settings: bool,
    info_open: bool,
    header_tools_bottom: f32,
    interface_anchor: Option<egui::Response>,
    help_open: bool,
    retry_interface: Option<u64>,
    retry_choice: Option<String>,
    retry_saving: Option<u64>,
    interface: String,
    device_filter: String,
    wizard: wizard::Wizard,
    needs_attention: bool,
    connection_selected: std::collections::BTreeSet<u64>,
    credentials_device: Option<u64>,
    console_windows: std::collections::BTreeMap<u64, console::ConsoleWindow>,
    interfaces: Vec<transferbuddy_core::netif::NetInterface>,
    palette_open: bool,
    palette_was_open: bool,
    palette_query: String,
    palette_index: usize,
    log_seen: usize,
    log_detail: Option<String>,
    scroll_device: bool,
    scroll_files: bool,
}
impl Desktop {
    pub fn new(
        ctx: &egui::Context,
        runtime: tokio::runtime::Handle,
        config_path: Option<PathBuf>,
        root: Option<PathBuf>,
        tx: mpsc::Sender<UiEvent>,
        events: mpsc::Receiver<UiEvent>,
    ) -> Self {
        let config_dir = config_path
            .as_ref()
            .and_then(|p| p.parent())
            .map(PathBuf::from)
            .unwrap_or_else(Config::default_dir);
        let prefs = Preferences::load(&config_dir);
        design::install(ctx);
        design::apply(ctx, &prefs);
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::new("console_columns"), prefs.columns.clone())
        });
        let native = if cfg!(test) {
            None
        } else {
            NativeShell::new(ctx.clone(), tx.clone()).ok()
        };
        let start_root = root.or_else(|| prefs.root.clone()).filter(|p| p.is_dir());
        let submissions = submission::Worker::new(ctx.clone(), tx.clone());
        let interface_tx = tx.clone();
        let interface_ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = interface_tx.send(UiEvent::Interfaces(transferbuddy_core::netif::candidates()));
            interface_ctx.request_repaint();
        });
        let mut result = Self {
            app: None,
            snapshot: Default::default(),
            tab: Tab::Dashboard,
            device: None,
            runtime,
            config_path,
            config_dir,
            prefs,
            events,
            submissions,
            pending_submissions: 0,
            snapshot_token: String::new(),
            local_cache_key: None,
            local_cache: Default::default(),
            log_cache_key: None,
            log_cache: Default::default(),
            log_total: 0,
            list_widgets: Default::default(),
            pending_service: None,
            pending_transfers: None,
            tx,
            status: "Choose the local folder to share. Services start only when you start them."
                .into(),
            native,
            activity: Activity::default(),
            booting: false,
            local_dir: String::new(),
            remote_dir: String::new(),
            local_selection: None,
            remote_selection: None,
            local_selected: Default::default(),
            remote_selected: Default::default(),
            local_anchor: None,
            remote_anchor: None,
            device_workspaces: Default::default(),
            jobs_open: false,
            small_devices_open: false,
            job_selected: None,
            jobs_last_selection: None,
            confirmation_text: String::new(),
            style_key: None,
            upgrade_selected: Default::default(),
            upgrade_images_open: true,
            remote_loaded: None,
            filter: String::new(),
            remote_filter: String::new(),
            sort: 0,
            image: None,
            upgrade_protocol: Protocol::Http,
            connect: None,
            service: None,
            console: None,
            cli: None,
            cli_focus: true,
            cli_new: false,
            protocol_picker: None,
            protocol_index: None,
            picker_options: Vec::new(),
            protocol_shown: None,
            focus: 1,
            armed_confirmation: None,
            hash_view: None,
            file_details: None,
            commands_view: None,
            hash_compare: String::new(),
            confirmation: None,
            handled_confirmation: None,
            quit_question: false,
            quitting: false,
            log_filter: String::new(),
            log_level: 0,
            log_protocol: String::new(),
            log_follow: true,
            settings: false,
            info_open: false,
            header_tools_bottom: 90.0,
            interface_anchor: None,
            help_open: false,
            retry_interface: None,
            retry_choice: None,
            retry_saving: None,
            interface: String::new(),
            device_filter: String::new(),
            wizard: wizard::Wizard::default(),
            needs_attention: false,
            connection_selected: Default::default(),
            credentials_device: None,
            console_windows: Default::default(),
            interfaces: Vec::new(),
            palette_open: false,
            palette_was_open: false,
            palette_query: String::new(),
            palette_index: 0,
            log_seen: 0,
            log_detail: None,
            scroll_device: false,
            scroll_files: false,
        };
        if let Some(root) = start_root {
            result.boot(root, ctx.clone())
        }
        result
    }
    fn boot(&mut self, root: PathBuf, ctx: egui::Context) {
        self.booting = true;
        let config_path = self.config_path.clone();
        let tx = self.tx.clone();
        let handle = self.runtime.clone();
        self.runtime.spawn(async move {
            let result = tokio::task::spawn_blocking(move || {
                let mut cfg = Config::load(config_path.as_deref()).map_err(|e| e.to_string())?;
                for id in ServiceId::ALL {
                    if cfg.service(id).port == 0 {
                        cfg.service_mut(id).port = id.default_port(true);
                    }
                }
                cfg.apply_cli(
                    &StartupOptions {
                        root: Some(root),
                        ..Default::default()
                    },
                    transferbuddy_core::platform::is_privileged(),
                )
                .map_err(|e| e.to_string())?;
                cfg.validate(transferbuddy_core::platform::is_privileged())
                    .map_err(|e| e.to_string())?;
                Ok(App::new(cfg, handle, false))
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            let _ = tx.send(UiEvent::Boot(result));
            ctx.request_repaint();
        });
    }
    fn pick_root(&self, ctx: &egui::Context) {
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let folder = rfd::AsyncFileDialog::new()
                .set_title("Choose TransferBuddy local folder")
                .pick_folder()
                .await;
            let _ = tx.send(UiEvent::Root(folder.map(|f| f.path().to_path_buf())));
            ctx.request_repaint();
        });
    }
    fn refresh_local(&mut self) {
        self.send(Command::ListLocal(self.local_dir.clone()));
    }
    fn local_path(&self, name: &str) -> String {
        if self.local_dir.is_empty() {
            name.into()
        } else {
            format!("{}/{name}", self.local_dir)
        }
    }
    fn remote_path(&self, name: &str) -> String {
        let storage = self
            .snapshot
            .devices
            .iter()
            .find(|d| Some(d.id) == self.device)
            .map(|d| d.facts.flash_device.as_str())
            .unwrap_or("flash:");
        format!(
            "{storage}{}{}{name}",
            self.remote_dir,
            if self.remote_dir.is_empty() || name.is_empty() {
                ""
            } else {
                "/"
            }
        )
    }
    fn selected(&self) -> Option<DeviceSnapshot> {
        self.snapshot
            .devices
            .iter()
            .find(|d| Some(d.id) == self.device)
            .cloned()
    }
    fn choose_device(&mut self, id: u64) {
        self.focus = 0;
        self.protocol_index = None;
        if self.device != Some(id) {
            if let Some(old) = self.device {
                self.device_workspaces.insert(old, self.capture_workspace());
            }
            self.device = Some(id);
            self.restore_workspace(id);
            self.remote_loaded = None;
        }
    }
    pub fn request_install(&mut self, id: u64, yolo: bool) {
        self.send(Command::RequestInstall { device: id, yolo });
        self.sync_confirmation();
    }
    fn sync_confirmation(&mut self) {
        if let Some(app) = &self.app {
            let pending = app.engine().pending_confirmations();
            if self
                .confirmation
                .as_ref()
                .is_some_and(|c| !pending.iter().any(|p| p.id == c.id))
            {
                self.confirmation = None;
            }
            if self.confirmation.is_none() {
                self.confirmation = pending
                    .into_iter()
                    .find(|c| Some(c.id) != self.handled_confirmation);
            }
        }
    }
    pub fn answer(&mut self, input: &str) {
        if let Some(c) = self.confirmation.take() {
            self.handled_confirmation = Some(c.id);
            self.send(Command::Reply {
                request: c.id,
                input: input.into(),
            });
        }
    }
    fn transfer(&mut self, receive: bool) {
        if self.pending_transfers.is_none() {
            self.pending_transfers = Some(self.transfer_requests(receive));
        }
        let Some(d) = self.selected() else {
            self.status = "Connect a device first".into();
            return;
        };
        if !self
            .app
            .as_ref()
            .is_some_and(|a| a.engine().device(d.id).is_ok_and(|sw| sw.protocol_chosen()))
            || (receive && d.protocol != Protocol::Ftp)
        {
            self.protocol_picker = Some((d.id, receive, true));
            return;
        }
        self.perform_transfer(receive);
    }
    fn perform_transfer(&mut self, receive: bool) {
        let mut requests = self
            .pending_transfers
            .take()
            .unwrap_or_else(|| self.transfer_requests(receive));
        if requests.is_empty() {
            self.status = "Select a file".into();
            return;
        }
        for request in &mut requests {
            if receive {
                request.protocol = Protocol::Ftp;
            }
        }
        if self.send(Command::QueueTransfers(requests)) {
            self.jobs_open = true;
        }
    }
    fn transfer_requests(&self, receive: bool) -> Vec<engine::TransferRequest> {
        let Some(d) = self.selected() else {
            return Vec::new();
        };
        self.selected_file_names(receive)
            .into_iter()
            .map(|name| engine::TransferRequest {
                device: d.id,
                local: self.local_path(&name),
                remote: self.remote_path(&name),
                receive,
                protocol: if receive { Protocol::Ftp } else { d.protocol },
                overwrite: false,
                platform_check: false,
            })
            .collect()
    }
    fn enter_local(&mut self) {
        if let Some(f) = self.local_selection.clone() {
            if f.is_dir {
                self.local_dir = if f.name == ".." {
                    parent(&self.local_dir)
                } else {
                    self.local_path(&f.name)
                };
                self.local_selection = None;
                self.local_selected.clear();
                self.refresh_local();
            } else {
                self.file_details = Some((self.local_path(&f.name), f.size, false));
            }
        }
    }
    fn enter_remote(&mut self) {
        if let Some(f) = self.remote_selection.clone() {
            if f.is_dir {
                self.remote_dir = if self.remote_dir.is_empty() {
                    f.name
                } else {
                    format!("{}/{}", self.remote_dir, f.name)
                };
                self.remote_selection = None;
                self.remote_selected.clear();
                self.remote_loaded = None;
            } else {
                self.file_details = Some((self.remote_path(&f.name), f.size, true));
            }
        }
    }
    fn delete_selected(&mut self) {
        if let (Some(d), Some(f)) = (
            self.selected(),
            self.remote_selection
                .clone()
                .filter(|f| self.remote_selected.contains(&f.name)),
        ) {
            self.send(Command::RequestDelete {
                device: d.id,
                remote: self.remote_path(&f.name),
                recursive: f.is_dir,
            });
            self.sync_confirmation();
        }
    }
    fn dialogs(&mut self, ctx: &egui::Context) {
        if self.confirmation.is_none()
            && self.protocol_picker.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.connect = None;
            self.credentials_device = None;
            self.service = None;
            self.settings = false;
            self.hash_view = None;
            self.file_details = None;
            self.commands_view = None;
            self.quit_question = false;
        }
        if let Some((path, size, remote)) = self.file_details.clone() {
            let mut open = true;
            egui::Window::new("File details")
                .id(egui::Id::new("file_details"))
                .open(&mut open)
                .default_width(480.0)
                .show(ctx, |ui| {
                    ui.label(if remote { "Remote file" } else { "Local file" });
                    ui.label(&path);
                    ui.label(fmt_bytes(size));
                    if !remote && ui.button("Calculate hashes").clicked() {
                        self.file_details = None;
                        self.hash_view = Some(path.clone());
                        self.send(Command::Hash(path.clone()));
                    }
                    if ui.button("Close").clicked() {
                        self.file_details = None;
                    }
                });
            if !open || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.file_details = None;
            }
        }
        if let Some(commands) = self.commands_view.clone() {
            let mut open = true;
            egui::Window::new("Cisco copy commands")
                .open(&mut open)
                .show(ctx, |ui| {
                    if commands.is_empty() {
                        ui.label("Enable or start a service in Dashboard first.");
                    }
                    for (protocol, command) in commands {
                        ui.horizontal(|ui| {
                            ui.label(protocol.label());
                            if ui.button("Copy").clicked() {
                                ui.ctx().copy_text(command.clone());
                            }
                        });
                        design::copy_command(ui, &command);
                        ui.separator();
                    }
                });
            if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.commands_view = None;
            }
        }
        if let Some(mut form) = self.connect.take() {
            let mut open = true;
            let mut submit = false;
            egui::Window::new(if self.credentials_device.is_some() {"Edit login credentials"} else if form.bulk {"Bulk connect"} else {"Connect device"})
                .collapsible(false).resizable(false).default_width(480.0).anchor(egui::Align2::CENTER_CENTER,[0.0,0.0]).open(&mut open).show(ctx,|ui| {
                    design::heading(ui,"SSH CONNECTION",if form.bulk {"IPs and subnets, comma separated"} else {"Persistent session · credentials kept in memory"});
                    ui.add_space(12.0);
                    ui.label(if form.bulk {"Device IP / Subnet"} else {"Device IP / hostname"});
                    ui.add_enabled(self.credentials_device.is_none(),egui::TextEdit::singleline(&mut form.targets).desired_width(f32::INFINITY));
                    ui.add_space(12.0);
                    egui::Grid::new("credentials").num_columns(2).spacing([24.0,12.0]).show(ui,|ui| {
                        ui.label("SSH port");ui.add_enabled(self.credentials_device.is_none(),egui::TextEdit::singleline(&mut form.port).desired_width(80.0));ui.end_row();
                        ui.label("Username");ui.text_edit_singleline(&mut form.user);ui.end_row();
                        ui.label("Password");ui.add(egui::TextEdit::singleline(&mut form.password).password(true));ui.end_row();
                        ui.label("Enable password");ui.add(egui::TextEdit::singleline(&mut form.enable).password(true).hint_text("Optional"));ui.end_row();
                    });
                    ui.add_space(12.0);
                    if form.bulk {ui.label(RichText::new("Failed discovered connections appear in Logs. Host-key acceptance follows Settings.").small().color(design::accent(ui)));}
                    if !form.error.is_empty() { ui.colored_label(design::semantic(ui, RED), &form.error); }
                    ui.separator();
                    submit=design::primary(ui,"Connect").clicked();
                });
            if submit {
                let port = form.port.parse().unwrap_or(0);
                let credentials = Credentials {
                    username: form.user.clone(),
                    password: form.password.clone(),
                    enable_password: form.enable.clone(),
                };
                let command = if let Some(device) = self.credentials_device {
                    Command::UpdateCredentials(device, credentials)
                } else if form.bulk {
                    Command::BulkConnect {
                        targets: form.targets.clone(),
                        port,
                        credentials,
                    }
                } else {
                    Command::Connect {
                        host: form.targets.clone(),
                        port,
                        credentials,
                        bulk: false,
                    }
                };
                if self.send(command) {
                    open = false;
                }
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                open = false;
            }
            if open {
                self.connect = Some(form);
            } else {
                self.credentials_device = None;
            }
        }
        if let Some(mut form) = self.service.take() {
            let mut open = true;
            let mut save = false;
            egui::Window::new(format!("Configure {}", form.id.display_name()))
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.checkbox(&mut form.enabled, "Enabled");
                    ui.horizontal(|ui| {
                        ui.label("Port");
                        ui.text_edit_singleline(&mut form.port);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Bind address");
                        ui.text_edit_singleline(&mut form.bind);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Server username");
                        ui.text_edit_singleline(&mut form.user);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Server password");
                        ui.add(egui::TextEdit::singleline(&mut form.password).password(true));
                    });
                    ui.checkbox(&mut form.uploads, "Allow general uploads");
                    ui.checkbox(&mut form.overwrite, "Allow overwrite for general uploads");
                    ui.label("Upload directory, relative to root");
                    ui.text_edit_singleline(&mut form.upload_dir);
                    ui.horizontal(|ui| {
                        ui.label("Maximum upload MiB (0 unlimited)");
                        ui.add(egui::DragValue::new(&mut form.max_upload));
                    });
                    save = ui.button("Save settings").clicked();
                });
            if save {
                self.pending_service = Some(form.clone());
                let config = transferbuddy_core::config::ServiceConfig {
                    enabled: form.enabled,
                    port: form.port.parse().unwrap_or(0),
                    bind: form.bind.clone(),
                };
                if self.send(Command::SetMany(vec![
                    Setting::Service(form.id, config),
                    Setting::Credentials(form.user.clone(), form.password.clone()),
                    Setting::Uploads(transferbuddy_core::config::UploadConfig {
                        enabled: form.uploads,
                        overwrite: form.overwrite,
                        dir: form.upload_dir.clone(),
                        max_upload_mib: form.max_upload,
                    }),
                ])) {
                    open = false;
                }
            }
            if open {
                self.service = Some(form);
            }
        }
        if let Some((id, receive, start)) = self.protocol_picker {
            let armed = self.protocol_shown == Some(id);
            self.protocol_shown = Some(id);
            let mut open = true;
            let mut choice = None;
            let mut cancel = false;
            if !armed {
                self.picker_options = self
                    .app
                    .as_ref()
                    .map(|a| engine::protocol_options(a, receive))
                    .unwrap_or_default();
            }
            let options = self.picker_options.clone();
            let chosen = self
                .app
                .as_ref()
                .and_then(|a| a.engine().device(id).ok())
                .filter(|s| s.protocol_chosen())
                .map(|s| s.protocol());
            let mut selected = self
                .protocol_index
                .unwrap_or_else(|| options.iter().position(|p| Some(*p) == chosen).unwrap_or(0))
                .min(options.len().saturating_sub(1));
            egui::Window::new("Transfer protocol")
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label("Active services first. ↑/↓ chooses; Enter accepts.");
                    if armed && ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                        selected = selected.saturating_sub(1)
                    }
                    if armed && ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                        selected = (selected + 1).min(options.len().saturating_sub(1))
                    }
                    for (i, p) in options.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.selectable_label(i == selected, p.label()).clicked() {
                                choice = Some(*p);
                            }
                            if let Some(app) = &self.app {
                                let id = cisco::service_of(*p);
                                let status = app.services.status(id);
                                design::badge(
                                    ui,
                                    if status.is_running() {
                                        "LIVE"
                                    } else {
                                        "START ON TRANSFER"
                                    },
                                    if status.is_running() { GREEN } else { CYAN },
                                );
                            }
                        });
                    }
                    if armed && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        choice = options.get(selected).copied();
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                        cancel = true;
                    }
                });
            self.protocol_index = Some(selected);
            if let Some(p) = choice {
                if self.send(Command::ChooseProtocol(id, p)) {
                    self.protocol_picker = None;
                    self.protocol_index = None;
                    self.protocol_shown = None;
                    if start {
                        if let Some(requests) = &mut self.pending_transfers {
                            for request in requests {
                                if request.device == id {
                                    request.protocol = p;
                                }
                            }
                        }
                        self.perform_transfer(receive);
                    }
                }
            } else if !open || cancel {
                self.pending_transfers = None;
                self.protocol_picker = None;
                self.protocol_index = None;
                self.protocol_shown = None;
            }
        }
        if let Some(path) = self.hash_view.clone() {
            let mut open = true;
            egui::Window::new("File hashes")
                .default_width(600.0)
                .max_width(700.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(&path);
                    match self.snapshot.hashes.get(&path) {
                        Some(Ok(h)) => {
                            for (label, digest) in [
                                ("MD5", &h.md5),
                                ("SHA-256", &h.sha256),
                                ("SHA-512", &h.sha512),
                            ] {
                                ui.horizontal(|ui| {
                                    ui.label(label);
                                    ui.add(
                                        egui::Label::new(RichText::new(digest).monospace()).wrap(),
                                    );
                                    if ui.button("Copy").clicked() {
                                        ui.ctx().copy_text(digest.clone());
                                    }
                                });
                            }
                            ui.label("Compare digest");
                            ui.text_edit_singleline(&mut self.hash_compare);
                            if !self.hash_compare.is_empty() {
                                let matches = [&h.md5, &h.sha256, &h.sha512]
                                    .iter()
                                    .any(|s| s.eq_ignore_ascii_case(self.hash_compare.trim()));
                                ui.colored_label(
                                    if matches {
                                        design::semantic(ui, GREEN)
                                    } else {
                                        design::semantic(ui, RED)
                                    },
                                    if matches { "MATCH" } else { "MISMATCH" },
                                );
                            }
                        }
                        Some(Err(e)) => {
                            ui.colored_label(design::semantic(ui, RED), e);
                        }
                        None => {
                            ui.spinner();
                        }
                    }
                });
            if !open {
                self.hash_view = None;
            }
        }
        if self.settings {
            let mut open = true;
            egui::Window::new("Settings")
                .collapsible(false)
                .vscroll(true)
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, self.header_tools_bottom + 8.0))
                .default_width(740.0)
                .default_height(520.0)
                .max_width(ctx.content_rect().width() - 40.0)
                .max_height((ctx.content_rect().height() - self.header_tools_bottom - 40.0).max(120.0))
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.heading(format!("TransferBuddy {VERSION}"));
                    ui.horizontal(|ui| {
                        if ui.button("System theme").clicked() {
                            self.prefs.dark = None;
                            ctx.set_theme(egui::ThemePreference::System);
                        }
                        if ui.button("Dark").clicked() {
                            self.prefs.dark = Some(true);
                            ctx.set_theme(egui::Theme::Dark);
                        }
                        if ui.button("Light").clicked() {
                            self.prefs.dark = Some(false);
                            ctx.set_theme(egui::Theme::Light);
                        }
                    });
                    ui.separator();
                    ui.label("CONSOLE APPEARANCE");
                    ui.checkbox(&mut self.prefs.compact, "Compact density");
                    ui.checkbox(&mut self.prefs.reduce_motion, "Reduce motion");
                    ui.add(
                        egui::Slider::new(&mut self.prefs.scale, 0.85..=2.0)
                            .step_by(0.05)
                            .fixed_decimals(2)
                            .text("Interface scale"),
                    );
                    ui.separator();
                    #[cfg(target_os = "macos")]
                    {
                        ui.label("NETWORK ACCESS");
                        ui.label("macOS controls local network access separately for Terminal and TransferBuddy. Allow TransferBuddy, then reconnect your device.");
                        if ui.button("Open Local Network settings").clicked() {
                            if let Err(e) = crate::native::open_network_privacy_settings() { self.status = e; }
                        }
                        ui.separator();
                    }
                    if let Some(app) = &self.app {
                        let cfg = app.config.read().unwrap().clone();
                        let mut bits = cfg.speed_in_bits;
                        let mut sound = cfg.sound;
                        let mut intro = cfg.intro;
                        let mut auto_keys = cfg.auto_accept_host_keys;
                        ui.separator();
                        ui.strong("SSH CONNECTIONS");
                        if ui.checkbox(&mut auto_keys, "Automatically accept device host keys").changed() {
                            self.send(Command::Set(Setting::AutoAcceptHostKeys(auto_keys)));
                        }
                        ui.label("Applies to new SSH sessions and reconnects, including bulk connections.");
                        ui.separator();
                        if ui.checkbox(&mut bits, "Display speed in bits").changed() {
                            self.send(Command::Set(Setting::SpeedInBits(bits)));
                        }
                        if ui.checkbox(&mut sound, "Sounds").changed() {
                            self.send(Command::Set(Setting::Sound(sound)));
                        }
                        if ui
                            .checkbox(&mut intro, "Terminal intro animation")
                            .changed()
                        {
                            self.send(Command::Set(Setting::Intro(intro)));
                        }
                        self.address_selection(ui);
                        ui.label(format!(
                            "TLS certificates: {}",
                            cfg.certificates_dir().display()
                        ));
                        ui.label(format!(
                            "SSH host key: {}",
                            cfg.config_dir.join("state/ssh_host_ed25519_key").display()
                        ));
                    }
                    if ui.button("Enable standard-port helper…").clicked() {
                        match crate::native::enable_helper() {
                            Ok(s) => self.status = s,
                            Err(e) => self.status = e,
                        }
                    }
                    if ui.button("Remove standard-port helper").clicked() {
                        match crate::native::disable_helper() {
                            Ok(()) => self.status = "Helper unregistered".into(),
                            Err(e) => self.status = e,
                        }
                    }
                });
            self.settings = open && !ctx.input(|i| i.key_pressed(egui::Key::Escape));
        }
        if self.quit_question {
            egui::Window::new("Active jobs — quit TransferBuddy?").collapsible(false).show(ctx,|ui|{ui.colored_label(AMBER,"Quitting closes services and SSH sessions. A device upgrade already started cannot be undone.");if ui.button("Keep running").clicked(){self.quit_question=false;}
if ui.button("Quit and close sessions").clicked(){self.quit(ctx);}});
        }
        if let Some(c) = self.confirmation.clone() {
            let opened = self.armed_confirmation != Some(c.id);
            if opened {
                self.confirmation_text.clear();
                self.armed_confirmation = Some(c.id);
            }
            let confirm_input = egui::Id::new(("confirm_input", c.id));
            let pasted = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))));
            if pasted {
                self.confirmation_text.clear();
            }
            ctx.input_mut(|i| {
                i.events.retain(|e| {
                    if matches!(e, egui::Event::Paste(_)) {
                        return false;
                    }
                    if opened
                        && matches!(
                            e,
                            egui::Event::Text(_)
                                | egui::Event::Key {
                                    key: egui::Key::Enter,
                                    ..
                                }
                        )
                    {
                        return false;
                    }
                    true
                });
            });
            let title = match c.kind {
                ConfirmationKind::OverwriteRetry { .. } => "CONFIRM OVERWRITE AND RETRY",
                ConfirmationKind::Delete { .. } => "CONFIRM REMOTE DELETE",
                ConfirmationKind::Install { .. } => "CONFIRM UPGRADE",
                ConfirmationKind::HostKey { .. } => "SSH HOST KEY",
                ConfirmationKind::Reload { .. } => "CONFIRM RELOAD",
                ConfirmationKind::Cleanup { .. } => "CONFIRM REMOVE INACTIVE",
            };
            let mut confirm = false;
            let mut cancel = false;
            let modal = egui::Modal::new(egui::Id::new(("confirmation", c.id))).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 64.0).clamp(240.0, 560.0));
                ui.heading(title);
                let device = self
                    .snapshot
                    .devices
                    .iter()
                    .find(|d| d.id == c.device)
                    .map(|d| format!("{} / {}", d.name, d.host))
                    .unwrap_or_else(|| c.device.to_string());
                design::heading(ui, "TARGET", &device);
                ui.add_space(16.0);
                match &c.kind {
                    ConfirmationKind::OverwriteRetry { remote, .. } => {
                        ui.label(format!(
                            "Replace the destination {remote} and transfer again?"
                        ));
                        ui.colored_label(
                            design::semantic(ui, RED),
                            "The existing destination will be overwritten. Type y to confirm.",
                        );
                    }
                    ConfirmationKind::Delete { remote, recursive } => {
                        design::badge(ui, "PERMANENT DELETE", RED);
                        ui.add_space(8.0);
                        ui.label(remote);
                        if *recursive {
                            ui.label("Includes ALL files and subdirectories.");
                        }
                        ui.colored_label(
                            design::semantic(ui, RED),
                            "Deleted contents cannot be recovered.",
                        );
                    }
                    ConfirmationKind::Install {
                        remote,
                        yolo,
                        md5,
                        version,
                        ..
                    } => {
                        design::badge(
                            ui,
                            if *yolo {
                                "AUTOMATIC RELOAD"
                            } else {
                                "INSTALL UPGRADE"
                            },
                            if *yolo { RED } else { AMBER },
                        );
                        ui.add_space(8.0);
                        ui.label(remote);
                        ui.label(format!("Target IOS {}", version.as_deref().unwrap_or("—")));
                        ui.label(format!("MD5 {md5}"));
                        ui.label("Local/device MD5 matched; version differs from running IOS.");
                        ui.separator();
                        ui.label("write memory → install add file … activate commit");
                        ui.label(if *yolo {
                            "The device will reload automatically after this confirmation."
                        } else {
                            "You will also confirm the device reload prompt."
                        });
                    }
                    ConfirmationKind::HostKey { fingerprint } => {
                        design::badge(ui, "UNKNOWN HOST KEY", AMBER);
                        ui.add_space(8.0);
                        ui.monospace(fingerprint);
                    }
                    ConfirmationKind::Reload { prompt } => {
                        design::badge(ui, "DEVICE RELOAD", AMBER);
                        ui.add_space(8.0);
                        ui.label(prompt);
                    }
                    ConfirmationKind::Cleanup { files, warning } => {
                        design::badge(ui, "REMOVE INACTIVE", AMBER);
                        ui.add_space(8.0);
                        egui::ScrollArea::vertical()
                            .max_height(240.0)
                            .show(ui, |ui| {
                                for file in files {
                                    ui.monospace(file);
                                }
                            });
                        if let Some(w) = warning {
                            ui.colored_label(design::semantic(ui, RED), w);
                        }
                    }
                }
                ui.add_space(16.0);
                ui.separator();
                ui.label("Type lowercase y, then choose Confirm. Esc cancels.");
                let input = ui.add(
                    egui::TextEdit::singleline(&mut self.confirmation_text)
                        .id(confirm_input)
                        .desired_width(90.0)
                        .hint_text("y"),
                );
                if opened {
                    input.request_focus();
                }
                if pasted {
                    ui.colored_label(
                        design::semantic(ui, RED),
                        "Paste cannot approve this action. Type y manually.",
                    );
                }
                ui.horizontal(|ui| {
                    cancel = ui.button("Cancel").clicked();
                    ui.add_enabled_ui(self.confirmation_text == "y", |ui| {
                        confirm = design::primary(ui, "Confirm").clicked();
                    });
                });
            });
            if cancel || modal.should_close() {
                self.answer("n");
            } else if confirm && !opened && !pasted {
                self.answer("y");
            }
        }
    }
    fn open_cli(&mut self, id: u64) {
        if self.console_windows.get(&id).is_some_and(|w| w.cli) {
            self.restore_console(id);
            return;
        }
        if self.send(Command::OpenCli(id)) {
            self.console_windows
                .insert(id, console::ConsoleWindow::new(true));
            self.restore_console(id);
            self.cli_new = true;
        }
    }
    fn quit(&mut self, ctx: &egui::Context) {
        self.submissions.stop();
        if let Some(app) = &self.app {
            app.shutdown();
        }
        self.quitting = true;
        self.quit_question = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
    fn request_quit(&mut self, ctx: &egui::Context) {
        if self.snapshot.active_jobs() || self.pending_submissions > 0 {
            self.quit_question = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        } else {
            self.quit(ctx);
        }
    }
}
impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                UiEvent::Root(Some(root)) => {
                    if self.app.is_none() {
                        self.boot(root, ctx.clone());
                    } else {
                        self.send(Command::ChangeRoot(root));
                    }
                }
                UiEvent::Dropped { root, result } => {
                    if root != self.snapshot.root {
                        self.status = "Local folder changed; drop the file again".into();
                    } else {
                        match result {
                            Ok((path, file)) => {
                                self.local_selected.clear();
                                self.local_selection = None;
                                self.local_dir = if file {
                                    self.image = Some(path.clone());
                                    parent(&path)
                                } else {
                                    path
                                };
                                self.refresh_local();
                            }
                            Err(error) => self.status = error,
                        }
                    }
                }
                UiEvent::Root(None) => {}
                UiEvent::Submitted(outcome) => self.submitted(outcome),
                UiEvent::CopyCommands(result) => match result {
                    Ok(commands) => self.commands_view = Some(commands),
                    Err(error) => self.status = error,
                },
                UiEvent::Boot(result) => {
                    self.booting = false;
                    match result {
                        Ok(app) => {
                            self.prefs.root = Some(app.config.read().unwrap().root.clone());
                            let mut updates = app.engine().subscribe();
                            let wake = ctx.clone();
                            let tx = self.tx.clone();
                            self.runtime.spawn(async move {
                                loop {
                                    match updates.recv().await {
                                        Ok(engine::AppEvent::Finished(operation)) => {
                                            let _ = tx.send(UiEvent::Operation(operation));
                                            wake.request_repaint();
                                        }
                                        Ok(_)
                                        | Err(tokio::sync::broadcast::error::RecvError::Lagged(
                                            _,
                                        )) => wake.request_repaint(),
                                        Err(_) => break,
                                    }
                                }
                            });
                            self.app = Some(app);
                            self.refresh_local();
                            self.status = "Ready — services stopped".into();
                        }
                        Err(e) => self.status = e,
                    }
                }
                UiEvent::Operation(operation) => {
                    if let engine::OperationState::Failed(error)
                    | engine::OperationState::Uncertain(error) = operation.state
                    {
                        if operation.transfer.is_some() {
                            self.job_selected = Some(operation.id);
                            self.jobs_open = true;
                        }
                        self.status = format!("{} failed: {error}", operation.label);
                    }
                }
                UiEvent::Preflight {
                    spec,
                    revision,
                    checks,
                } => {
                    self.wizard.checking = false;
                    self.wizard.check_spec = Some(spec);
                    self.wizard.check_revision = revision;
                    self.wizard.checks = checks;
                }
                UiEvent::Interfaces(interfaces) => self.interfaces = interfaces,
                UiEvent::Status(status) => self.status = status,
                UiEvent::Native(action) => match action {
                    NativeAction::Show => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                    }
                    NativeAction::Quit => self.request_quit(ctx),
                    NativeAction::Folder => self.pick_root(ctx),
                    NativeAction::Settings => self.settings = true,
                    NativeAction::StopAll => {
                        self.send(Command::StopAll);
                    }
                    NativeAction::About => self.info_open = true,
                    NativeAction::Help => self.help_open = true,
                    NativeAction::Copy => ctx.input_mut(|i| i.events.push(egui::Event::Copy)),
                    NativeAction::Cut => ctx.input_mut(|i| i.events.push(egui::Event::Cut)),
                    NativeAction::Paste => {
                        ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste)
                    }
                    NativeAction::SelectAll => ctx.input_mut(|i| {
                        i.events.push(egui::Event::Key {
                            key: egui::Key::A,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::COMMAND,
                        })
                    }),
                    NativeAction::Command(intent) => {
                        if !self.obstructed() && !egui::Popup::is_any_open(ctx) {
                            self.intent(intent, ctx);
                        }
                    }
                },
            }
        }
        if let Some(app) = &self.app {
            let token = app.engine().change_token();
            if self.snapshot_token != token {
                self.snapshot = app.engine().snapshot();
                if self.snapshot.network.revision > 0 {
                    self.interfaces = self
                        .snapshot
                        .network
                        .interfaces
                        .iter()
                        .filter(|i| i.ip.is_ipv4() && !i.ip.is_loopback())
                        .cloned()
                        .collect();
                }
                self.snapshot_token = token;
                self.log_cache_key = None;
            }
        }
        if self
            .device
            .is_some_and(|id| !self.snapshot.devices.iter().any(|d| d.id == id))
        {
            self.device = None;
            self.remote_loaded = None;
        }
        if self.device.is_none() {
            self.device = self.snapshot.devices.first().map(|d| d.id);
        }
        self.sync_confirmation();
        self.activity.set_active(self.snapshot.running_jobs());
        if let Some(mut native) = self.native.take() {
            native.update_commands(|intent| {
                !self.obstructed()
                    && !egui::Popup::is_any_open(ctx)
                    && self.intent_blocker(intent).is_none()
            });
            native.update(
                self.snapshot.confirmations.len(),
                self.snapshot.active_jobs(),
            );
            self.native = Some(native);
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if self.native.is_some() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            } else {
                self.request_quit(ctx);
            }
        }
        if self.snapshot.running_jobs() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
    fn on_exit(&mut self) {
        self.submissions.stop();
        let _ = self.prefs.save(&self.config_dir);
        if let Some(app) = &self.app {
            app.shutdown();
        }
    }
}
fn parent(path: &str) -> String {
    path.rsplit_once('/')
        .map(|(p, _)| p.into())
        .unwrap_or_default()
}
fn mode(d: &DeviceSnapshot) -> String {
    d.facts
        .version
        .as_ref()
        .map(|v| {
            if v.members
                .iter()
                .any(|m| m.mode.eq_ignore_ascii_case("BUNDLE"))
            {
                "BUNDLE"
            } else if v
                .image
                .as_ref()
                .is_some_and(|i| i.contains("packages.conf"))
            {
                "INSTALL"
            } else {
                "?"
            }
        })
        .unwrap_or("?")
        .into()
}
fn reboot_color(p: &upgrade::Progress) -> Color32 {
    match p {
        upgrade::Progress::Rebooting { since, .. } => {
            let t = since.elapsed().as_secs();
            if t < 300 {
                GREEN
            } else if t < 600 {
                AMBER
            } else {
                RED
            }
        }
        upgrade::Progress::Failed(_) => RED,
        upgrade::Progress::Complete { .. } | upgrade::Progress::Verified { .. } => GREEN,
        upgrade::Progress::Verifying
        | upgrade::Progress::Installing
        | upgrade::Progress::AwaitingReload { .. } => AMBER,
        _ => CYAN,
    }
}
fn cli_bytes(event: &egui::Event) -> Option<Vec<u8>> {
    match event {
        egui::Event::Text(s) | egui::Event::Paste(s) => Some(s.replace('\n', "\r").into_bytes()),
        egui::Event::Key {
            key,
            pressed: true,
            modifiers,
            ..
        } => {
            if modifiers.ctrl {
                let c = match key {
                    egui::Key::C => 3,
                    egui::Key::A => 1,
                    egui::Key::E => 5,
                    egui::Key::D => 4,
                    egui::Key::U => 21,
                    egui::Key::K => 11,
                    _ => return None,
                };
                return Some(vec![c]);
            }
            Some(match key {
                egui::Key::Enter => vec![13],
                egui::Key::Tab => vec![9],
                egui::Key::Backspace => vec![8],
                egui::Key::ArrowUp => b"\x1b[A".to_vec(),
                egui::Key::ArrowDown => b"\x1b[B".to_vec(),
                egui::Key::ArrowLeft => b"\x1b[D".to_vec(),
                egui::Key::ArrowRight => b"\x1b[C".to_vec(),
                egui::Key::Delete => b"\x1b[3~".to_vec(),
                egui::Key::Home => b"\x1b[H".to_vec(),
                egui::Key::End => b"\x1b[F".to_vec(),
                _ => return None,
            })
        }
        _ => None,
    }
}
fn terminal_color(color: vt100_placeholder::Color, fallback: Color32) -> Color32 {
    transferbuddy_core::terminal::rgb(color)
        .map(|[r, g, b]| Color32::from_rgb(r, g, b))
        .unwrap_or(fallback)
}
use transferbuddy_core::terminal_types as vt100_placeholder;

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{
        kittest::{NodeT, Queryable},
        Harness,
    };
    use std::sync::Arc;
    fn fixture() -> (tempfile::TempDir, tokio::runtime::Runtime, SharedApp) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("file.txt"), b"abc").unwrap();
        let mut cfg = Config {
            root,
            config_dir: dir.path().join("cfg"),
            sound: false,
            ..Default::default()
        };
        cfg.apply_cli(&StartupOptions::default(), false).unwrap();
        Preferences {
            scale: 1.0,
            compact: false,
            ..Default::default()
        }
        .save(&cfg.config_dir)
        .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let app = App::new(cfg, runtime.handle().clone(), false);
        (dir, runtime, app)
    }
    fn harness(app: SharedApp) -> Harness<'static, Desktop> {
        let runtime = app.runtime.clone();
        let config = app.config.read().unwrap().config_dir.join("config.toml");
        Harness::builder()
            .with_size(egui::vec2(1280.0, 840.0))
            .build_eframe(move |cc| {
                let (tx, rx) = mpsc::channel();
                let mut d = Desktop::new(&cc.egui_ctx, runtime, Some(config), None, tx, rx);
                d.snapshot = app.engine().snapshot();
                d.app = Some(app);
                d
            })
    }
    fn settle_steps(h: &mut Harness<'static, Desktop>, steps: usize) {
        h.run_steps(steps);
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while h.state().pending_submissions > 0 {
            assert!(
                std::time::Instant::now() < until,
                "submission worker did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
            h.run_steps(1);
        }
    }
    fn device(
        app: &SharedApp,
    ) -> (
        Arc<transferbuddy_core::switch::Switch>,
        tokio::sync::mpsc::UnboundedReceiver<transferbuddy_core::switch::Job>,
    ) {
        use transferbuddy_core::switch::{Facts, Switch};
        let sw = Switch::for_test(
            "127.0.0.1",
            SwitchState::Ready,
            Facts {
                flash_device: "flash:".into(),
                version: Some(cisco::parse_show_version(include_str!(
                    "../../../testdata/show_version_c9200l.txt"
                ))),
                ..Default::default()
            },
            vec![],
        );
        let jobs = sw.test_job_receiver();
        app.switches.add_for_test(sw.clone());
        (sw, jobs)
    }
    #[test]
    fn frontend_catalog_covers_shared_actions() {
        assert_eq!(SUPPORTED_ACTIONS, engine::ACTIONS);
    }
    #[test]
    fn all_five_views_render_and_connect_form_masks_passwords() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app);
        for tab in [
            Tab::Dashboard,
            Tab::Connect,
            Tab::Transfer,
            Tab::Upgrade,
            Tab::Logs,
        ] {
            h.state_mut().tab = tab;
            settle_steps(&mut h, 2);
        }
        h.state_mut().tab = Tab::Connect;
        settle_steps(&mut h, 2);
        h.get_by_label("Add device").click();
        settle_steps(&mut h, 2);
        assert!(h.state().connect.is_some());
        assert_eq!(h.state().connect.as_ref().unwrap().port, "22");
        h.state_mut().connect.as_mut().unwrap().password = "visible-login".into();
        h.state_mut().connect.as_mut().unwrap().enable = "visible-enable".into();
        settle_steps(&mut h, 2);
        for password in ["visible-login", "visible-enable"] {
            assert!(
                h.query_by_value(password).is_none(),
                "SSH passwords must be masked"
            );
        }
        assert_eq!(
            h.state().connect.as_ref().unwrap().password,
            "visible-login"
        );
    }
    #[test]
    fn gui_fields_support_control_select_all_copy_and_paste_and_native_edit_menu() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app);
        h.state_mut().connect = Some(ConnectionForm {
            user: "netadmin".into(),
            port: "22".into(),
            ..Default::default()
        });
        settle_steps(&mut h, 2);
        assert!(h.state().connect.as_ref().unwrap().targets.is_empty());
        h.get_all_by_value("netadmin")
            .find(|node| node.accesskit_node().role() == egui::accesskit::Role::TextInput)
            .unwrap()
            .click();
        settle_steps(&mut h, 1);
        h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::A);
        settle_steps(&mut h, 1);
        h.event(egui::Event::Copy);
        settle_steps(&mut h, 1);
        assert!(h
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(s) if s == "netadmin")));
        h.event(egui::Event::Paste("replacement".into()));
        settle_steps(&mut h, 1);
        assert_eq!(h.state().connect.as_ref().unwrap().user, "replacement");
        h.state()
            .tx
            .send(UiEvent::Native(NativeAction::SelectAll))
            .unwrap();
        settle_steps(&mut h, 1);
        h.state()
            .tx
            .send(UiEvent::Native(NativeAction::Cut))
            .unwrap();
        settle_steps(&mut h, 1);
        assert_eq!(h.state().connect.as_ref().unwrap().user, "");
    }
    #[test]
    fn upgrade_release_change_and_log_context_are_visible() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let filename = "cat9k_lite_iosxe.17.15.06.SPA.bin";
        std::fs::write(app.config.read().unwrap().root.join(filename), b"image").unwrap();
        app.engine()
            .submit(Command::AssignImage {
                devices: vec![sw.id],
                local: filename.into(),
            })
            .unwrap();
        let command = "copy ftp://cisco:cisco123@127.0.0.1/image.bin flash:image.bin";
        app.logger.log(
            transferbuddy_core::logging::Event::new(
                transferbuddy_core::logging::LogLevel::Info,
                "FTP",
                "copy command",
            )
            .ip("127.0.0.1".parse().unwrap())
            .device("switch-lab".into(), Some("C9200L-48P-4X".into()))
            .command(command.into()),
        );
        let mut h = harness(app);
        h.state_mut().tab = Tab::Upgrade;
        settle_steps(&mut h, 2);
        h.get_by_label("17.15.3 → 17.15.6");
        h.get_by_label("5 Logs").click();
        settle_steps(&mut h, 2);
        for label in [
            "Hostname / IP",
            "Model",
            "Protocol",
            "switch-lab",
            "C9200L-48P-4X",
            command,
        ] {
            h.get_by_label(label);
        }
    }
    #[test]
    fn info_help_header_and_device_remove_are_discoverable() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app.clone());
        settle_steps(&mut h, 2);
        let logo = h.get_by_label("TRANSFERBUDDY").rect();
        let status = h.get_by_label_contains("LIVE SERVICES").rect();
        assert!((logo.center().y - status.center().y).abs() < 10.0);
        assert!(status.min.x > logo.max.x);
        h.get_by_label("Info").click();
        settle_steps(&mut h, 1);
        h.get_by_label("Created by Samuel Heinrich");
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(!h.state().info_open);
        h.get_by_label("Help").click();
        settle_steps(&mut h, 1);
        h.get_by_label_contains("SSH: network versus negotiation");
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        h.get_by_label("2 Connect").click();
        settle_steps(&mut h, 2);
        h.get_by_label("Remove").click();
        settle_steps(&mut h, 2);
        assert!(app.engine().device(sw.id).is_err());
        assert!(h.state().snapshot.devices.is_empty());
    }
    #[test]
    fn dashboard_groups_actions_and_exposes_status_as_plain_text() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app);
        settle_steps(&mut h, 3);
        let status = h.get_by_label_contains("LIVE SERVICES");
        assert_eq!(status.accesskit_node().role(), egui::accesskit::Role::Label);
        let service = h
            .get_by_role_and_label(egui::accesskit::Role::CheckBox, "HTTP")
            .rect();
        let starts = h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Start");
        let start = starts
            .map(|s| s.rect())
            .min_by(|a, b| {
                (a.center().y - service.center().y)
                    .abs()
                    .total_cmp(&(b.center().y - service.center().y).abs())
            })
            .unwrap();
        assert!((start.center().y - service.center().y).abs() < 30.0);
        assert!(
            start.min.x - service.max.x < 130.0,
            "Start belongs beside its service"
        );
        let address = h.get_by_label("Addresses in copy URLs").rect();
        assert!(
            address.min.x > start.max.x,
            "Interface selection belongs in the right pane"
        );
    }
    #[test]
    fn clicked_file_focus_accepts_enter_and_device_enter_only_opens_picker() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        app.engine().submit(Command::ListLocal("".into())).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !app.engine().snapshot().listings.contains_key("") {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut h = harness(app);
        h.state_mut().tab = Tab::Transfer;
        h.state_mut().device = Some(sw.id);
        h.state_mut().remote_loaded = Some((sw.id, "flash:".into()));
        settle_steps(&mut h, 2);
        h.get_by_label_contains("sub/").click();
        settle_steps(&mut h, 1);
        assert_eq!(
            h.state().focus,
            1,
            "Clicking a local entry selects its pane"
        );
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 1);
        assert_eq!(h.state().local_dir, "sub");
        h.state_mut().focus = 0;
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 1);
        assert!(h.state().protocol_picker.is_some());
        assert!(!sw.protocol_chosen());
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 1);
        assert!(h.state().protocol_picker.is_none());
    }
    #[test]
    fn local_enter_navigates_and_parent_returns_to_root() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app);
        h.state_mut().local_selection = Some(FileEntry {
            name: "sub".into(),
            is_dir: true,
            size: 0,
            modified: None,
            ext: String::new(),
        });
        h.state_mut().enter_local();
        assert_eq!(h.state().local_dir, "sub");
        h.state_mut().local_selection = Some(FileEntry {
            name: "..".into(),
            is_dir: true,
            size: 0,
            modified: None,
            ext: String::new(),
        });
        h.state_mut().enter_local();
        assert_eq!(h.state().local_dir, "");
    }
    #[test]
    fn escape_returns_from_cli_and_ctrl_c_goes_to_device() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut input = sw.test_cli_receiver();
        let mut h = harness(app);
        h.state_mut().cli = Some(sw.id);
        settle_steps(&mut h, 2);
        h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::C);
        settle_steps(&mut h, 1);
        assert_eq!(input.try_recv().unwrap(), vec![3]);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(h.state().cli.is_none());
        assert!(sw.cli_open(), "Minimizing must keep SSH open");
        assert!(h.state().console_windows[&sw.id].minimized);
        assert!(input.try_recv().is_err());
    }
    #[test]
    fn confirmations_require_typed_y_and_a_separate_button_and_tab_does_not_cancel() {
        for (text, yes) in [("y", true), ("Y", false), ("yes", false)] {
            let (_dir, _runtime, app) = fixture();
            let (sw, mut jobs) = device(&app);
            let request = app
                .engine()
                .submit(Command::RequestDelete {
                    device: sw.id,
                    remote: "flash:file.txt".into(),
                    recursive: false,
                })
                .unwrap();
            let mut h = harness(app);
            settle_steps(&mut h, 3);
            h.event(egui::Event::Text(text.into()));
            settle_steps(&mut h, 2);
            assert!(jobs.try_recv().is_err());
            h.key_press(egui::Key::Tab);
            settle_steps(&mut h, 1);
            assert_eq!(h.state().confirmation.as_ref().unwrap().id, request);
            if yes {
                h.get_by_label("Confirm").click();
                settle_steps(&mut h, 2);
                assert!(
                    matches!(jobs.try_recv().unwrap().untracked(), transferbuddy_core::switch::Job::Delete { path, .. } if path == "flash:file.txt")
                );
            } else {
                assert!(jobs.try_recv().is_err());
            }
        }
    }
    #[test]
    fn pasted_y_cannot_approve_and_escape_cancels() {
        let (_dir, _runtime, app) = fixture();
        let (sw, mut jobs) = device(&app);
        app.engine()
            .submit(Command::RequestDelete {
                device: sw.id,
                remote: "flash:file.txt".into(),
                recursive: false,
            })
            .unwrap();
        let mut h = harness(app);
        settle_steps(&mut h, 3);
        h.event(egui::Event::Paste("y".into()));
        settle_steps(&mut h, 2);
        assert_ne!(h.state().confirmation_text, "y");
        assert!(jobs.try_recv().is_err());
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 2);
        assert!(h.state().confirmation.is_none());
        assert!(jobs.try_recv().is_err());
        assert_eq!(
            cli_bytes(&egui::Event::Paste("show version\n".into())).unwrap(),
            b"show version\r"
        );
    }
    #[test]
    fn protocol_enter_accepts_priority_and_does_not_reopen_picker() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut().device = Some(sw.id);
        h.state_mut().protocol_picker = Some((sw.id, false, false));
        settle_steps(&mut h, 2);
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 1);
        assert!(h.state().protocol_picker.is_none());
        assert_eq!(sw.protocol(), Protocol::Sftp);
        assert!(sw.protocol_chosen());
    }
    fn list_root(app: &SharedApp) {
        app.engine().submit(Command::ListLocal("".into())).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while !app.engine().snapshot().listings.contains_key("") {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn file_enter_opens_details_and_explicit_copy_creates_a_frozen_job() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        app.engine()
            .submit(Command::ChooseProtocol(sw.id, Protocol::Http))
            .unwrap();
        list_root(&app);
        let mut h = harness(app.clone());
        h.state_mut().tab = Tab::Transfer;
        h.state_mut().device = Some(sw.id);
        h.state_mut().remote_loaded = Some((sw.id, "flash:".into()));
        settle_steps(&mut h, 2);
        h.get_by_label_contains("file.txt").click();
        settle_steps(&mut h, 1);
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 1);
        assert_eq!(h.state().file_details.as_ref().unwrap().0, "file.txt");
        assert!(!app
            .engine()
            .snapshot()
            .operations
            .iter()
            .any(|o| o.transfer.is_some()));
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        sw.set_state_for_test(SwitchState::Busy {
            what: "another command".into(),
        });
        settle_steps(&mut h, 1);
        h.get_by_label("Copy 1 file to device →").click();
        settle_steps(&mut h, 1);
        let op = app
            .engine()
            .snapshot()
            .operations
            .into_iter()
            .find(|o| o.transfer.is_some())
            .unwrap();
        let copy = op.transfer.unwrap();
        assert_eq!(
            (copy.local.as_str(), copy.remote.as_str(), copy.protocol),
            ("file.txt", "flash:file.txt", Protocol::Http)
        );
        assert!(h.state().jobs_open);
        h.state_mut().local_dir = "sub".into();
        h.state_mut().remote_dir = "elsewhere".into();
        let frozen = app
            .engine()
            .snapshot()
            .operations
            .into_iter()
            .find(|o| o.id == op.id)
            .unwrap()
            .transfer
            .unwrap();
        assert_eq!(
            (frozen.local.as_str(), frozen.remote.as_str()),
            ("file.txt", "flash:file.txt")
        );
    }
    #[test]
    fn command_click_deselects_and_shift_selects_the_visible_range() {
        let (_dir, _runtime, app) = fixture();
        list_root(&app);
        let mut h = harness(app);
        h.state_mut().tab = Tab::Transfer;
        settle_steps(&mut h, 2);
        h.get_by_label_contains("file.txt").click();
        settle_steps(&mut h, 1);
        assert_eq!(h.state().selected_file_names(false), ["file.txt"]);
        h.get_by_label_contains("file.txt")
            .click_modifiers(egui::Modifiers::COMMAND);
        settle_steps(&mut h, 1);
        assert!(h.state().selected_file_names(false).is_empty());
        h.get_by_label_contains("sub/").click();
        settle_steps(&mut h, 1);
        h.get_by_label_contains("file.txt")
            .click_modifiers(egui::Modifiers::SHIFT);
        settle_steps(&mut h, 1);
        assert_eq!(h.state().local_selected.len(), 2);
        assert_eq!(h.state().selected_file_names(false), ["file.txt"]);
    }
    #[test]
    fn cli_toolbar_keyboard_does_not_reach_the_switch_and_details_escape_keeps_cli() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut input = sw.test_cli_receiver();
        let mut h = harness(app);
        h.state_mut().cli = Some(sw.id);
        h.state_mut().cli_new = true;
        settle_steps(&mut h, 2);
        h.get_by_label("Copy transcript").click();
        settle_steps(&mut h, 1);
        h.key_press(egui::Key::Tab);
        settle_steps(&mut h, 1);
        assert!(
            input.try_recv().is_err(),
            "Toolbar navigation must stay in the GUI"
        );
        h.state_mut().file_details = Some(("file.txt".into(), 3, false));
        settle_steps(&mut h, 1);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(h.state().file_details.is_none());
        assert!(h.state().cli.is_some());
    }
    #[test]
    fn root_change_cancels_pending_transfer_intents() {
        let (dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        list_root(&app);
        let mut h = harness(app);
        h.state_mut().device = Some(sw.id);
        h.state_mut().local_selected.insert("file.txt".into());
        h.state_mut().transfer(false);
        assert!(h.state().protocol_picker.is_some());
        let root = dir.path().join("other");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("file.txt"), b"different").unwrap();
        h.state_mut().send(Command::ChangeRoot(root.clone()));
        settle_steps(&mut h, 2);
        assert_eq!(h.state().prefs.root.as_ref(), Some(&root));
        assert!(h.state().pending_transfers.is_none());
        assert!(h.state().protocol_picker.is_none());
    }
    #[test]
    fn nested_remote_drop_keeps_the_filesystem_and_current_local_destination() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut().device = Some(sw.id);
        h.state_mut().local_dir = "sub".into();
        h.state_mut().remote_dir = "other".into();
        h.state_mut().drop_files(
            &workspace::FileDrag {
                receive: true,
                device: Some(sw.id),
                directory: "flash:configs".into(),
                names: vec!["backup.cfg".into()],
            },
            false,
        );
        let copy = &h.state().pending_transfers.as_ref().unwrap()[0];
        assert_eq!(
            (copy.local.as_str(), copy.remote.as_str()),
            ("sub/backup.cfg", "flash:configs/backup.cfg")
        );
        assert_eq!(h.state().remote_dir, "other");
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 2);
        assert!(h.state().pending_transfers.is_none());
    }

    #[test]
    fn retry_interface_applies_before_queuing_and_cancel_does_nothing() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        sw.set_state_for_test(SwitchState::Offline {
            reason: "VPN lost".into(),
        });
        let engine = app.engine();
        let id = engine
            .submit(Command::QueueTransfers(vec![engine::TransferRequest {
                device: sw.id,
                local: "file.txt".into(),
                remote: "flash:file.txt".into(),
                receive: false,
                protocol: Protocol::Http,
                overwrite: false,
                platform_check: false,
            }]))
            .unwrap();
        engine.submit(Command::CancelOperation(id)).unwrap();
        let mut h = harness(app.clone());
        h.state_mut().open_retry_interface(id);
        settle_steps(&mut h, 2);
        let previous = app.config.read().unwrap().advertise.clone();
        h.get_by_label("Cancel").click();
        settle_steps(&mut h, 2);
        assert!(h.state().retry_interface.is_none());
        assert_eq!(app.config.read().unwrap().advertise, previous);
        h.state_mut().open_retry_interface(id);
        h.state_mut().retry_choice = Some("127.0.0.1".into());
        settle_steps(&mut h, 2);
        h.get_by_label("Apply interface & retry").click();
        settle_steps(&mut h, 3);
        assert_eq!(
            app.config.read().unwrap().advertise.as_deref(),
            Some("127.0.0.1")
        );
        assert!(h.state().retry_interface.is_none());
        assert!(engine.snapshot().operations.iter().any(|o| o.id != id
            && o.transfer
                .as_ref()
                .is_some_and(|t| t.local == "file.txt" && t.remote == "flash:file.txt")));
    }
    #[test]
    fn paused_job_menu_cancels_without_remote_work_and_retry_keeps_the_request() {
        let (_dir, _runtime, app) = fixture();
        let (sw, mut jobs) = device(&app);
        sw.set_state_for_test(SwitchState::Offline {
            reason: "VPN lost".into(),
        });
        let id = app
            .engine()
            .submit(Command::QueueTransfers(vec![engine::TransferRequest {
                device: sw.id,
                local: "file.txt".into(),
                remote: "flash:file.txt".into(),
                receive: false,
                protocol: Protocol::Http,
                overwrite: false,
                platform_check: false,
            }]))
            .unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while !app
            .engine()
            .snapshot()
            .operations
            .iter()
            .any(|o| o.id == id && o.state == engine::OperationState::Paused)
        {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut h = harness(app.clone());
        h.state_mut().jobs_open = true;
        h.state_mut().job_selected = Some(id);
        h.state_mut().prefs.scale = 1.5;
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(600.0, 400.0));
        settle_steps(&mut h, 3);
        let action = h.get_by_role_and_label(egui::accesskit::Role::Button, "Actions");
        assert!(action.rect().min.y > 0.0);
        assert!(action.rect().max.y < 400.0 * h.ctx.pixels_per_point());
        action.click_accesskit();
        settle_steps(&mut h, 3);
        scaled_click(&mut h, "Cancel job");
        settle_steps(&mut h, 2);
        assert_eq!(
            app.engine()
                .snapshot()
                .operations
                .iter()
                .find(|o| o.id == id)
                .unwrap()
                .state,
            engine::OperationState::Cancelled,
            "Cancel should complete: {} ({} pending submissions)",
            h.state().status,
            h.state().pending_submissions
        );
        assert!(jobs.try_recv().is_err());
        scaled_click(&mut h, "Retry");
        settle_steps(&mut h, 2);
        let snapshot = app.engine().snapshot();
        let retried = snapshot
            .operations
            .iter()
            .rfind(|o| o.transfer.is_some())
            .unwrap();
        assert_ne!(retried.id, id);
        let spec = retried.transfer.as_ref().unwrap();
        assert_eq!(
            (spec.local.as_str(), spec.remote.as_str(), spec.protocol),
            ("file.txt", "flash:file.txt", Protocol::Http)
        );
        assert!(jobs.try_recv().is_err());
    }

    #[test]
    fn cli_windows_keep_connections_and_route_input_only_to_the_focused_device() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 2);
        let first = app.engine().device(1).unwrap();
        let second = app.engine().device(2).unwrap();
        let mut h = harness(app);
        h.state_mut().open_cli(1);
        settle_steps(&mut h, 3);
        let mut first_input = first.test_cli_receiver();
        h.state_mut().open_cli(2);
        settle_steps(&mut h, 3);
        let mut second_input = second.test_cli_receiver();
        assert!(first.cli_open() && second.cli_open());
        assert_eq!(h.state().console_windows.len(), 2);
        h.event(egui::Event::Paste("show version\n".into()));
        settle_steps(&mut h, 2);
        assert_eq!(second_input.try_recv().unwrap(), b"show version\r");
        assert!(first_input.try_recv().is_err());
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 2);
        assert!(h.state().console_windows[&2].minimized);
        assert!(second.cli_open());
        h.state_mut().restore_console(2);
        settle_steps(&mut h, 2);
        h.get_by_role_and_label(egui::accesskit::Role::Button, "CLI access-01")
            .click();
        settle_steps(&mut h, 2);
        h.get_by_label("Terminal for access-01").click();
        settle_steps(&mut h, 2);
        h.event(egui::Event::Text("x".into()));
        settle_steps(&mut h, 2);
        assert_eq!(first_input.try_recv().unwrap(), b"x");
        assert!(second_input.try_recv().is_err());
        let ctx = h.ctx.clone();
        h.state_mut().close_console(1, &ctx);
        settle_steps(&mut h, 2);
        assert!(!first.cli_open());
        assert!(second.cli_open());
        assert_eq!(h.state().console_windows.len(), 1);
    }
    #[test]
    fn cli_selection_copies_without_interrupt_and_background_minimizes() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut input = sw.test_cli_receiver();
        sw.test_cli_output(b"switch# show version\r\nCisco IOS XE\r\n");
        let mut h = harness(app);
        h.state_mut().cli = Some(sw.id);
        settle_steps(&mut h, 3);
        let id = egui::Id::new(("terminal_input", sw.id));
        let mut state = egui::text_edit::TextEditState::load(&h.ctx, id).unwrap();
        state
            .cursor
            .set_char_range(Some(egui::text::CCursorRange::two(
                egui::text::CCursor::new(0),
                egui::text::CCursor::new(7),
            )));
        state.store(&h.ctx, id);
        h.event(egui::Event::Copy);
        settle_steps(&mut h, 1);
        assert!(
            input.try_recv().is_err(),
            "Copy must not send Ctrl+C to SSH"
        );
        assert!(h
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == "switch#")));
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowRight);
        settle_steps(&mut h, 1);
        assert!(input.try_recv().is_err(), "Selection keys stay local");
        h.event(egui::Event::Paste("show clock\n".into()));
        settle_steps(&mut h, 2);
        assert_eq!(input.try_recv().unwrap(), b"show clock\r");
        h.get_by_label("2 Connect").click();
        settle_steps(&mut h, 2);
        assert!(h.state().console_windows[&sw.id].minimized);
        assert!(sw.cli_open());
        let tab = format!("CLI {}", h.state().snapshot.devices[0].name);
        h.get_by_role_and_label(egui::accesskit::Role::Button, &tab)
            .click();
        settle_steps(&mut h, 2);
        assert!(!h.state().console_windows[&sw.id].minimized);
        assert!(sw.cli_open());
        let window_id = egui::Id::new(("console_window", sw.id));
        let before = h.ctx.memory(|m| m.area_rect(window_id)).unwrap();
        let d = &h.state().snapshot.devices[0];
        let title = format!("CLI · {} · {}", d.name, d.host);
        let title_rect = h
            .get_by_role_and_label(egui::accesskit::Role::Label, &title)
            .rect();
        let from = egui::pos2(title_rect.min.x + 40.0, title_rect.center().y);
        let to = from + egui::vec2(45.0, 25.0);
        h.hover_at(from);
        settle_steps(&mut h, 1);
        h.drag_at(from);
        settle_steps(&mut h, 1);
        h.hover_at(to);
        settle_steps(&mut h, 2);
        h.drop_at(to);
        settle_steps(&mut h, 2);
        let after = h.ctx.memory(|m| m.area_rect(window_id)).unwrap();
        assert!(
            after.min.distance(before.min) > 20.0,
            "Console header must drag the window"
        );
    }
    #[test]
    fn removed_device_releases_cli_focus_and_gui_shortcuts() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let _input = sw.test_cli_receiver();
        let mut h = harness(app.clone());
        h.state_mut().cli = Some(sw.id);
        settle_steps(&mut h, 3);
        app.switches.forget(sw.id);
        h.state_mut().snapshot = app.engine().snapshot();
        settle_steps(&mut h, 2);
        assert!(h.state().console_windows.is_empty());
        assert!(h.state().cli.is_none());
        assert!(!h.state().cli_focus);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        settle_steps(&mut h, 2);
        assert!(h.state().palette_open);
    }
    #[test]
    fn rejected_cli_open_can_retry_while_other_submissions_are_pending() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut()
            .console_windows
            .insert(sw.id, console::ConsoleWindow::new(true));
        h.state_mut().restore_console(sw.id);
        h.state_mut().pending_submissions = 2;
        h.state_mut().submitted(submission::Outcome {
            result: Err("device is busy".into()),
            context: submission::Context::Cli(sw.id),
            quiet: false,
            queued: false,
        });
        h.run_steps(2);
        assert_eq!(h.state().pending_submissions, 1);
        assert!(!h.state().console_windows[&sw.id].cli);
        assert!(h.state().cli.is_none());
        h.state_mut().pending_submissions = 0;
        h.state_mut().open_cli(sw.id);
        settle_steps(&mut h, 3);
        assert!(sw.cli_open());
        assert!(h.state().console_windows[&sw.id].cli);
    }
    #[test]
    fn native_quit_asks_while_jobs_run() {
        let (_dir, _runtime, app) = fixture();
        let (_sw, _jobs) = device(&app);
        app.engine().submit(Command::RemoveInactive(1)).unwrap();
        let mut h = harness(app);
        h.state_mut()
            .tx
            .send(UiEvent::Native(NativeAction::Quit))
            .unwrap();
        settle_steps(&mut h, 2);
        assert!(h.state().quit_question);
        assert!(!h.state().quitting);
    }
    #[test]
    fn upgrade_buttons_and_gates_use_shared_verified_state() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut().request_install(sw.id, false);
        settle_steps(&mut h, 1);
        assert!(h.state().confirmation.is_none());
        sw.set_upgrade(upgrade::Progress::Verified {
            remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
            md5: "900150983cd24fb0d6963f7d28e17f72".into(),
            version: Some("17.15.6".into()),
        });
        h.state_mut().request_install(sw.id, false);
        settle_steps(&mut h, 1);
        assert!(h.state().confirmation.is_some());
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn unreachable_connection_shows_network_privacy_help_and_reconnect() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        sw.set_state_for_test(SwitchState::Failed {
            reason: "TCP connection failed before SSH negotiation: No route to host (os error 65)"
                .into(),
        });
        let mut h = harness(app);
        h.state_mut().tab = Tab::Connect;
        h.state_mut().device = Some(sw.id);
        settle_steps(&mut h, 3);
        h.get_by_label("Open Local Network settings");
        h.get_by_label("Reconnect");
        h.get_by_label_contains("If this host works in Terminal");
    }
    fn preview_devices(
        app: &SharedApp,
        count: usize,
    ) -> Vec<tokio::sync::mpsc::UnboundedReceiver<transferbuddy_core::switch::Job>> {
        use transferbuddy_core::switch::{Facts, Line, LineKind, Switch};
        let mut jobs = Vec::new();
        for i in 0..count {
            let mut sw = Switch::for_test(
                &format!("192.168.22.{}", 11 + i),
                SwitchState::Ready,
                Facts {
                    hostname: Some(format!("access-{:02}", i + 1)),
                    flash_device: "flash:".into(),
                    flash: Some(cisco::FlashUsage {
                        total: 4_000_000_000,
                        free: if i == 2 { 20_000 } else { 2_300_000_000 },
                    }),
                    version: Some(cisco::parse_show_version(include_str!(
                        "../../../testdata/show_version_c9200l.txt"
                    ))),
                    ..Default::default()
                },
                vec![
                    Line {
                        kind: LineKind::Info,
                        text: "SSH session established".into(),
                    },
                    Line {
                        kind: LineKind::Sent,
                        text: "show version".into(),
                    },
                    Line {
                        kind: LineKind::Output,
                        text: "Cisco IOS XE Software, Version 17.15.03".into(),
                    },
                ],
            );
            Arc::get_mut(&mut sw).unwrap().id = i as u64 + 1;
            sw.test_listing(
                "flash:",
                vec![
                    cisco::RemoteFile {
                        name: "logs".into(),
                        is_dir: true,
                        size: 0,
                    },
                    cisco::RemoteFile {
                        name: "packages.conf".into(),
                        is_dir: false,
                        size: 1124,
                    },
                    cisco::RemoteFile {
                        name: "cat9k_lite-rpbase.17.15.03.SPA.pkg".into(),
                        is_dir: false,
                        size: 330_000_000,
                    },
                    cisco::RemoteFile {
                        name: "startup-config".into(),
                        is_dir: false,
                        size: 14280,
                    },
                ],
            );
            jobs.push(sw.test_job_receiver());
            app.switches.add_for_test(sw.clone());
            if i == 1 {
                sw.set_upgrade(upgrade::Progress::Verified {
                    remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
                    md5: "900150983cd24fb0d6963f7d28e17f72".into(),
                    version: Some("17.15.6".into()),
                });
            }
            if i == 3 {
                sw.set_upgrade(upgrade::Progress::Rebooting {
                    since: std::time::Instant::now() - Duration::from_secs(380),
                    attempts: 2,
                    last_error: Some("SSH service not ready — retrying".into()),
                });
            }
            if i == 4 {
                sw.set_state_for_test(SwitchState::Offline {
                    reason: "VPN connection lost".into(),
                });
            }
        }
        jobs
    }
    #[test]
    fn keyboard_navigation_and_palette_preserve_text_input_and_cli() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut input = sw.test_cli_receiver();
        let mut h = harness(app);
        settle_steps(&mut h, 2);
        h.key_press(egui::Key::Num3);
        settle_steps(&mut h, 1);
        assert_eq!(
            h.state().tab,
            Tab::Dashboard,
            "Bare digits must not switch views"
        );
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num3);
        settle_steps(&mut h, 1);
        assert_eq!(h.state().tab, Tab::Transfer);
        h.key_press(egui::Key::Tab);
        settle_steps(&mut h, 1);
        assert!(
            h.ctx.memory(|m| m.focused().is_some()),
            "Tab must focus a real control"
        );
        h.state_mut().cli = Some(sw.id);
        h.state_mut().cli_focus = true;
        h.state_mut().cli_new = true;
        settle_steps(&mut h, 2);
        h.event(egui::Event::Text("3".into()));
        settle_steps(&mut h, 1);
        assert_eq!(input.try_recv().unwrap(), b"3");
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::K);
        settle_steps(&mut h, 1);
        assert!(h.state().palette_open);
        assert!(input.try_recv().is_err());
        h.event(egui::Event::Text("Connect".into()));
        settle_steps(&mut h, 1);
        assert!(input.try_recv().is_err());
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(!h.state().palette_open);
        assert!(h.state().cli.is_some());
    }
    fn scaled_click(h: &mut Harness<'static, Desktop>, label: &str) {
        // egui_kittest 0.35 Node::click forwards physical AccessKit bounds as points.
        let pos = h.get_by_label(label).rect().center() / h.ctx.pixels_per_point();
        h.event(egui::Event::PointerMoved(pos));
        for pressed in [true, false] {
            h.event(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            });
        }
    }
    #[test]
    fn zoomed_minimum_window_can_browse_both_sides_and_reach_upgrade_jobs() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 3);
        list_root(&app);
        let mut h = harness(app);
        h.state_mut().tab = Tab::Transfer;
        h.state_mut().prefs.scale = 1.5;
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(600.0, 400.0));
        settle_steps(&mut h, 3);
        let rect = h.get_by_label_contains("sub/").rect();
        assert!(rect.min.y > 0.0 && rect.max.y < 400.0 * h.ctx.pixels_per_point());
        scaled_click(&mut h, "Remote files");
        settle_steps(&mut h, 2);
        assert_eq!(h.state().focus, 2);
        assert!(h.query_by_label_contains("logs/").is_some());
        h.state_mut().tab = Tab::Upgrade;
        settle_steps(&mut h, 2);
        scaled_click(&mut h, "Device jobs");
        settle_steps(&mut h, 2);
        assert!(!h.state().upgrade_images_open);
        assert!(h.query_by_label_contains("access-01").is_some());
    }
    #[test]
    fn upgrade_browser_enter_opens_folders_and_device_workspaces_restore_selection() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 2);
        list_root(&app);
        let mut h = harness(app);
        h.state_mut().tab = Tab::Upgrade;
        settle_steps(&mut h, 2);
        h.get_by_label_contains("sub/").click();
        settle_steps(&mut h, 1);
        h.key_press(egui::Key::Enter);
        settle_steps(&mut h, 2);
        assert_eq!(h.state().local_dir, "sub");
        h.state_mut().choose_device(1);
        h.state_mut().local_dir = "sub".into();
        h.state_mut().filter = "cfg".into();
        h.state_mut().remote_dir = "configs".into();
        h.state_mut().choose_device(2);
        assert_eq!(h.state().local_dir, "");
        h.state_mut().choose_device(1);
        assert_eq!(
            (
                h.state().local_dir.as_str(),
                h.state().remote_dir.as_str(),
                h.state().filter.as_str()
            ),
            ("sub", "configs", "cfg")
        );
    }

    #[test]
    fn minimum_window_keeps_navigation_and_transfer_actions_visible() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 12);
        let mut h = harness(app);
        h.state_mut().tab = Tab::Transfer;
        h.set_size(egui::vec2(1440.0, 900.0));
        settle_steps(&mut h, 3);
        h.set_size(egui::vec2(900.0, 600.0));
        settle_steps(&mut h, 3);
        for name in ["1 Dashboard", "5 Logs", "Copy 0 files to device →"] {
            let r = h.get_by_label(name).rect();
            assert!(
                r.left() >= 0.0 && r.right() <= 900.0 && r.bottom() <= 600.0,
                "{name}: {r:?}"
            );
        }
    }
    #[test]
    #[ignore = "CPU layout profile with 100 devices, 10,000 files and 5,000 logs"]
    fn large_workspace_profile() {
        let (_dir, _runtime, app) = fixture();
        let root = app.config.read().unwrap().root.clone();
        for i in 0..10_000 {
            std::fs::write(root.join(format!("backup-{i:05}.cfg")), b"preview").unwrap();
        }
        let _jobs = preview_devices(&app, 100);
        for i in 0..5000 {
            app.logger.log_simple(
                transferbuddy_core::logging::LogLevel::Info,
                "switch",
                format!("Connection diagnostic {i:05}"),
            );
        }
        app.engine().submit(Command::ListLocal("".into())).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while !app.engine().snapshot().listings.contains_key("") {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut h = harness(app);
        for tab in [Tab::Connect, Tab::Transfer, Tab::Upgrade, Tab::Logs] {
            h.state_mut().tab = tab;
            settle_steps(&mut h, 3);
            let mut samples = Vec::new();
            for _ in 0..30 {
                let start = std::time::Instant::now();
                settle_steps(&mut h, 1);
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{} CPU layout: median {:.2} ms, p95 {:.2} ms",
                tab.name(),
                samples[15],
                samples[28]
            );
        }
    }
    #[test]
    #[ignore = "Generate GUI review images; requires a GPU adapter"]
    fn desktop_preview() {
        let (_dir, _runtime, app) = fixture();
        {
            let mut config = app.config.write().unwrap();
            for (id, port) in [
                (ServiceId::Http, 8080),
                (ServiceId::Https, 8443),
                (ServiceId::Ftp, 2121),
                (ServiceId::Ssh, 2222),
                (ServiceId::Tftp, 69),
            ] {
                let service = config.service_mut(id);
                service.port = port;
                service.bind = "0.0.0.0".into();
                service.enabled = matches!(id, ServiceId::Http | ServiceId::Ftp | ServiceId::Ssh);
            }
        }
        let root = app.config.read().unwrap().root.clone();
        for name in [
            "cat9k_lite_iosxe.17.15.06.SPA.bin",
            "cat9k_lite-rpbase.17.15.03.SPA.pkg",
            "packages.conf",
            "switch-backup.cfg",
        ] {
            std::fs::write(root.join(name), b"preview").unwrap();
        }
        std::fs::OpenOptions::new()
            .write(true)
            .open(root.join("cat9k_lite_iosxe.17.15.06.SPA.bin"))
            .unwrap()
            .set_len(600_000_000)
            .unwrap();
        let _jobs = preview_devices(&app, 12);
        app.engine()
            .submit(Command::AssignImage {
                devices: vec![1, 2, 3],
                local: "cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
            })
            .unwrap();
        app.engine()
            .device(2)
            .unwrap()
            .set_upgrade(upgrade::Progress::Verified {
                remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
                md5: "900150983cd24fb0d6963f7d28e17f72".into(),
                version: Some("17.15.6".into()),
            });
        use transferbuddy_core::logging::{Event, LogLevel};
        for (level, action, result) in [
            (LogLevel::Info, "SSH connection established", "ready"),
            (LogLevel::Info, "Image transferred", "100%"),
            (LogLevel::Info, "Remote MD5 matches local image", "verified"),
            (
                LogLevel::Warning,
                "SSH service not ready after reload",
                "retrying",
            ),
            (LogLevel::Error, "Connection lost", "reconnect available"),
        ] {
            app.logger.log(
                Event::new(level, "switch", action)
                    .ip("192.168.22.12".parse().unwrap())
                    .result(result),
            );
        }
        app.engine().submit(Command::ListLocal("".into())).unwrap();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while !app.engine().snapshot().listings.contains_key("") {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        // Documentation devices are fixtures rather than routable hosts. Pin
        // a real local interface so their Copy URL preview is meaningful.
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while app.engine().snapshot().network.interfaces.is_empty() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let interface = app
            .engine()
            .snapshot()
            .network
            .interfaces
            .iter()
            .find(|i| i.ip.is_ipv4() && !i.ip.is_loopback())
            .map(|i| i.name.clone());
        app.config.write().unwrap().advertise = interface;
        app.logger.log(Event::new(LogLevel::Info, "FTP", "copy command")
            .ip("192.168.22.12".parse().unwrap())
            .device("access-02".into(), Some("C9200L-48P-4X".into()))
            .command("copy ftp://cisco:cisco123@10.10.100.23:2121/cat9k_lite_iosxe.17.15.06.SPA.bin flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into()));
        let dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dist/gui-review");
        std::fs::create_dir_all(&dir).unwrap();
        let mut h = Harness::builder()
            .with_size(egui::vec2(1440.0, 900.0))
            .wgpu()
            .build_eframe(move |cc| {
                let (tx, rx) = mpsc::channel();
                let mut d = Desktop::new(
                    &cc.egui_ctx,
                    app.runtime.clone(),
                    Some(app.config.read().unwrap().config_dir.join("config.toml")),
                    None,
                    tx,
                    rx,
                );
                d.snapshot = app.engine().snapshot();
                d.app = Some(app);
                d.device = Some(1);
                d.image = Some("cat9k_lite_iosxe.17.15.06.SPA.bin".into());
                d.remote_loaded = Some((1, "flash:".into()));
                d.status = "Ready · SSH connections shared across all views".into();
                d
            });
        for (tag, size, compact, dark) in [
            ("desktop", [1440.0, 900.0], false, true),
            ("small", [900.0, 600.0], false, true),
            ("compact", [1280.0, 800.0], true, true),
            ("light", [1280.0, 800.0], false, false),
            ("zoom", [1440.0, 900.0], false, true),
            ("small-zoom", [900.0, 600.0], false, true),
            ("macbook-default", [1280.0, 800.0], true, true),
        ] {
            let scale = if tag.contains("zoom") {
                1.5
            } else if tag == "macbook-default" {
                0.85
            } else {
                1.0
            };
            h.state_mut().prefs.scale = scale;
            h.state_mut().prefs.compact = compact;
            h.state_mut().prefs.dark = Some(dark);
            // egui rescales its previous viewport when a new zoom takes effect.
            settle_steps(&mut h, 2);
            h.set_size(egui::vec2(size[0] / scale, size[1] / scale));
            for tab in Tab::ALL {
                h.state_mut().tab = tab;
                settle_steps(&mut h, 3);
                h.render()
                    .unwrap()
                    .save(dir.join(format!("{tag}-{}.png", tab.name().to_lowercase())))
                    .unwrap();
            }
        }
        h.state_mut().tab = Tab::Upgrade;
        h.state_mut().upgrade_images_open = false;
        settle_steps(&mut h, 3);
        h.render()
            .unwrap()
            .save(dir.join("small-zoom-upgrade-jobs.png"))
            .unwrap();
        h.state_mut().prefs.scale = 1.0;
        h.set_size(egui::vec2(1440.0, 900.0));
        h.state_mut().prefs.dark = Some(true);
        h.state_mut().tab = Tab::Connect;
        h.state_mut().console = Some(1);
        settle_steps(&mut h, 3);
        h.render().unwrap().save(dir.join("console.png")).unwrap();
        h.state_mut().console_windows.clear();
        let app = h.state().app.clone().unwrap();
        let first = app.engine().device(1).unwrap();
        let second = app.engine().device(2).unwrap();
        let _first_input = first.test_cli_receiver();
        let _second_input = second.test_cli_receiver();
        first.test_cli_output(
            b"access-01# show version\r\nCisco IOS XE Software, Version 17.15.03\r\naccess-01# ",
        );
        second.test_cli_output(
            b"access-02# show clock\r\n12:45:16 CET Sat Oct 3 2026\r\naccess-02# ",
        );
        h.state_mut()
            .console_windows
            .insert(1, console::ConsoleWindow::new(true));
        h.state_mut()
            .console_windows
            .insert(2, console::ConsoleWindow::new(true));
        h.state_mut().restore_console(2);
        for (tag, size, scale, dark) in [
            ("cli-desktop", [1440.0, 900.0], 0.85, true),
            ("cli-light", [1280.0, 800.0], 0.85, false),
            ("cli-small-zoom", [900.0, 600.0], 1.5, true),
        ] {
            h.state_mut().prefs.scale = scale;
            h.state_mut().prefs.compact = true;
            h.state_mut().prefs.dark = Some(dark);
            settle_steps(&mut h, 2);
            h.set_size(egui::vec2(size[0] / scale, size[1] / scale));
            settle_steps(&mut h, 3);
            h.render()
                .unwrap()
                .save(dir.join(format!("{tag}.png")))
                .unwrap();
        }
        first.close_cli();
        second.close_cli();
        h.state_mut().cli = None;
        h.state_mut().console_windows.clear();
        h.state_mut().prefs.scale = 1.0;
        h.state_mut().prefs.compact = false;
        h.state_mut().prefs.dark = Some(true);
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(1440.0, 900.0));
        h.state_mut().console = None;
        h.state_mut().connect = Some(ConnectionForm {
            bulk: true,
            port: "22".into(),
            ..Default::default()
        });
        settle_steps(&mut h, 3);
        h.render()
            .unwrap()
            .save(dir.join("bulk-dialog.png"))
            .unwrap();
        h.state_mut().connect = None;
        h.state_mut().tab = Tab::Transfer;
        h.state_mut().jobs_open = true;
        h.state_mut().prefs.devices_collapsed = true;
        for (id, state, bytes) in [(4001, engine::OperationState::Running, 600_000_000), (4002, engine::OperationState::Paused, 0), (4003, engine::OperationState::Failed("FTP connection lost. Reconnect the device and inspect the destination before retrying.".into()), 40_000_000)] {
            h.state_mut().snapshot.operations.push(engine::Operation { id, device: Some(if id == 4002 { 5 } else { 1 }), label: "copy file".into(), state,
                transfer: Some(engine::TransferDetails { inspection: None, command: Some("copy http://10.10.100.23:8080/images/cat9k_lite_iosxe.17.15.06.SPA.bin flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into()), local: "images/cat9k_lite_iosxe.17.15.06.SPA.bin".into(), remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(), receive: false, protocol: Protocol::Http, size: 1_000_000_000, bytes, speed: 12_000_000.0, eta: Some(Duration::from_secs(34)), cancellable: true }) });
        }
        h.state_mut().job_selected = Some(4003);
        for (tag, size, dark) in [
            ("jobs", [1440.0, 900.0], true),
            ("jobs-small", [900.0, 600.0], true),
            ("jobs-light", [1280.0, 800.0], false),
            ("jobs-small-zoom", [900.0, 600.0], true),
        ] {
            let scale = if tag.contains("zoom") {
                1.5
            } else if tag == "macbook-default" {
                0.85
            } else {
                1.0
            };
            h.state_mut().prefs.scale = scale;
            h.state_mut().prefs.dark = Some(dark);
            settle_steps(&mut h, 2);
            h.set_size(egui::vec2(size[0] / scale, size[1] / scale));
            settle_steps(&mut h, 3);
            h.render()
                .unwrap()
                .save(dir.join(format!("{tag}.png")))
                .unwrap();
        }
        h.state_mut().prefs.scale = 0.85;
        h.state_mut().prefs.compact = true;
        h.state_mut().prefs.dark = Some(true);
        h.state_mut().jobs_open = false;
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(1280.0 / 0.85, 800.0 / 0.85));
        h.state_mut().info_open = true;
        settle_steps(&mut h, 3);
        h.render().unwrap().save(dir.join("info.png")).unwrap();
        h.state_mut().info_open = false;
        h.state_mut().help_open = true;
        settle_steps(&mut h, 3);
        h.render().unwrap().save(dir.join("help.png")).unwrap();
        h.state_mut().help_open = false;

        for (tag, size, scale, dark) in [
            ("header-dark", [1280.0, 800.0], 0.85, true),
            ("header-light", [1280.0, 800.0], 0.85, false),
            ("header-small-zoom", [900.0, 600.0], 1.5, true),
        ] {
            h.state_mut().prefs.scale = scale;
            h.state_mut().prefs.dark = Some(dark);
            settle_steps(&mut h, 2);
            h.set_size(egui::vec2(size[0] / scale, size[1] / scale));
            settle_steps(&mut h, 2);
            let label = format!("{} ▾", h.state().link_summary().label);
            scaled_click(&mut h, &label);
            settle_steps(&mut h, 2);
            h.get_by_label("Copy URL interface");
            h.render()
                .unwrap()
                .save(dir.join(format!("{tag}-interfaces.png")))
                .unwrap();
            egui::Popup::close_all(&h.ctx);
            h.state_mut().settings = true;
            settle_steps(&mut h, 2);
            h.render()
                .unwrap()
                .save(dir.join(format!("{tag}-settings.png")))
                .unwrap();
            h.state_mut().settings = false;
            h.state_mut().info_open = true;
            settle_steps(&mut h, 2);
            h.render()
                .unwrap()
                .save(dir.join(format!("{tag}-info.png")))
                .unwrap();
            h.state_mut().info_open = false;
        }
        h.state_mut().prefs.scale = 0.85;
        h.state_mut().prefs.dark = Some(true);
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(1280.0 / 0.85, 800.0 / 0.85));
        h.state_mut().open_retry_interface(4003);
        settle_steps(&mut h, 3);
        h.render()
            .unwrap()
            .save(dir.join("retry-interface.png"))
            .unwrap();
        h.state_mut().retry_interface = None;

        h.state_mut().console_windows.clear();
        h.state_mut().cli = None;
        h.state_mut().console = None;
        h.state_mut().jobs_open = false;
        h.state_mut().connect = None;
        h.state_mut().confirmation = None;
        h.state_mut().tab = Tab::Dashboard;
        h.state_mut().open_wizard();
        h.state_mut().wizard.spec.goal = engine::WorkflowGoal::Upgrade;
        h.state_mut()
            .wizard
            .files
            .insert("cat9k_lite_iosxe.17.15.06.SPA.bin".into());
        h.state_mut().wizard.devices = [1, 2, 3].into();
        for (tag, size, scale, dark) in [
            ("wizard-small-dark", [900.0, 600.0], 1.0, true),
            ("wizard-small-light", [900.0, 600.0], 1.0, false),
            ("wizard-macbook", [1280.0, 800.0], 0.85, true),
            ("wizard-small-zoom", [900.0, 600.0], 1.5, true),
        ] {
            h.state_mut().prefs.scale = scale;
            h.state_mut().prefs.dark = Some(dark);
            h.state_mut().prefs.compact = true;
            settle_steps(&mut h, 2);
            h.set_size(egui::vec2(size[0] / scale, size[1] / scale));
            for step in [0, 1, 2, 3, 4, 5, 6] {
                h.state_mut().wizard.step = step;
                settle_steps(&mut h, 3);
                h.render()
                    .unwrap()
                    .save(dir.join(format!("{tag}-step-{step}.png")))
                    .unwrap();
            }
        }
        std::fs::copy(
            dir.join("desktop-connect.png"),
            dir.parent().unwrap().join("desktop-preview.png"),
        )
        .unwrap();
    }
    #[test]
    fn workflow_shortcut_opens_assistant_and_escape_keeps_the_draft() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 1);
        let mut h = harness(app);
        settle_steps(&mut h, 2);
        h.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::N,
        );
        settle_steps(&mut h, 2);
        assert!(h.state().wizard.open);
        h.get_by_label("What would you like to do?");
        h.get_by_label("Transfer files").click();
        settle_steps(&mut h, 1);
        h.get_by_label("Next").click();
        settle_steps(&mut h, 1);
        assert_eq!(h.state().wizard.step, 1);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(!h.state().wizard.open);
        assert_eq!(h.state().wizard.step, 1);
        h.key_press_modifiers(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::N,
        );
        settle_steps(&mut h, 1);
        assert!(h.state().wizard.open);
        assert_eq!(h.state().wizard.step, 1);
    }
    #[test]
    fn workflow_requires_current_preflight_and_uses_core_jobs_after_minimizing() {
        let (_dir, _runtime, app) = fixture();
        let (sw, mut jobs) = device(&app);
        {
            let mut cfg = app.config.write().unwrap();
            cfg.advertise = Some("127.0.0.1".into());
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            cfg.http.port = listener.local_addr().unwrap().port();
        }
        let mut h = harness(app.clone());
        h.state_mut().device = Some(sw.id);
        h.state_mut().open_wizard();
        h.state_mut().wizard.files.insert("file.txt".into());
        h.state_mut().wizard.spec.protocol = Protocol::Http;
        h.state_mut().wizard.step = 3;
        settle_steps(&mut h, 3);
        assert!(h
            .get_by_label("Start selected transfers")
            .accesskit_node()
            .is_disabled());
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while h.state().snapshot.network.revision == 0 {
            settle_steps(&mut h, 1);
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        h.get_by_label("Check prerequisites").click();
        let until = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            settle_steps(&mut h, 1);
            if !h.state().wizard.checking {
                break;
            }
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            h.state().wizard.checks.len(),
            1,
            "status={}",
            h.state().status
        );
        assert!(h.state().wizard.checks.iter().all(|c| c.blocker.is_none()));
        assert!(!h
            .get_by_label("Start selected transfers")
            .accesskit_node()
            .is_disabled());
        h.get_by_label("Start selected transfers").click();
        settle_steps(&mut h, 3);
        assert_eq!(
            h.state().wizard.step,
            4,
            "status={}, checks={:?}, revision={}/{}",
            h.state().status,
            h.state().wizard.checks,
            h.state().wizard.check_revision,
            h.state().snapshot.network.revision
        );
        assert!(h.state().wizard.workflow.is_some());
        assert!(h.get_by_label("Back").accesskit_node().is_disabled());
        h.get_by_label("Continue in background").click();
        settle_steps(&mut h, 1);
        assert!(!h.state().wizard.open);
        let until = std::time::Instant::now() + Duration::from_secs(3);
        let job = loop {
            if let Ok(job) = jobs.try_recv() {
                break job;
            }
            assert!(std::time::Instant::now() < until);
            settle_steps(&mut h, 1);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert!(
            matches!(job.untracked(), transferbuddy_core::switch::Job::Copy { command, .. } if command.ends_with("flash:file.txt"))
        );
        sw.complete_job_for_test();
        app.shutdown();
    }
    #[test]
    fn workflow_profile_preserves_per_device_sources_and_destinations() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 2);
        let spec = engine::WorkflowSpec {
            protocol: Protocol::Http,
            targets: vec![
                engine::WorkflowTarget {
                    device: 1,
                    local: "file.txt".into(),
                    remote: "flash:one/original.cfg".into(),
                },
                engine::WorkflowTarget {
                    device: 2,
                    local: "sub/other.cfg".into(),
                    remote: "bootflash:two/renamed.cfg".into(),
                },
            ],
            ..Default::default()
        };
        let id = app
            .engine()
            .submit(Command::SaveWorkflow {
                previous: None,
                spec: spec.clone(),
            })
            .unwrap();
        app.engine()
            .submit(Command::SaveProfile {
                name: "Exact mapping".into(),
                workflow: id,
            })
            .unwrap();
        let mut h = harness(app);
        h.state_mut().open_wizard();
        settle_steps(&mut h, 2);
        h.get_by_label("Exact mapping").click();
        settle_steps(&mut h, 3);
        assert_eq!(h.state().wizard.spec.targets, spec.targets);
        assert_eq!(h.state().wizard.spec.protocol, Protocol::Http);
    }
    #[test]
    fn workflow_minimum_window_keeps_navigation_visible() {
        let (_dir, _runtime, app) = fixture();
        let _jobs = preview_devices(&app, 12);
        let mut h = harness(app);
        h.set_size(egui::vec2(900.0, 600.0));
        h.state_mut().open_wizard();
        h.state_mut().wizard.step = 1;
        settle_steps(&mut h, 3);
        for name in ["Next", "Continue in background", "Choose devices"] {
            let rect = h.get_by_label(name).rect();
            assert!(
                rect.left() >= 0.0 && rect.right() <= 900.0 && rect.bottom() <= 600.0,
                "{name}: {rect:?}"
            );
        }
    }
    #[test]
    fn escaping_login_edit_resets_identity_before_add_bulk_and_profile_connections() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut().credentials_device = Some(sw.id);
        h.state_mut().connect = Some(ConnectionForm {
            targets: sw.host.clone(),
            port: "22".into(),
            user: "admin".into(),
            ..Default::default()
        });
        settle_steps(&mut h, 2);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(h.state().credentials_device.is_none());
        h.state_mut().tab = Tab::Connect;
        settle_steps(&mut h, 2);
        h.get_by_label("Add device").click();
        settle_steps(&mut h, 1);
        assert!(h.state().credentials_device.is_none());
        assert!(h.state().connect.as_ref().unwrap().targets.is_empty());
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        h.get_by_label("Bulk add / subnet").click();
        settle_steps(&mut h, 1);
        assert!(h.state().credentials_device.is_none());
        assert!(h.state().connect.as_ref().unwrap().bulk);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        h.state_mut().open_wizard();
        h.state_mut().wizard.step = 1;
        h.state_mut().wizard.profile = Some(engine::WorkProfile {
            name: "Missing host".into(),
            targets: vec![engine::ProfileTarget {
                host: "192.0.2.123".into(),
                port: 22,
                local: "file.txt".into(),
                remote: "flash:file.txt".into(),
            }],
            ..Default::default()
        });
        settle_steps(&mut h, 2);
        h.get_by_label("Connect missing profile devices…").click();
        settle_steps(&mut h, 1);
        assert!(h.state().credentials_device.is_none());
        assert_eq!(h.state().connect.as_ref().unwrap().targets, "192.0.2.123");
    }
    #[test]
    fn stale_workflow_save_ack_cannot_attach_to_or_start_a_new_draft() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app.clone());
        h.state_mut().open_wizard();
        let old = h.state().wizard.generation;
        settle_steps(&mut h, 2);
        h.get_by_label("New workflow").click();
        settle_steps(&mut h, 1);
        assert_ne!(h.state().wizard.generation, old);
        h.state_mut().wizard.start_after_save = true;
        h.state_mut().submitted(submission::Outcome {
            result: Ok(999),
            context: submission::Context::Workflow {
                generation: old,
                start: true,
            },
            quiet: false,
            queued: false,
        });
        assert!(h.state().wizard.workflow.is_none());
        assert!(h.state().wizard.start_after_save);
        assert!(app.engine().snapshot().operations.is_empty());
    }
    #[test]
    fn wizard_blocks_background_transfer_shortcut_and_shows_copy_results() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app.clone());
        h.state_mut().device = Some(sw.id);
        h.state_mut().tab = Tab::Transfer;
        h.state_mut().local_selection = Some(FileEntry {
            name: "file.txt".into(),
            is_dir: false,
            size: 3,
            modified: None,
            ext: "txt".into(),
        });
        h.state_mut().open_wizard();
        h.state_mut().wizard.step = 2;
        settle_steps(&mut h, 2);
        h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
        settle_steps(&mut h, 2);
        assert!(app
            .engine()
            .snapshot()
            .operations
            .iter()
            .all(|o| o.transfer.is_none()));
        assert!(h.state().pending_transfers.is_none());
        h.state_mut().wizard.step = 5;
        settle_steps(&mut h, 2);
        h.get_by_label("Transfer results");
        h.get_by_label("No jobs started. Complete the prerequisite check first.");
        assert!(h.query_by_label("not deployed").is_none());
    }
    #[test]
    fn pending_workflow_start_locks_edits_until_ack_and_restores_check_after_failure() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app);
        h.state_mut().open_wizard();
        h.state_mut().wizard.start_after_save = true;
        settle_steps(&mut h, 2);
        h.get_by_label("Starting workflow…");
        h.get_by_label("Upgrade firmware").click();
        settle_steps(&mut h, 1);
        assert_eq!(h.state().wizard.spec.goal, engine::WorkflowGoal::Transfer);
        h.get_by_label("2. Devices").click();
        settle_steps(&mut h, 1);
        assert_eq!(h.state().wizard.step, 0);
        let generation = h.state().wizard.generation;
        h.state_mut().wizard.step = 4;
        h.state_mut().submitted(submission::Outcome {
            result: Err("Network changed; check again".into()),
            context: submission::Context::WorkflowStart { generation },
            quiet: false,
            queued: false,
        });
        assert!(!h.state().wizard.start_after_save);
        assert_eq!(h.state().wizard.step, 3);
    }
    #[test]
    fn global_interface_selector_is_available_in_all_views_and_blocks_background_shortcuts() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app.clone());
        for tab in Tab::ALL {
            h.state_mut().tab = tab;
            settle_steps(&mut h, 2);
            let selector = h.get_by_label_contains("Links:");
            selector.click();
            settle_steps(&mut h, 2);
            h.get_by_label("Copy URL interface");
            assert!(h
                .get_all_by_label("Automatic · per device")
                .next()
                .is_some());
            h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
            settle_steps(&mut h, 1);
            assert!(h.state().pending_transfers.is_none());
            h.key_press(egui::Key::Escape);
            settle_steps(&mut h, 1);
            assert!(!egui::Popup::is_any_open(&h.ctx));
        }
        assert!(h.query_by_label("COMPACT").is_none());
        assert!(h.query_by_label("STANDARD").is_none());
        h.get_by_label("Light").click();
        settle_steps(&mut h, 2);
        assert_eq!(h.state().prefs.dark, Some(false));
        h.get_by_label("Dark").click();
        settle_steps(&mut h, 2);
        assert_eq!(h.state().prefs.dark, Some(true));
    }
    #[test]
    fn link_summary_shows_current_route_multiple_binds_and_missing_interface() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        settle_steps(&mut h, 1);
        h.state_mut().device = Some(sw.id);
        h.state_mut().snapshot.network.interfaces = vec![
            transferbuddy_core::netif::NetInterface {
                name: "en0".into(),
                ip: "10.0.0.2".parse().unwrap(),
                kind: transferbuddy_core::netif::IfKind::Physical,
            },
            transferbuddy_core::netif::NetInterface {
                name: "utun4".into(),
                ip: "192.0.2.2".parse().unwrap(),
                kind: transferbuddy_core::netif::IfKind::Tunnel,
            },
        ];
        h.state_mut()
            .snapshot
            .network
            .routes
            .insert(sw.id, Some("192.0.2.2".parse().unwrap()));
        assert!(h.state().link_summary().label.contains("utun4 · 192.0.2.2"));
        h.state_mut()
            .snapshot
            .services
            .iter_mut()
            .find(|s| s.id == ServiceId::Http)
            .unwrap()
            .settings
            .bind = "10.0.0.2".into();
        assert!(h.state().link_summary().label.contains("Multiple IPs"));
        h.state_mut().snapshot.advertise = Some("gone".into());
        assert!(h.state().link_summary().warning);
        h.state_mut()
            .snapshot
            .services
            .iter_mut()
            .find(|s| s.id == ServiceId::Http)
            .unwrap()
            .settings
            .bind = "0.0.0.0".into();
        assert_eq!(h.state().link_summary().label, "Links: Unavailable");
    }
    #[test]
    fn connect_actions_share_one_row_and_settings_are_right_aligned_with_host_key_option() {
        let (_dir, _runtime, app) = fixture();
        let (_sw, _jobs) = device(&app);
        let mut h = harness(app.clone());
        h.state_mut().tab = Tab::Connect;
        settle_steps(&mut h, 2);
        let cli = h.get_by_label("CLI").rect();
        let actions = h
            .get_all_by_label("Actions")
            .map(|node| node.rect())
            .find(|rect| (rect.center().y - cli.center().y).abs() < 1.0)
            .unwrap();
        let remove = h.get_by_label("Remove").rect();
        assert!((cli.center().y - actions.center().y).abs() < 1.0);
        assert!((cli.center().y - remove.center().y).abs() < 1.0);
        h.get_by_label("Settings").click();
        settle_steps(&mut h, 2);
        let window = h
            .get_all_by_label("Settings")
            .map(|node| node.rect())
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .unwrap();
        assert!(window.width() > 700.0);
        assert!((window.right() - 1264.0).abs() < 5.0);
        h.get_by_label("Compact density");
        h.get_by_label("Automatically accept device host keys")
            .click();
        settle_steps(&mut h, 2);
        assert!(!app.config.read().unwrap().auto_accept_host_keys);
        h.key_press(egui::Key::Escape);
        settle_steps(&mut h, 1);
        assert!(!h.state().settings);
    }
    #[test]
    fn quick_selector_applies_manual_address_and_copies_effective_ip() {
        let (_dir, _runtime, app) = fixture();
        let mut h = harness(app.clone());
        let until = std::time::Instant::now() + Duration::from_secs(3);
        while h.state().snapshot.network.interfaces.is_empty() {
            assert!(std::time::Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
            settle_steps(&mut h, 1);
        }
        h.get_by_label_contains("Links:").click();
        settle_steps(&mut h, 1);
        h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Interface or IP")
            .focus();
        settle_steps(&mut h, 1);
        h.input_mut()
            .events
            .push(egui::Event::Paste("127.0.0.1".into()));
        settle_steps(&mut h, 1);
        assert_eq!(h.state().interface, "127.0.0.1");
        h.get_by_label("Apply address").click();
        settle_steps(&mut h, 2);
        assert_eq!(
            app.config.read().unwrap().advertise.as_deref(),
            Some("127.0.0.1")
        );
        h.get_by_label_contains("Links:").click();
        settle_steps(&mut h, 1);
        h.get_by_label_contains("Links:").click();
        settle_steps(&mut h, 1);
        h.get_by_label("Copy IP · sftp").click();
        settle_steps(&mut h, 1);
        assert!(h
            .output()
            .platform_output
            .commands
            .iter()
            .any(|c| matches!(c, egui::OutputCommand::CopyText(text) if text == "127.0.0.1")));
    }
    #[test]
    fn zoomed_header_keeps_devices_and_interface_popup_visible() {
        let (_dir, _runtime, app) = fixture();
        let (sw, _jobs) = device(&app);
        let mut h = harness(app);
        h.state_mut().prefs.scale = 1.5;
        h.state_mut().prefs.compact = true;
        h.state_mut().tab = Tab::Connect;
        settle_steps(&mut h, 2);
        h.set_size(egui::vec2(600.0, 400.0));
        settle_steps(&mut h, 3);
        let cli = h.get_by_label("CLI").rect();
        assert!(
            cli.top() > 0.0 && cli.bottom() < 400.0 * h.ctx.pixels_per_point(),
            "CLI must remain in the visible device list: {cli:?}"
        );
        scaled_click(&mut h, "CLI");
        settle_steps(&mut h, 2);
        assert_eq!(
            h.state().cli,
            Some(sw.id),
            "The visible CLI button must be clickable"
        );
        h.state_mut().console_windows.clear();
        h.state_mut().cli = None;
        h.state_mut().cli_focus = false;
        sw.close_cli();
        h.state_mut().connection_selected.insert(sw.id);
        settle_steps(&mut h, 2);
        let cli = h.get_by_label("CLI").rect();
        assert!(
            cli.bottom() < 400.0 * h.ctx.pixels_per_point(),
            "Selection controls must leave room for devices: {cli:?}"
        );
        let label = format!("{} ▾", h.state().link_summary().label);
        scaled_click(&mut h, &label);
        settle_steps(&mut h, 3);
        let heading = h.get_by_label("Copy URL interface").rect();
        assert!(
            heading.top() > 0.0 && heading.bottom() < 400.0 * h.ctx.pixels_per_point(),
            "Selector heading must be visible: {heading:?}"
        );
        let choice = h
            .get_all_by_label("Automatic · per device")
            .next()
            .unwrap()
            .rect();
        assert!(choice.bottom() < 400.0 * h.ctx.pixels_per_point());
    }
}
