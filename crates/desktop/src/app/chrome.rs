use super::*;
use crate::design;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Intent {
    View(Tab),
    Add(bool),
    Workflow,
    Refresh,
    Cli,
    Protocol,
    Hash,
    Deploy,
    Verify,
    Install,
    Cleanup,
    Folder,
    Settings,
    Jobs,
    Copy,
    Receive,
    CancelJob,
    RetryJob,
}
impl Intent {
    pub(crate) fn groups() -> [(&'static str, Vec<Self>); 4] {
        [
            (
                "File",
                vec![
                    Self::Workflow,
                    Self::Add(false),
                    Self::Add(true),
                    Self::Folder,
                    Self::Settings,
                ],
            ),
            (
                "View",
                Tab::ALL
                    .into_iter()
                    .map(Self::View)
                    .chain([Self::Jobs])
                    .collect(),
            ),
            ("Device", vec![Self::Refresh, Self::Cli, Self::Protocol]),
            (
                "Transfer",
                vec![
                    Self::Copy,
                    Self::Receive,
                    Self::CancelJob,
                    Self::RetryJob,
                    Self::Deploy,
                    Self::Verify,
                    Self::Install,
                    Self::Cleanup,
                ],
            ),
        ]
    }
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::View(t) => t.title(),
            Self::Workflow => "New workflow…",
            Self::Add(false) => "Add device",
            Self::Add(true) => "Bulk add / subnet",
            Self::Refresh => "Refresh / reconnect",
            Self::Cli => "Connect to CLI",
            Self::Protocol => "Choose protocol",
            Self::Hash => "File hashes",
            Self::Deploy => "Upload & verify",
            Self::Verify => "Verify existing image",
            Self::Install => "Upgrade…",
            Self::Cleanup => "Remove inactive",
            Self::Folder => "Choose folder…",
            Self::Settings => "Settings",
            Self::Jobs => "Show jobs",
            Self::Copy => "Copy to device",
            Self::Receive => "Copy to local",
            Self::CancelJob => "Cancel selected job",
            Self::RetryJob => "Retry selected job",
        }
    }
}
impl Desktop {
    pub(super) fn intent(&mut self, intent: Intent, ctx: &egui::Context) {
        if let Some(reason) = self.intent_blocker(intent) {
            self.status = reason.into();
            return;
        }
        match intent {
            Intent::Workflow => self.open_wizard(),
            Intent::Jobs => self.jobs_open = !self.jobs_open,
            Intent::Copy => self.transfer(false),
            Intent::Receive => self.transfer(true),
            Intent::CancelJob => {
                if let Some(id) = self.job_selected {
                    self.send(Command::CancelOperation(id));
                }
            }
            Intent::RetryJob => {
                if let Some(id) = self.job_selected {
                    self.send(Command::RetryOperation(id));
                }
            }
            Intent::View(t) => {
                self.tab = t;
                self.cli_focus = false;
            }
            Intent::Add(bulk) => {
                self.credentials_device = None;
                self.connect = Some(ConnectionForm {
                    port: "22".into(),
                    bulk,
                    ..Default::default()
                })
            }
            Intent::Folder => self.pick_root(ctx),
            Intent::Settings => self.settings = true,
            Intent::Hash => {
                if let Some(f) = self.local_selection.clone().filter(|f| !f.is_dir) {
                    let p = self.local_path(&f.name);
                    self.hash_view = Some(p.clone());
                    self.hash_compare.clear();
                    self.send(Command::Hash(p));
                }
            }
            _ => {
                if let Some(d) = self.device {
                    match intent {
                        Intent::Refresh => {
                            let over = self.selected().is_some_and(|d| d.state.is_over());
                            self.send(if over {
                                Command::Reconnect(d)
                            } else {
                                Command::Refresh(d)
                            });
                        }
                        Intent::Cli => self.open_cli(d),
                        Intent::Protocol => self.protocol_picker = Some((d, false, false)),
                        Intent::Deploy => {
                            self.send(Command::Deploy(d, self.upgrade_protocol));
                        }
                        Intent::Verify => {
                            self.send(Command::Verify(d));
                        }
                        Intent::Install => self.request_install(d, false),
                        Intent::Cleanup => {
                            self.send(Command::RemoveInactive(d));
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    pub(crate) fn intent_blocker(&self, intent: Intent) -> Option<&'static str> {
        match intent {
            Intent::View(_)
            | Intent::Jobs
            | Intent::Settings
            | Intent::Folder
            | Intent::Add(_)
            | Intent::Workflow => None,
            Intent::Hash if self.local_selection.as_ref().is_some_and(|f| !f.is_dir) => None,
            Intent::Hash => Some("Select a local file"),
            Intent::Copy
                if !self.selected_file_names(false).is_empty()
                    && self.selected().is_some_and(|d| !d.state.is_over()) =>
            {
                None
            }
            Intent::Receive
                if !self.selected_file_names(true).is_empty()
                    && self.selected().is_some_and(|d| !d.state.is_over()) =>
            {
                None
            }
            Intent::Copy | Intent::Receive => Some("Select files and a connected device"),
            Intent::CancelJob
                if self.snapshot.operations.iter().any(|o| {
                    Some(o.id) == self.job_selected
                        && o.state.is_active()
                        && o.transfer.as_ref().is_some_and(|t| t.cancellable)
                }) =>
            {
                None
            }
            Intent::RetryJob
                if self.snapshot.operations.iter().any(|o| {
                    Some(o.id) == self.job_selected
                        && matches!(
                            o.state,
                            engine::OperationState::Cancelled
                                | engine::OperationState::Failed(_)
                                | engine::OperationState::Uncertain(_)
                        )
                }) =>
            {
                None
            }
            Intent::CancelJob | Intent::RetryJob => Some("Select an eligible job"),
            _ => {
                let Some(d) = self
                    .snapshot
                    .devices
                    .iter()
                    .find(|d| Some(d.id) == self.device)
                else {
                    return Some("Select a device");
                };
                match intent {
                    Intent::Refresh | Intent::Protocol => None,
                    Intent::Cli if self.console_windows.get(&d.id).is_some_and(|w| w.cli) => None,
                    Intent::Deploy | Intent::Verify if d.assigned.is_none() => {
                        Some("Assign an image first")
                    }
                    Intent::Install
                        if upgrade::install_blocker(
                            &d.upgrade,
                            &d.facts.version.clone().unwrap_or_default(),
                        )
                        .is_some() =>
                    {
                        Some("Verify a different IOS version before installing")
                    }
                    _ if d.state != SwitchState::Ready
                        || self.app.as_ref().is_some_and(|a| a.engine().busy(d.id)) =>
                    {
                        Some("Device is busy or disconnected")
                    }
                    _ => None,
                }
            }
        }
    }
    pub(super) fn obstructed(&self) -> bool {
        self.confirmation.is_some()
            || self.connect.is_some()
            || self.service.is_some()
            || self.hash_view.is_some()
            || self.file_details.is_some()
            || self.commands_view.is_some()
            || self.settings
            || self.info_open
            || self.help_open
            || self.retry_interface.is_some()
            || self.quit_question
            || self.protocol_picker.is_some()
            || self.palette_open
    }
    fn header_tools(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if ui.available_width() < 750.0 {
            let more = ui.menu_button("More", |ui| {
                if ui.button("Info").clicked() {
                    self.info_open = true;
                    ui.close();
                }
                if ui.button("Help").clicked() {
                    self.help_open = true;
                    ui.close();
                }
                if ui.button("Jobs").clicked() {
                    self.jobs_open = !self.jobs_open;
                    ui.close();
                }
                if ui.button("Search commands").clicked() {
                    self.palette_open = true;
                    ui.close();
                }
            });
            self.header_tools_bottom = more.response.rect.bottom();
            let settings = ui.button("Settings");
            if settings.clicked() {
                self.intent(Intent::Settings, ctx);
            }
            self.interface_quick_selector(ui);
            self.header_theme_switch(ui, ctx);
            return;
        }
        let info = ui.button("Info");
        self.header_tools_bottom = info.rect.bottom();
        if info.clicked() {
            self.info_open = true;
        }
        let help = ui.button("Help");
        self.header_tools_bottom = self.header_tools_bottom.max(help.rect.bottom());
        if help.clicked() {
            self.help_open = true;
        }
        let settings = ui.button("Settings");
        self.header_tools_bottom = self.header_tools_bottom.max(settings.rect.bottom());
        if settings.clicked() {
            self.intent(Intent::Settings, ctx);
        }
        self.interface_quick_selector(ui);
        let jobs = self
            .snapshot
            .operations
            .iter()
            .filter(|o| o.transfer.is_some() && o.state.is_active())
            .count();
        if ui
            .button(format!("Jobs ({jobs})"))
            .on_hover_text("Show transfer queue · Cmd/Ctrl+J")
            .clicked()
        {
            self.jobs_open = !self.jobs_open;
        }
        if ui.button("⌘ K").on_hover_text("Search commands").clicked() {
            self.palette_open = true;
        }
        self.header_theme_switch(ui, ctx);
    }
    fn header_theme_switch(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let dark = ctx.theme() == egui::Theme::Dark;
        if ui
            .button(if dark { "Light" } else { "Dark" })
            .on_hover_text("Switch between dark and light appearance")
            .clicked()
        {
            self.prefs.dark = Some(!dark);
            ctx.set_theme(if dark {
                egui::Theme::Light
            } else {
                egui::Theme::Dark
            });
        }
    }

    pub(super) fn render(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let style_key = (
            self.prefs.dark,
            self.prefs.compact,
            self.prefs.scale.to_bits(),
            self.prefs.reduce_motion,
        );
        if self.style_key != Some(style_key) {
            design::apply(&ctx, &self.prefs);
            self.style_key = Some(style_key);
        }
        self.edit_shortcuts(&ctx);
        self.shortcuts(&ctx);
        self.list_widgets.clear();
        #[cfg(not(target_os = "macos"))]
        egui::Panel::top("application_menu").show(ui, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                for (title, intents) in Intent::groups() {
                    ui.menu_button(title, |ui| {
                        for intent in intents {
                            let blocker = if self.obstructed() {
                                Some("Close the current dialog first")
                            } else {
                                self.intent_blocker(intent)
                            };
                            if ui
                                .add_enabled(blocker.is_none(), egui::Button::new(intent.title()))
                                .on_disabled_hover_text(blocker.unwrap_or(""))
                                .clicked()
                            {
                                ui.close();
                                self.intent(intent, &ctx);
                            }
                        }
                    });
                }
            });
        });
        egui::Panel::top("console_header")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| {
                if self.obstructed() {
                    ui.disable();
                }
                let narrow = ui.available_width() < 700.0;
                let separate_tools = ui.available_width() < 1350.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("[⇄]").color(design::accent(ui)).size(20.0));
                    ui.label(RichText::new("TRANSFERBUDDY").font(egui::FontId::new(
                        if narrow { 14.0 } else { 17.0 },
                        egui::FontFamily::Name("medium".into()),
                    )));
                    ui.label(
                        RichText::new(format!("v{VERSION}"))
                            .small()
                            .color(design::muted(ui)),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        self.header_status(ui)
                    });
                });
                ui.add_space(10.0);
                if separate_tools {
                    ui.horizontal_wrapped(|ui| {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center).with_main_wrap(true),
                            |ui| self.header_tools(ui, &ctx),
                        );
                    });
                }
                let navigation = |ui: &mut egui::Ui| {
                    for tab in Tab::ALL {
                        let selected = self.tab == tab;
                        let response = ui.add(
                            egui::Button::new(RichText::new(tab.title()).color(if selected {
                                design::accent(ui)
                            } else {
                                design::muted(ui)
                            }))
                            .selected(selected)
                            .min_size(egui::vec2(0.0, 34.0)),
                        );
                        if selected {
                            ui.painter().hline(
                                response.rect.x_range(),
                                response.rect.bottom() + 2.0,
                                egui::Stroke::new(2.0, design::accent(ui)),
                            );
                        }
                        if response.clicked() {
                            self.intent(Intent::View(tab), &ctx);
                        }
                    }
                    if !separate_tools {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            self.header_tools(ui, &ctx)
                        });
                    }
                    if let Some(app) = self.app.as_ref().filter(|_| !narrow) {
                        let root = app.config.read().unwrap().root.clone();
                        if ui.available_width() > 120.0 {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let name = root
                                        .file_name()
                                        .unwrap_or(root.as_os_str())
                                        .to_string_lossy();
                                    design::label(ui, format!("ROOT / {name}"))
                                        .on_hover_text(root.display().to_string());
                                },
                            );
                        }
                    }
                };
                if narrow {
                    egui::ScrollArea::horizontal()
                        .id_salt("workspace_navigation")
                        .show(ui, |ui| {
                            ui.horizontal(navigation);
                        });
                } else {
                    ui.horizontal(navigation);
                }
            });
        egui::Panel::bottom("console_status")
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().faint_bg_color)
                    .inner_margin(egui::Margin::symmetric(16, 6)),
            )
            .show(ui, |ui| {
                if ui.available_width() < 700.0 {
                    ui.add(egui::Label::new(&self.status).truncate())
                        .on_hover_text(&self.status);
                    return;
                }
                ui.horizontal_wrapped(|ui| {
                    if self.wizard.workflow.is_some() && ui.small_button("Show workflow").clicked()
                    {
                        self.wizard.open = true;
                    }
                    if self.cli.is_some() && self.cli_focus {
                        design::key(ui, "ESC", "Minimize CLI");
                        design::key(ui, "Ctrl+C", "Interrupt device");
                    } else {
                        for (key, desc) in self.hints() {
                            design::key(ui, key, desc);
                        }
                    }
                    ui.separator();
                    design::label(ui, &self.status).on_hover_text(&self.status);
                });
            });
        let taskbar = self.console_taskbar(ui);
        let console_workspace = ui.available_rect_before_wrap();
        let jobs_cover_workspace = self.jobs_open && ui.available_height() < 360.0;
        if self.jobs_open && !jobs_cover_workspace {
            let max = (ui.available_height() * 0.4).clamp(100.0, 300.0);
            let panel = egui::Panel::bottom("jobs_dock")
                .resizable(true)
                .default_size(self.prefs.jobs_height.min(max))
                .size_range(100.0..=max)
                .frame(
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .inner_margin(10),
                )
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    self.jobs_panel(ui);
                });
            self.prefs.jobs_height = panel.response.rect.height();
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(16),
            )
            .show(ui, |ui| {
                if self.obstructed() {
                    ui.disable();
                }
                if self.app.is_none() {
                    ui.add_space(60.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("[ ⇄ ]").size(42.0).color(design::accent(ui)));
                        ui.add_space(20.0);
                        ui.heading("Your network. Your console.");
                        ui.label("Choose the local folder for transfers and upgrade images.");
                        ui.label(
                            RichText::new("SSH sessions · File transfers · Cisco INSTALL upgrades")
                                .color(design::muted(ui)),
                        );
                        ui.add_space(20.0);
                        if self.booting {
                            ui.spinner();
                        } else if design::primary(ui, "Choose local folder").clicked() {
                            self.pick_root(&ctx);
                        }
                    });
                } else if jobs_cover_workspace {
                    self.jobs_panel(ui);
                } else {
                    match self.tab {
                        Tab::Dashboard => self.dashboard(ui),
                        Tab::Connect => self.connections(ui),
                        Tab::Transfer => self.transfer_view(ui),
                        Tab::Upgrade => self.upgrades(ui),
                        Tab::Logs => self.logs(ui),
                    }
                }
            });
        self.prefs.columns = ctx
            .data(|data| data.get_temp(egui::Id::new("console_columns")))
            .unwrap_or_default();
        self.interface_popup(&ctx);
        self.console_windows(&ctx, console_workspace, taskbar);
        self.palette(&ctx);
        self.wizard_dialog(&ctx);
        self.dialogs(&ctx);
        self.assistance_dialogs(&ctx);
        if let Some(rect) = ctx.input(|i| i.viewport().inner_rect) {
            self.prefs.size = [rect.width(), rect.height()];
        }
        for drop in ctx.input(|i| i.raw.dropped_files.clone()) {
            if let (Some(app), Some(path)) = (&self.app, drop.path) {
                let root = app.config.read().unwrap().root.clone();
                let tx = self.tx.clone();
                let ctx = ctx.clone();
                self.runtime.spawn_blocking(move || {
                    let result = (|| {
                        let jail = transferbuddy_core::fsroot::SecureRoot::new(&root)
                            .map_err(|e| e.to_string())?;
                        let path = path.canonicalize().map_err(|e| e.to_string())?;
                        let rel = jail
                            .relative(&path)
                            .ok_or("Choose this file's folder as the transfer root first.")?;
                        Ok((rel, path.is_file()))
                    })();
                    let _ = tx.send(UiEvent::Dropped { root, result });
                    ctx.request_repaint();
                });
            }
        }
    }
    fn hints(&self) -> Vec<(&'static str, &'static str)> {
        match self.tab {
            Tab::Transfer if self.focus == 0 => vec![
                ("↑↓", "Device"),
                ("Enter", "Protocol"),
                ("Tab", "Next control"),
            ],
            Tab::Transfer => vec![
                ("Enter", "Open / details"),
                ("⌘/Ctrl+Enter", "Copy"),
                ("⌘/Ctrl+J", "Jobs"),
            ],
            Tab::Connect => vec![
                ("⌘/Ctrl+N", "Connect"),
                ("⌘/Ctrl+R", "Refresh / reconnect"),
                ("⌘/Ctrl+K", "Commands"),
            ],
            _ => vec![
                ("⌘/Ctrl+1–5", "Views"),
                ("⌘/Ctrl+J", "Jobs"),
                ("⌘/Ctrl+K", "Commands"),
            ],
        }
    }
    fn shortcuts(&mut self, ctx: &egui::Context) {
        if self.obstructed() || self.wizard.open || egui::Popup::is_any_open(ctx) {
            return;
        }
        let shortcut = |key| {
            ctx.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, key))
            })
        };
        if shortcut(egui::Key::K) {
            self.palette_open = true;
            self.palette_query.clear();
            self.palette_index = 0;
            self.cli_focus = false;
            return;
        }
        if shortcut(egui::Key::J) {
            self.jobs_open = !self.jobs_open;
            return;
        }
        for (key, tab) in [
            (egui::Key::Num1, Tab::Dashboard),
            (egui::Key::Num2, Tab::Connect),
            (egui::Key::Num3, Tab::Transfer),
            (egui::Key::Num4, Tab::Upgrade),
            (egui::Key::Num5, Tab::Logs),
        ] {
            if shortcut(key) {
                self.intent(Intent::View(tab), ctx);
                return;
            }
        }
        if self.cli.is_some() && self.cli_focus {
            return;
        }
        if ctx.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::N,
            ))
        }) {
            self.intent(Intent::Workflow, ctx);
            return;
        }
        if shortcut(egui::Key::N) {
            self.intent(Intent::Add(false), ctx);
            return;
        }
        if shortcut(egui::Key::R) {
            if self.tab == Tab::Transfer && self.focus == 1 {
                self.refresh_local();
            } else if self.tab == Tab::Transfer && self.focus == 2 {
                self.remote_loaded = None;
            } else {
                self.intent(Intent::Refresh, ctx);
            }
            return;
        }
        if shortcut(egui::Key::Enter) && self.tab == Tab::Transfer {
            self.transfer(self.focus == 2);
            return;
        }
        if ctx.text_edit_focused() {
            return;
        }
        if ctx.memory(|m| {
            m.focused()
                .is_some_and(|id| !self.list_widgets.contains(&id))
        }) {
            return;
        }
        let key = |key| ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key));
        if key(egui::Key::ArrowUp) {
            self.move_selection(-1);
        }
        if key(egui::Key::ArrowDown) {
            self.move_selection(1);
        }
        if self.tab == Tab::Upgrade && self.focus == 1 && key(egui::Key::Enter) {
            if self.local_selection.as_ref().is_some_and(|f| f.is_dir) {
                self.enter_local();
            } else if let Some(file) = &self.local_selection {
                self.image = Some(self.local_path(&file.name));
            }
        }
        if self.tab == Tab::Transfer && key(egui::Key::Enter) {
            match self.focus {
                0 => self.intent(Intent::Protocol, ctx),
                1 => self.enter_local(),
                _ => self.enter_remote(),
            }
        }
        if self.tab == Tab::Transfer && self.focus == 2 && shortcut(egui::Key::Backspace) {
            self.delete_selected();
        }
    }
    fn palette(&mut self, ctx: &egui::Context) {
        if !self.palette_open {
            self.palette_was_open = false;
            return;
        }
        let just_opened = !self.palette_was_open;
        self.palette_was_open = true;
        let mut open = true;
        let mut selected = None;
        egui::Window::new("COMMANDS")
            .anchor(egui::Align2::CENTER_TOP, [0.0, 110.0])
            .resizable(false)
            .collapsible(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(460.0);
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.palette_query)
                        .hint_text("Find an action…")
                        .desired_width(f32::INFINITY),
                );
                if just_opened {
                    response.request_focus();
                }
                let mut intents: Vec<_> = Tab::ALL.into_iter().map(Intent::View).collect();
                intents.extend([
                    Intent::Workflow,
                    Intent::Add(false),
                    Intent::Add(true),
                    Intent::Folder,
                    Intent::Settings,
                ]);
                if self.device.is_some() {
                    intents.extend([
                        Intent::Refresh,
                        Intent::Cli,
                        Intent::Protocol,
                        Intent::Deploy,
                        Intent::Verify,
                        Intent::Install,
                        Intent::Cleanup,
                    ]);
                }
                if self.local_selection.as_ref().is_some_and(|f| !f.is_dir) {
                    intents.push(Intent::Hash);
                }
                let q = self.palette_query.to_lowercase();
                intents.extend([
                    Intent::Jobs,
                    Intent::Copy,
                    Intent::Receive,
                    Intent::CancelJob,
                    Intent::RetryJob,
                ]);
                intents.retain(|a| a.title().to_lowercase().contains(&q));
                self.palette_index = self.palette_index.min(intents.len().saturating_sub(1));
                if ui.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                    self.palette_index =
                        (self.palette_index + 1).min(intents.len().saturating_sub(1));
                }
                if ui.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                    self.palette_index = self.palette_index.saturating_sub(1);
                }
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(330.0)
                    .show(ui, |ui| {
                        for (index, intent) in intents.iter().enumerate() {
                            if ui
                                .add_enabled_ui(self.intent_blocker(*intent).is_none(), |ui| {
                                    ui.add_sized(
                                        [ui.available_width(), 32.0],
                                        egui::Button::new(intent.title())
                                            .selected(index == self.palette_index),
                                    )
                                })
                                .inner
                                .on_disabled_hover_text(self.intent_blocker(*intent).unwrap_or(""))
                                .clicked()
                            {
                                selected = Some(*intent);
                            }
                        }
                    });
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    selected = intents.get(self.palette_index).copied();
                }
                ui.label(
                    RichText::new("↑↓ Navigate   Enter Run   ESC Close")
                        .small()
                        .color(design::muted(ui)),
                );
            });
        if let Some(intent) = selected {
            self.palette_open = false;
            self.intent(intent, ctx);
        }
        if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.palette_open = false;
        }
    }
}
