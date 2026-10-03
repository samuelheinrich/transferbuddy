use super::*;

impl Desktop {
    pub(super) fn header_status(&self, ui: &mut egui::Ui) {
        let short = ui.available_width() < 490.0;
        for (label, abbreviated, value, color) in [
            (
                "LIVE SERVICES",
                "SVC",
                self.snapshot
                    .services
                    .iter()
                    .filter(|s| s.status.is_running())
                    .count(),
                GREEN,
            ),
            ("DEVICES", "DEV", self.snapshot.devices.len(), CYAN),
            (
                "TRANSFERS",
                "TX",
                self.snapshot
                    .transfers
                    .iter()
                    .filter(|t| t.state.is_active() && t.file.is_some())
                    .count(),
                CYAN,
            ),
            (
                "NEEDS INPUT",
                "INPUT",
                self.snapshot.confirmations.len(),
                AMBER,
            ),
        ]
        .into_iter()
        .rev()
        {
            ui.label(
                RichText::new(format!(
                    "{value:02} {}",
                    if short { abbreviated } else { label }
                ))
                .small()
                .color(design::semantic(ui, color)),
            )
            .on_hover_text(label);
        }
    }

    pub(super) fn edit_shortcuts(&self, ctx: &egui::Context) {
        // macOS normally uses Command. Also accept Control in GUI text fields;
        // terminal Control sequences must still reach the switch.
        let terminal_focused = self
            .cli
            .is_some_and(|id| ctx.memory(|m| m.has_focus(egui::Id::new(("terminal_input", id)))));
        if terminal_focused || !ctx.text_edit_focused() {
            return;
        }
        let paste = ctx.input_mut(|input| {
            let mut paste = false;
            for event in &mut input.events {
                if let egui::Event::Key {
                    key,
                    modifiers,
                    pressed: true,
                    ..
                } = event
                {
                    if modifiers.ctrl && !modifiers.command {
                        match key {
                            egui::Key::A => *modifiers = egui::Modifiers::COMMAND,
                            egui::Key::C => *event = egui::Event::Copy,
                            egui::Key::X => *event = egui::Event::Cut,
                            egui::Key::V => {
                                paste = true;
                                *event = egui::Event::Copy;
                            }
                            _ => {}
                        }
                    }
                }
            }
            if paste {
                // Avoid forwarding an artificial Copy while requesting paste.
                input.events.retain(|e| !matches!(e, egui::Event::Copy));
            }
            paste
        });
        if paste {
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste);
        }
    }

    pub(super) fn open_retry_interface(&mut self, id: u64) {
        self.retry_interface = Some(id);
        self.retry_choice = self
            .app
            .as_ref()
            .and_then(|a| a.config.read().unwrap().advertise.clone());
    }

    pub(super) fn assistance_dialogs(&mut self, ctx: &egui::Context) {
        if self.info_open {
            let mut open = true;
            let response = egui::Window::new("About TransferBuddy")
                .open(&mut open)
                .collapsible(false)
                .anchor(
                    egui::Align2::RIGHT_TOP,
                    egui::vec2(-16.0, self.header_tools_bottom + 8.0),
                )
                .default_width(620.0)
                .default_height(260.0)
                .vscroll(true)
                .max_height(
                    (ctx.content_rect().height() - self.header_tools_bottom - 40.0).max(120.0),
                )
                .max_width(ctx.content_rect().width() - 40.0)
                .show(ctx, |ui| {
                    ui.heading(format!("TransferBuddy {VERSION}"));
                    ui.label("Created by Samuel Heinrich");
                    ui.label("File transfers and Cisco upgrades · Rust core, TUI and egui desktop");
                    ui.hyperlink_to(
                        "GitHub · samuelheinrich/transferbuddy",
                        "https://github.com/samuelheinrich/transferbuddy",
                    );
                    ui.hyperlink_to(
                        "Report an issue",
                        "https://github.com/samuelheinrich/transferbuddy/issues",
                    );
                });
            if !open || response.is_some_and(|_| ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
                self.info_open = false;
            }
        }
        if self.help_open {
            let mut open = true;
            egui::Window::new("TransferBuddy Help").open(&mut open).vscroll(true)
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, self.header_tools_bottom + 8.0))
                .default_width(660.0).max_width(ctx.content_rect().width() - 40.0)
                .max_height((ctx.content_rect().height() - self.header_tools_bottom - 40.0).max(120.0)).show(ctx, |ui| {
                    ui.heading("Connect → Transfer → Upgrade");
                    for (title, text) in [
                        ("1 Dashboard · services and interfaces", "Start the services needed for this transfer. Choose Automatic or the network address the switch can reach. This address appears in the copy URL, not in the SSH connection."),
                        ("Guided workflow", "Dashboard → New workflow (Cmd/Ctrl+Shift+N) walks through devices, files, preflight, transfers and results. Close it to continue in the background; reopen to see its jobs. Installs always need a separate confirmation. Profiles contain no SSH passwords."),
                        ("Recovery", "Unexpected network failure reconnects established SSH sessions, but never restarts copies or installers. Review pending jobs, then Resume checked. For unknown copy results, inspect destination size and MD5 first; replacing a different destination requires typed lowercase y. Login rejection stops automatic attempts."),
                        ("2 Connect · manage devices", "Add one device or mixed IP/subnet lists. Credentials stay in memory for reconnect. Search filters the list. Remove disconnects and removes an idle device; running jobs must finish or be cancelled first."),
                        ("3 Transfer · copy files", "Choose a device and protocol, then source files and destination. Copy queues the selection. The job shows the actual copy command and highlighted server IP. Retry with interface lets you correct the address; retries rebuild URLs from current settings."),
                        ("4 Upgrade · old → new", "Assign an image to one or more devices. Orange means a different target release is assigned; red marks a downgrade or error. Upload and verify MD5 before Upgrade. The current release and target are shown together. Every install requires typed y confirmation."),
                        ("CLI windows", "Each device can have a CLI window. Select text and use Cmd/Ctrl+C to copy; paste sends to the focused switch. Ctrl+C interrupts. Escape, Minimize or a background click keeps the CLI connected. Close ends its channel. Taskbar tabs restore windows."),
                        ("Text and keyboard", "Cmd/Ctrl+A selects all in text fields, Cmd/Ctrl+C copies, Cmd/Ctrl+V pastes. On macOS Control+A/C/V also work in GUI fields. Cmd/Ctrl+1–5 changes views, K opens commands, J opens jobs, N adds a device."),
                        ("SSH: network versus negotiation", "No route to host happens before SSH negotiation. Check the route/VPN and app network access. If Terminal reaches the device, check macOS Local Network permission for TransferBuddy. Group14-SHA1, ssh-rsa and older Cisco ciphers are already supported. Test SSH transport checks negotiation without attempting a login."),
                    ] {
                        ui.collapsing(title, |ui| { ui.add(egui::Label::new(text).wrap().selectable(true)); });
                    }
                    #[cfg(target_os = "macos")]
                    if ui.button("Open Local Network settings").clicked() {
                        if let Err(error) = crate::native::open_network_privacy_settings() { self.status = error; }
                    }
                    ui.hyperlink_to("Documentation on GitHub", "https://github.com/samuelheinrich/transferbuddy#readme");
                });
            if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.help_open = false;
            }
        }
        if let Some(id) = self.retry_interface {
            let mut confirm = false;
            let modal = egui::Modal::new(egui::Id::new("retry_interface")).show(ctx, |ui| {
                ui.set_max_width(600.0);
                ui.heading("Retry with interface");
                ui.label("Choose the server address the device can reach. Paths and protocol stay with the original job.");
                egui::ComboBox::from_id_salt("retry_address")
                    .selected_text(self.retry_choice.as_deref().unwrap_or("Automatic"))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.retry_choice, None, "Automatic · route to device");
                        for interface in &self.interfaces {
                            ui.selectable_value(&mut self.retry_choice, Some(interface.name.clone()),
                                format!("{} · {}", interface.name, interface.ip));
                        }
                    });
                if let Some(operation) = self.snapshot.operations.iter().find(|o| o.id == id) {
                    if let Some(command) = operation.transfer.as_ref().and_then(|t| t.command.as_deref()) {
                        ui.label("Previous attempt:"); design::copy_command(ui, command);
                    }
                }
                ui.horizontal(|ui| {
                    confirm = ui.add_enabled(self.retry_saving.is_none(), egui::Button::new("Apply interface & retry")).clicked();
                    if ui.button("Cancel").clicked() { self.retry_interface = None; }
                });
            });
            if confirm {
                self.retry_saving = Some(id);
                if !self.send(Command::Set(Setting::Advertise(self.retry_choice.clone()))) {
                    self.retry_saving = None;
                }
            }
            if modal.should_close() {
                self.retry_interface = None;
            }
        }
    }

    pub(super) fn test_ssh_transport(&mut self, d: &DeviceSnapshot, ctx: &egui::Context) {
        self.status = format!("Testing SSH transport to {}:{}…", d.host, d.port);
        let host = d.host.clone();
        let port = d.port;
        let tx = self.tx.clone();
        let app = self.app.clone();
        let hostname = d.name.clone();
        let model = d.facts.version.as_ref().and_then(|v| v.model.clone());
        let ctx = ctx.clone();
        self.runtime.spawn(async move {
            let result = transferbuddy_core::switch::test_ssh_transport(&host, port).await;
            let level = if result.is_ok() { transferbuddy_core::logging::LogLevel::Info } else { transferbuddy_core::logging::LogLevel::Error };
            let status = match result {
                Ok(()) => format!("SSH negotiation to {host}:{port} works. No login attempted; reconnect with your credentials."),
                Err(error) => format!("{error:#}"),
            };
            if let Some(app) = app {
                let mut event = transferbuddy_core::logging::Event::new(
                    level, "SSH", "transport test")
                    .device(hostname, model).result(status.clone());
                if let Ok(ip) = host.parse() { event = event.ip(ip); }
                app.logger.log(event);
            }
            let _ = tx.send(UiEvent::Status(status)); ctx.request_repaint();
        });
    }
}
