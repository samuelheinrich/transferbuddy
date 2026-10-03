use super::*;
fn release_order(version: &str) -> Vec<u32> {
    version
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}
fn log_protocol_label(proto: &str) -> String {
    if proto.eq_ignore_ascii_case("switch") {
        "SSH".into()
    } else {
        proto.to_uppercase()
    }
}
use egui_extras::{Column, TableBuilder};
fn table_area<R>(ui: &mut egui::Ui, id: &str, draw: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let rect = ui.available_rect_before_wrap();
    ui.scope_builder(egui::UiBuilder::new().id_salt(id).max_rect(rect), |ui| {
        ui.set_clip_rect(rect.intersect(ui.clip_rect()));
        draw(ui)
    })
    .inner
}
fn table<'a>(ui: &'a mut egui::Ui, id: &str, widths: &[f32]) -> TableBuilder<'a> {
    let available = ui.available_width();
    let width_key = ui.id().with(("table_available", id));
    let reset_width = ui.ctx().data_mut(|data| {
        let changed = data
            .get_temp::<f32>(width_key)
            .is_some_and(|old| (old - available).abs() > 1.0);
        data.insert_temp(width_key, available);
        changed
    });
    let fixed: f32 = widths.iter().filter(|w| **w > 0.0).sum();
    let spacing = ui.spacing().item_spacing.x * widths.len().saturating_sub(1) as f32;
    let remainder_min = if id == "connect_devices" { 220.0 } else { 80.0 };
    let ratio = ((available - spacing - remainder_min) / fixed.max(1.0)).clamp(0.1, 1.0);
    let key = format!("{id}-{}", widths.len());
    let stored = ui
        .ctx()
        .data(|data| data.get_temp::<ColumnHistory>(egui::Id::new("console_columns")))
        .and_then(|h| h.get(&key).cloned());
    let mut t = TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .min_scrolled_height(0.0)
        .auto_shrink([false, false]);
    for (index, width) in widths.iter().enumerate() {
        t = t.column(if *width < 0.0 {
            Column::remainder()
                .at_least(remainder_min)
                .resizable(false)
                .clip(true)
        } else {
            Column::initial(
                stored
                    .as_ref()
                    .and_then(|w| w.get(index))
                    .copied()
                    .unwrap_or(*width),
            )
            .at_least(45.0)
            .at_most(if ratio < 1.0 {
                (*width * ratio).max(45.0)
            } else {
                available.max(45.0)
            })
            .clip(true)
        });
    }
    if reset_width {
        t.reset();
    }
    t
}
type ColumnHistory = std::collections::BTreeMap<String, Vec<f32>>;
fn remember_columns(body: &mut egui_extras::TableBody<'_>, id: &str) {
    let widths = body.widths().to_vec();
    let key = format!("{id}-{}", widths.len());
    body.ui_mut().ctx().data_mut(|data| {
        let map = data.get_temp_mut_or_default::<ColumnHistory>(egui::Id::new("console_columns"));
        map.insert(key, widths);
    });
}
fn headers(mut row: egui_extras::TableRow<'_, '_>, labels: &[&str]) {
    for label in labels {
        row.col(|ui| {
            ui.label(
                RichText::new(*label)
                    .small()
                    .color(ui.visuals().hyperlink_color),
            );
        });
    }
}
pub(super) fn state_color(state: &SwitchState) -> Color32 {
    match state {
        SwitchState::Ready => GREEN,
        SwitchState::Failed { .. } => RED,
        SwitchState::Offline { .. } => AMBER,
        SwitchState::Closed => design::DIM,
        _ => CYAN,
    }
}
fn file_kind(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        "DIR"
    } else if name.eq_ignore_ascii_case("packages.conf") {
        "BOOT"
    } else if upgrade::ios_file(name) {
        if name.ends_with(".bin") {
            "IMAGE"
        } else if name.ends_with(".pkg") {
            "PACKAGE"
        } else {
            "CONFIG"
        }
    } else {
        "FILE"
    }
}
impl Desktop {
    pub(super) fn assign(&mut self, all: bool) {
        if let Some(local) = self.image.clone() {
            let devices = if all {
                self.snapshot.devices.iter().map(|d| d.id).collect()
            } else {
                if self.upgrade_selected.is_empty() {
                    self.device.into_iter().collect()
                } else {
                    self.upgrade_selected.iter().copied().collect()
                }
            };
            self.send(Command::AssignImage { devices, local });
        }
    }
    pub(super) fn move_selection(&mut self, delta: isize) {
        if self.tab == Tab::Connect
            || (self.tab == Tab::Transfer && self.focus == 0)
            || (self.tab == Tab::Upgrade && self.focus == 0)
        {
            let devices = if self.tab == Tab::Connect {
                self.filtered_devices()
            } else {
                self.snapshot.devices.clone()
            };
            if !devices.is_empty() {
                let index = devices
                    .iter()
                    .position(|d| Some(d.id) == self.device)
                    .unwrap_or(0);
                let next = index.saturating_add_signed(delta).min(devices.len() - 1);
                self.choose_device(devices[next].id);
                self.scroll_device = true;
            }
            return;
        }
        if self.tab != Tab::Transfer && self.tab != Tab::Upgrade {
            return;
        }
        self.scroll_files = true;
        if self.focus == 1 {
            if let Some(Ok(entries)) = self.snapshot.listings.get(&self.local_dir) {
                let mut files = entries.clone();
                self.sort_files(&mut files);
                if !files.is_empty() {
                    let index = files
                        .iter()
                        .position(|f| {
                            self.local_selection
                                .as_ref()
                                .is_some_and(|s| s.name == f.name)
                        })
                        .unwrap_or(0);
                    let file =
                        files[index.saturating_add_signed(delta).min(files.len() - 1)].clone();
                    self.local_selected.clear();
                    self.local_selected.insert(file.name.clone());
                    self.local_anchor = Some(file.name.clone());
                    if self.tab == Tab::Upgrade && !file.is_dir {
                        self.image = Some(self.local_path(&file.name));
                    }
                    self.local_selection = Some(file);
                }
            }
        } else if self.focus == 2 {
            if let Some(d) = self.selected() {
                let path = self.remote_path("");
                let files = self
                    .app
                    .as_ref()
                    .and_then(|a| a.engine().device(d.id).ok())
                    .and_then(|s| s.listing(&path))
                    .and_then(Result::ok)
                    .unwrap_or_default();
                let files: Vec<_> = files
                    .into_iter()
                    .filter(|f| {
                        self.remote_filter.is_empty()
                            || f.name
                                .to_lowercase()
                                .contains(&self.remote_filter.to_lowercase())
                    })
                    .collect();
                if !files.is_empty() {
                    let index = files
                        .iter()
                        .position(|f| {
                            self.remote_selection
                                .as_ref()
                                .is_some_and(|s| s.name == f.name)
                        })
                        .unwrap_or(0);
                    let file =
                        files[index.saturating_add_signed(delta).min(files.len() - 1)].clone();
                    self.remote_selected.clear();
                    self.remote_selected.insert(file.name.clone());
                    self.remote_anchor = Some(file.name.clone());
                    self.remote_selection = Some(file);
                }
            }
        }
    }
    fn filtered_devices(&self) -> Vec<DeviceSnapshot> {
        let q = self.device_filter.to_lowercase();
        self.snapshot
            .devices
            .iter()
            .filter(|d| {
                (!self.needs_attention
                    || d.state.is_over()
                    || d.recovery.phase == engine::RecoveryPhase::LoginRequired
                    || matches!(d.upgrade, upgrade::Progress::Failed(_)))
                    && (q.is_empty()
                        || format!("{} {}", d.name, d.host).to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }
    pub(super) fn cached_local_files(
        &mut self,
    ) -> Option<Result<std::sync::Arc<Vec<FileEntry>>, String>> {
        let entries = self.snapshot.listings.get(&self.local_dir)?;
        let key = (
            self.local_dir.clone(),
            self.filter.clone(),
            self.sort,
            self.snapshot.listing_revision,
        );
        if self.local_cache_key.as_ref() != Some(&key) {
            let mut files = match entries {
                Ok(files) => files.clone(),
                Err(error) => return Some(Err(error.clone())),
            };
            self.sort_files(&mut files);
            self.local_cache = std::sync::Arc::new(files);
            self.local_cache_key = Some(key);
        }
        Some(Ok(self.local_cache.clone()))
    }
    fn sort_files(&self, files: &mut Vec<FileEntry>) {
        files.retain(|e| {
            e.name == ".."
                || self.filter.is_empty()
                || e.name.to_lowercase().contains(&self.filter.to_lowercase())
        });
        files.sort_by(|a, b| {
            b.is_dir.cmp(&a.is_dir).then_with(|| match self.sort {
                1 => b.size.cmp(&a.size),
                2 => b.modified.cmp(&a.modified),
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            })
        });
    }
    fn open_service(&mut self, service: &engine::ServiceSnapshot) {
        let cfg = self.app.as_ref().unwrap().config.read().unwrap().clone();
        self.service = Some(ServiceForm {
            id: service.id,
            port: service.settings.port.to_string(),
            bind: service.settings.bind.clone(),
            user: cfg.auth.username,
            password: cfg.auth.password,
            uploads: cfg.uploads.enabled,
            overwrite: cfg.uploads.overwrite,
            upload_dir: cfg.uploads.dir,
            max_upload: cfg.uploads.max_upload_mib,
            enabled: service.settings.enabled,
        });
    }
    pub(super) fn dashboard(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical()
            .id_salt("dashboard_view")
            .show(ui, |ui| {
                design::panel(ui, "WORKSPACE", false, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("ROOT").small().color(design::muted(ui)));
                        design::label(ui, self.snapshot.root.display().to_string())
                            .on_hover_text(self.snapshot.root.display().to_string());
                        if ui.button("Choose folder…").clicked() {
                            self.pick_root(ui.ctx());
                        }
                    });
                });
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    if design::primary(ui, "New workflow…").clicked() {
                        self.open_wizard();
                    }
                    ui.label("Connect → Transfer → Upgrade · guided or use the tabs");
                });
                if let Some(message) = &self.snapshot.network.message {
                    ui.colored_label(design::semantic(ui, AMBER), message);
                }
                ui.add_space(12.0);
                if ui.available_width() >= 820.0 {
                    ui.columns(2, |columns| {
                        self.dashboard_services(&mut columns[0]);
                        design::panel(&mut columns[1], "TRANSFER INTERFACE", false, |ui| {
                            self.address_selection(ui)
                        });
                    });
                } else {
                    self.dashboard_services(ui);
                    ui.add_space(12.0);
                    design::panel(ui, "TRANSFER INTERFACE", false, |ui| {
                        self.address_selection(ui)
                    });
                }
                ui.add_space(12.0);
                design::panel(ui, "TRANSFERS", false, |ui| self.transfer_rows(ui));
                ui.add_space(12.0);
                design::panel(ui, "RECENT OPERATIONS", false, |ui| {
                    if self.snapshot.operations.is_empty() {
                        ui.label(
                            RichText::new("Activity will appear here as you work.")
                                .color(design::muted(ui)),
                        );
                    }
                    for o in self.snapshot.operations.iter().rev().take(6) {
                        ui.horizontal(|ui| {
                            let (label, color) = match &o.state {
                                engine::OperationState::Running => ("RUNNING", CYAN),
                                engine::OperationState::Complete => ("DONE", GREEN),
                                engine::OperationState::Failed(_)
                                | engine::OperationState::Uncertain(_) => (o.state.label(), RED),
                                _ => (o.state.label(), AMBER),
                            };
                            design::badge(ui, label, color);
                            design::label(ui, &o.label);
                            if let engine::OperationState::Failed(error) = &o.state {
                                design::label(ui, error).on_hover_text(error);
                            }
                        });
                    }
                });
            });
    }
    fn dashboard_services(&mut self, ui: &mut egui::Ui) {
        design::panel(ui, "SERVICES", false, |ui| {
            ui.horizontal(|ui| {
                if design::primary(ui, "Start enabled").clicked() {
                    self.send(Command::StartEnabled);
                }
                if ui.button("Stop all").clicked() {
                    self.send(Command::StopAll);
                }
            });
            let services = self.snapshot.services.clone();
            let h = design::row_height(ui) * 1.6;
            table(ui, "service_table", &[120.0, 65.0, 85.0, -1.0])
                .max_scroll_height(h * services.len() as f32 + 2.0)
                .header(28.0, |r| {
                    headers(r, &["Service", "Run", "Options", "State / address"])
                })
                .body(|body| {
                    body.rows(h, services.len(), |mut row| {
                        let s = &services[row.index()];
                        row.col(|ui| {
                            ui.push_id(s.id, |ui| {
                                let mut enabled = s.settings.enabled;
                                if ui.checkbox(&mut enabled, s.id.display_name()).changed() {
                                    let mut cfg = s.settings.clone();
                                    cfg.enabled = enabled;
                                    self.send(Command::Set(Setting::Service(s.id, cfg)));
                                }
                            });
                        });
                        row.col(|ui| {
                            ui.push_id(s.id, |ui| {
                                if ui
                                    .button(if s.status.is_running() {
                                        "Stop"
                                    } else {
                                        "Start"
                                    })
                                    .clicked()
                                {
                                    self.send(if s.status.is_running() {
                                        Command::StopService(s.id)
                                    } else {
                                        Command::StartService(s.id)
                                    });
                                }
                            });
                        });
                        row.col(|ui| {
                            ui.push_id(s.id, |ui| {
                                ui.menu_button("Options", |ui| {
                                    if ui.button("Configure…").clicked() {
                                        self.open_service(s);
                                        ui.close();
                                    }
                                    if ui.button("Restart").clicked() {
                                        self.send(Command::RestartService(s.id));
                                        ui.close();
                                    }
                                });
                            });
                        });
                        row.col(|ui| {
                            let color = match s.status {
                                ServiceStatus::Running => GREEN,
                                ServiceStatus::Failed(_) => RED,
                                ServiceStatus::Starting | ServiceStatus::Stopping => CYAN,
                                _ => design::muted(ui),
                            };
                            design::label(ui, s.status.label()).on_hover_text(match &s.status {
                                ServiceStatus::Failed(e) => e.as_str(),
                                _ => s.status.label(),
                            });
                            ui.colored_label(
                                design::semantic(ui, color),
                                format!("{}:{}", s.settings.bind, s.settings.port),
                            );
                        });
                    });
                });
            if let Some(app) = &self.app {
                let cfg = app.config.read().unwrap();
                ui.label(format!("Server login: {} / ••••••••", cfg.auth.username));
            }
            for s in services {
                if let ServiceStatus::Failed(error) = s.status {
                    ui.colored_label(
                        design::semantic(ui, RED),
                        format!("{}: {error}", s.id.display_name()),
                    );
                }
            }
        });
    }
    pub(super) fn address_selection(&mut self, ui: &mut egui::Ui) {
        let Some(app) = &self.app else { return };
        let selected = app.config.read().unwrap().advertise.clone();
        ui.label("Addresses in copy URLs");
        let mut choice = selected.clone();
        if ui
            .selectable_label(
                selected.is_none(),
                "Automatic · choose address for the device",
            )
            .clicked()
        {
            choice = None;
        }
        egui::ScrollArea::vertical()
            .id_salt("transfer_interfaces")
            .max_height(220.0)
            .show(ui, |ui| {
                for iface in &self.interfaces {
                    let ip = iface.ip.to_string();
                    if ui
                        .selectable_label(
                            selected.as_deref() == Some(&ip)
                                || selected.as_deref() == Some(&iface.name),
                            format!("{} · {} · {}", iface.name, ip, iface.kind.label()),
                        )
                        .clicked()
                    {
                        choice = Some(iface.name.clone());
                    }
                }
            });
        ui.horizontal(|ui| {
            ui.label("Interface or IP");
            ui.add(
                egui::TextEdit::singleline(&mut self.interface)
                    .id_salt("advertise_input")
                    .desired_width(150.0),
            );
            if ui.button("Apply").clicked() {
                choice =
                    (!self.interface.trim().is_empty()).then(|| self.interface.trim().to_owned());
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Refresh interfaces").clicked() {
                let tx = self.tx.clone();
                let ctx = ui.ctx().clone();
                std::thread::spawn(move || {
                    let _ = tx.send(UiEvent::Interfaces(transferbuddy_core::netif::candidates()));
                    ctx.request_repaint();
                });
            }
            ui.label(
                RichText::new(format!(
                    "Using: {}",
                    selected.as_deref().unwrap_or("automatic")
                ))
                .small()
                .color(design::muted(ui)),
            );
        });
        if choice != selected {
            self.send(Command::Set(Setting::Advertise(choice)));
        }
    }
    fn transfer_rows(&self, ui: &mut egui::Ui) {
        let transfers: Vec<_> = self
            .snapshot
            .transfers
            .iter()
            .filter(|t| t.file.is_some())
            .collect();
        if transfers.is_empty() {
            design::empty(
                ui,
                "No file transfers yet",
                "Connect devices in 2 Connect, then open 3 Transfer.",
            );
            return;
        }
        let bits = self
            .app
            .as_ref()
            .is_some_and(|a| a.config.read().unwrap().speed_in_bits);
        table(ui, "active_transfers", &[-1.0, 90.0, 160.0, 130.0, 85.0])
            .max_scroll_height(200.0)
            .header(28.0, |r| {
                headers(r, &["File / peer", "Protocol", "Progress", "Speed", "ETA"])
            })
            .body(|mut body| {
                remember_columns(&mut body, "active_transfers");
                let height = design::row_height(body.ui_mut()) * 1.7;
                body.rows(height, transfers.len(), |mut row| {
                    let t = transfers[row.index()];
                    row.col(|ui| {
                        design::label(ui, t.file.as_deref().unwrap_or("—"))
                            .on_hover_text(t.file.as_deref().unwrap_or(""));
                        ui.label(
                            RichText::new(format!("{} · {}", t.peer, t.state.label()))
                                .small()
                                .color(design::muted(ui)),
                        );
                    });
                    row.col(|ui| {
                        design::badge(ui, t.protocol.label(), CYAN);
                    });
                    row.col(|ui| design::progress(ui, t.progress(), GREEN));
                    row.col(|ui| {
                        ui.label(fmt_speed(t.current_speed, bits));
                    });
                    row.col(|ui| {
                        ui.label(t.eta().map(fmt_duration).unwrap_or_else(|| "—".into()));
                    });
                })
            });
    }
    pub(super) fn connections(&mut self, ui: &mut egui::Ui) {
        design::heading(
            ui,
            "CONNECT",
            if ui.available_width() < 750.0 {
                "Shared SSH connections"
            } else {
                "SSH sessions shared by Transfer and Upgrade"
            },
        );
        ui.add_space(8.0);
        let narrow = ui.available_width() < 750.0;
        if narrow {
            ui.horizontal(|ui| {
                if design::primary(ui, "Add device").clicked() {
                    self.credentials_device = None;
                    self.connect = Some(ConnectionForm {
                        port: "22".into(),
                        ..Default::default()
                    });
                }
                if ui.button("Bulk add / subnet").clicked() {
                    self.credentials_device = None;
                    self.connect = Some(ConnectionForm {
                        port: "22".into(),
                        bulk: true,
                        ..Default::default()
                    });
                }
                ui.menu_button(
                    if self.needs_attention {
                        "Attention"
                    } else {
                        "Filters"
                    },
                    |ui| {
                        ui.checkbox(&mut self.needs_attention, "Needs attention");
                        if ui.button("Clear finished").clicked() {
                            self.send(Command::ClearFinished);
                            ui.close();
                        }
                        ui.separator();
                        ui.collapsing("Device details / connection errors", |ui| {
                            self.connection_details(ui)
                        });
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.device_filter)
                            .hint_text("Search devices / IP…")
                            .desired_width(ui.available_width().min(160.0)),
                    );
                });
            });
        } else {
            ui.horizontal_wrapped(|ui| {
                if design::primary(ui, "Add device").clicked() {
                    self.credentials_device = None;
                    self.connect = Some(ConnectionForm {
                        port: "22".into(),
                        ..Default::default()
                    });
                }
                if ui.button("Bulk add / subnet").clicked() {
                    self.credentials_device = None;
                    self.connect = Some(ConnectionForm {
                        port: "22".into(),
                        bulk: true,
                        ..Default::default()
                    });
                }
                ui.checkbox(&mut self.needs_attention, "Needs attention");
                if ui.button("Clear finished").clicked() {
                    self.send(Command::ClearFinished);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.device_filter)
                            .hint_text("Search devices / IP…")
                            .desired_width(ui.available_width().min(240.0)),
                    );
                });
            });
        }
        if !self.connection_selected.is_empty() && ui.available_width() < 750.0 {
            ui.horizontal(|ui| {
                ui.label(format!("{} selected", self.connection_selected.len()));
                ui.menu_button("Selection actions", |ui| {
                    if ui.button("Reconnect selected").clicked() {
                        self.send(Command::ReconnectMany(
                            self.connection_selected.iter().copied().collect(),
                        ));
                        ui.close();
                    }
                    if ui.button("Review selected jobs").clicked() {
                        self.send(Command::ReviewPending(
                            self.connection_selected.iter().copied().collect(),
                        ));
                        self.jobs_open = true;
                        ui.close();
                    }
                    if ui.button("Clear selection").clicked() {
                        self.connection_selected.clear();
                        ui.close();
                    }
                });
            });
        } else if !self.connection_selected.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} selected · Cmd/Ctrl-click adds devices",
                    self.connection_selected.len()
                ));
                if ui.button("Reconnect selected").clicked() {
                    self.send(Command::ReconnectMany(
                        self.connection_selected.iter().copied().collect(),
                    ));
                }
                if ui.button("Review selected jobs").clicked() {
                    self.send(Command::ReviewPending(
                        self.connection_selected.iter().copied().collect(),
                    ));
                    self.jobs_open = true;
                }
                if ui.button("Clear selection").clicked() {
                    self.connection_selected.clear();
                }
            });
        }
        if let Some(scan) = self.snapshot.scan.clone() {
            ui.horizontal_wrapped(|ui| {
                design::badge(
                    ui,
                    if scan.finished {
                        "SCAN COMPLETE"
                    } else {
                        "SCANNING"
                    },
                    CYAN,
                );
                ui.label(format!(
                    "{} · {}/{} checked · {} reachable",
                    scan.subnet, scan.checked, scan.total, scan.reachable
                ));
                if !scan.finished && ui.button("Cancel scan").clicked() {
                    self.send(Command::CancelScan);
                }
                if ui.button("Scan logs").clicked() {
                    self.tab = Tab::Logs;
                    self.log_protocol = "switch".into();
                }
            });
        }
        ui.add_space(10.0);
        let devices = self.filtered_devices();
        if devices.is_empty() {
            design::panel(ui, "DEVICES", false, |ui| {
                design::empty(
                    ui,
                    "No devices in this view",
                    "Add a device or connect a subnet. Failed discovery attempts are in Logs.",
                )
            });
            return;
        }
        if narrow {
            // Details are available in Filters, preserving room for the device list.
        } else if ui.available_height() < 260.0 {
            ui.collapsing("Device details / connection errors", |ui| {
                self.connection_details(ui)
            });
        } else {
            egui::Panel::bottom("device_details")
                .resizable(true)
                .default_size(112.0)
                .size_range(85.0..=300.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| self.connection_details(ui));
        }
        table_area(ui, "connect_table_area", |ui| {
            let wide = ui.available_width() > 1100.0;
            let h = design::row_height(ui);
            let height = (ui.available_height() - 32.0).max(0.0);
            let widths = if wide {
                vec![180.0, 165.0, 155.0, 105.0, 160.0, -1.0]
            } else {
                vec![160.0, 125.0, 110.0, 100.0, -1.0]
            };
            let mut device_table = table(ui, "connect_devices", &widths).max_scroll_height(height);
            if self.scroll_device {
                self.scroll_device = false;
                if let Some(index) = devices.iter().position(|d| Some(d.id) == self.device) {
                    device_table = device_table.scroll_to_row(index, Some(egui::Align::Center));
                }
            }
            device_table
                .header(32.0, |r| {
                    headers(
                        r,
                        if wide {
                            &[
                                "Device / IP",
                                "Model",
                                "Mode / version",
                                "Free flash",
                                "SSH / ping",
                                "Actions",
                            ]
                        } else {
                            &[
                                "Device / IP",
                                "Mode / version",
                                "Free flash",
                                "SSH",
                                "Actions",
                            ]
                        },
                    )
                })
                .body(|mut body| {
                    remember_columns(&mut body, "connect_devices");
                    body.rows(h * 2.4, devices.len(), |mut row| {
                        let d = &devices[row.index()];
                        let info = d.facts.version.clone().unwrap_or_default();
                        row.set_selected(self.device == Some(d.id)||self.connection_selected.contains(&d.id));
                        row.col(|ui| {
                            let response = ui
                                .scope_builder(
                                    egui::UiBuilder::new().id(egui::Id::new(("device_name", d.id))),
                                    |ui| {
                                        ui.add(
                                            egui::Button::new(&d.name)
                                                .frame(false)
                                                .selected(self.device == Some(d.id))
                                                .truncate(),
                                        )
                                    },
                                )
                                .inner
                                .on_hover_text(&d.host);
                            self.list_widgets.insert(response.id);
                            if response.has_focus() {
                                self.focus = 0;
                            }
                            if response.clicked() {
                                if ui.input(|i|i.modifiers.command){if !self.connection_selected.remove(&d.id){self.connection_selected.insert(d.id);}}
                                else{self.connection_selected.clear();self.connection_selected.insert(d.id);self.choose_device(d.id);}
                            }
                            ui.label(RichText::new(&d.host).small().color(design::muted(ui)));
                        });
                        if wide {
                            row.col(|ui| {
                                design::label(ui, info.model.clone().unwrap_or_else(|| "—".into()))
                                    .on_hover_text(
                                        info.model.as_deref().unwrap_or("Unknown model"),
                                    );
                            });
                        }
                        row.col(|ui| {
                            design::label(ui, info.version.unwrap_or_else(|| "—".into()));
                            ui.label(RichText::new(mode(d)).small().color(design::muted(ui)));
                        });
                        row.col(|ui| {
                            ui.label(
                                d.facts
                                    .flash
                                    .map(|f| fmt_bytes(f.free))
                                    .unwrap_or_else(|| "—".into()),
                            );
                        });
                        row.col(|ui| {
                            design::badge(ui, d.state.label(), state_color(&d.state));
                            if wide {
                                ui.label(
                                    RichText::new(if d.reach.online {
                                        "● ping reachable"
                                    } else {
                                        "○ ping unavailable"
                                    })
                                    .small()
                                    .color(design::muted(ui)),
                                );
                            }
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                if ui.button("CLI").clicked() {
                                    self.choose_device(d.id);
                                    self.open_cli(d.id);
                                }
                                ui.scope_builder(
                                    egui::UiBuilder::new()
                                        .id(egui::Id::new(("device_actions", d.id))),
                                    |ui| {
                                        ui.menu_button("Actions", |ui| {
                                            if ui.button("Console").clicked() {
                                                self.console = Some(d.id);
                                                ui.close();
                                            }
                                            if ui
                                                .button(if d.state.is_over() {
                                                    "Reconnect"
                                                } else {
                                                    "Refresh"
                                                })
                                                .clicked()
                                            {
                                                self.send(if d.state.is_over() {
                                                    Command::Reconnect(d.id)
                                                } else {
                                                    Command::Refresh(d.id)
                                                });
                                                ui.close();
                                            }
                                            if ui.button("Logs").clicked() {
                                                self.tab = Tab::Logs;
                                                self.log_filter = d.host.clone();
                                                ui.close();
                                            }
                                            if ui.button("Disconnect").clicked() {
                                                self.send(Command::Disconnect(d.id));
                                                ui.close();
                                            }
                                        })
                                    },
                                );
                            let removable = !self.app.as_ref().is_some_and(|a| a.engine().busy(d.id))
                                && !matches!(d.state, SwitchState::Busy { .. } | SwitchState::Rebooting | SwitchState::ReloadConfirm { .. } | SwitchState::CleanupConfirm { .. });
                            if ui.add_enabled(removable, egui::Button::new("Remove"))
                                .on_disabled_hover_text("Finish or cancel device jobs, including the CLI, before removing it")
                                .on_hover_text("Disconnect and remove this device from all views").clicked() {
                                self.send(Command::RemoveDevice(d.id));
                            }
                            });
                        });
                    })
                });
            ui.add_space(10.0);
        });
    }
    fn connection_details(&mut self, ui: &mut egui::Ui) {
        if let Some(d) = self.selected() {
            design::panel(ui, "DEVICE DETAILS", false, |ui| {
                let info = d.facts.version.clone().unwrap_or_default();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{} · {}:{}", d.name, d.host, d.port));
                    design::badge(ui, &mode(&d), CYAN);
                    ui.label(info.model.unwrap_or_else(|| "Model pending".into()));
                });
                ui.label(
                    RichText::new(format!(
                        "Storage {} · uptime {}",
                        d.facts.flash_device,
                        info.uptime.unwrap_or_else(|| "—".into())
                    ))
                    .small()
                    .color(design::muted(ui)),
                );
                if let SwitchState::Failed { reason } | SwitchState::Offline { reason } = &d.state {
                    ui.horizontal_wrapped(|ui| {
                        ui.colored_label(design::semantic(ui, RED), reason);
                        if ui.button("Reconnect").clicked() {
                            self.send(Command::Reconnect(d.id));
                        }
                        if ui.button("View logs").clicked() {
                            self.tab = Tab::Logs;
                            self.log_filter = d.host.clone();
                        }
                    });
                    ui.label(d.recovery.label());
                    if let Some(failure) = &d.recovery.reason {
                        ui.label(failure.remedy);
                    }
                    if ui.button("Edit credentials…").clicked() {
                        self.credentials_device = Some(d.id);
                        self.connect = Some(ConnectionForm {
                            targets: d.host.clone(),
                            port: d.port.to_string(),
                            user: d.username.clone(),
                            ..Default::default()
                        });
                    }
                    if ui.button("Test SSH transport").clicked() {
                        self.test_ssh_transport(&d, ui.ctx());
                    }
                    if let Some(hint) = transferbuddy_core::switch::macos_network_hint(reason) {
                        ui.label(RichText::new(hint).small().color(design::accent(ui)));
                        if ui.button("Open Local Network settings").clicked() {
                            if let Err(e) = crate::native::open_network_privacy_settings() {
                                self.status = e;
                            }
                        }
                    }
                }
            });
        }
    }
    pub(super) fn transfer_view(&mut self, ui: &mut egui::Ui) {
        let tight = ui.available_height() < 320.0;
        let small = ui.available_height() < 470.0;
        let collapsed = if small {
            !self.small_devices_open
        } else {
            self.prefs.devices_collapsed
        };
        egui::Panel::bottom("copy_actions")
            .exact_size(if tight { 42.0 } else { 98.0 })
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.transfer_actions(ui));
        let max = (ui.available_height() - 290.0).clamp(100.0, 480.0);
        let devices = egui::Panel::top(if collapsed {
            "device_summary"
        } else {
            "device_strip"
        })
        .resizable(!collapsed)
        .default_size(if collapsed {
            42.0
        } else {
            self.prefs.devices_height.min(max)
        })
        .size_range(if collapsed { 42.0..=42.0 } else { 100.0..=max })
        .frame(egui::Frame::NONE)
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.horizontal_wrapped(|ui| {
                if ui
                    .small_button(if collapsed {
                        "▸ Show devices"
                    } else {
                        "▾ Hide devices"
                    })
                    .clicked()
                {
                    if small {
                        self.small_devices_open = !self.small_devices_open;
                    } else {
                        self.prefs.devices_collapsed = !self.prefs.devices_collapsed;
                    }
                }
                if let Some(d) = self.selected() {
                    ui.label(format!("{} · {} · {}", d.name, d.host, d.state.label()));
                    if ui
                        .button(d.protocol.label())
                        .on_hover_text("Choose transfer protocol")
                        .clicked()
                    {
                        self.protocol_picker = Some((d.id, false, false));
                    }
                }
            });
            if !collapsed {
                self.devices(ui);
            }
        });
        if !collapsed {
            self.prefs.devices_height = devices.response.rect.height();
        }
        ui.add_space(8.0);
        if ui.available_width() < 700.0 {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.focus != 2, "Local files")
                    .clicked()
                {
                    self.focus = 1;
                }
                if ui
                    .selectable_label(self.focus == 2, "Remote files")
                    .clicked()
                {
                    self.focus = 2;
                }
            });
            if self.focus == 2 {
                self.remote_browser(ui);
            } else {
                self.browser(ui, false);
            }
            return;
        }
        // Store a ratio, rather than an egui panel's cached pixel width. Both
        // browsers must stay usable when a previously large window is resized.
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let divider_x = rect.left() + rect.width() * self.prefs.split.clamp(0.25, 0.75);
        let divider = egui::Rect::from_min_max(
            egui::pos2(divider_x - 5.0, rect.top()),
            egui::pos2(divider_x + 5.0, rect.bottom()),
        );
        let drag = ui.interact(divider, ui.id().with("file_split"), egui::Sense::drag());
        if drag.dragged() {
            self.prefs.split =
                (self.prefs.split + drag.drag_delta().x / rect.width()).clamp(0.25, 0.75);
        }
        if drag.hovered() || drag.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            ui.painter().vline(
                divider_x,
                rect.y_range(),
                egui::Stroke::new(1.0, design::accent(ui)),
            );
        }
        let left = egui::Rect::from_min_max(rect.min, egui::pos2(divider.left(), rect.bottom()));
        let right = egui::Rect::from_min_max(egui::pos2(divider.right(), rect.top()), rect.max);
        ui.scope_builder(
            egui::UiBuilder::new().id_salt("local_pane").max_rect(left),
            |ui| {
                ui.set_clip_rect(left.intersect(ui.clip_rect()));
                self.browser(ui, false);
                let target = ui.interact(left, egui::Id::new("drop_local"), egui::Sense::hover());
                if let Some(payload) = target.dnd_release_payload::<workspace::FileDrag>() {
                    self.drop_files(&payload, false);
                }
            },
        );
        ui.scope_builder(
            egui::UiBuilder::new()
                .id_salt("remote_pane")
                .max_rect(right),
            |ui| {
                ui.set_clip_rect(right.intersect(ui.clip_rect()));
                self.remote_browser(ui);
                let target = ui.interact(right, egui::Id::new("drop_remote"), egui::Sense::hover());
                if let Some(payload) = target.dnd_release_payload::<workspace::FileDrag>() {
                    self.drop_files(&payload, true);
                }
            },
        );
    }
    fn devices(&mut self, ui: &mut egui::Ui) {
        let h = 26.0;
        let capacity = ((ui.available_height() - 64.0) / h).max(1.0) as usize;
        design::heading(
            ui,
            "DEVICES",
            &format!(
                "{} devices · ↑↓ select · Enter protocol{}",
                self.snapshot.devices.len(),
                if self.snapshot.devices.len() > capacity {
                    " · ↕ scroll for more"
                } else {
                    ""
                }
            ),
        );
        let devices = self.snapshot.devices.clone();
        if devices.is_empty() {
            design::empty(
                ui,
                "Connect a device to transfer files",
                "Connections are managed in 2 Connect.",
            );
            if ui.button("Open Connect").clicked() {
                self.tab = Tab::Connect;
            }
            return;
        }
        let selected = devices.iter().position(|d| Some(d.id) == self.device);
        let max_height = (ui.available_height() - 60.0).max(h);
        let mut t = table(ui, "transfer_devices", &[170.0, 150.0, 170.0, 110.0, -1.0])
            .sense(egui::Sense::click())
            .max_scroll_height(max_height);
        if self.scroll_device {
            self.scroll_device = false;
            if let Some(index) = selected {
                t = t.scroll_to_row(index, Some(egui::Align::Center));
            }
        }
        let output = t
            .header(28.0, |r| {
                headers(r, &["Device", "Address", "State", "Protocol", "Platform"])
            })
            .body(|mut body| {
                body.ui_mut().spacing_mut().item_spacing.y = 0.0;
                body.ui_mut().spacing_mut().interact_size.y = 24.0;
                remember_columns(&mut body, "transfer_devices");
                body.rows(h, devices.len(), |mut row| {
                    let d = &devices[row.index()];
                    row.set_selected(self.device == Some(d.id));
                    row.col(|ui| {
                        let response = ui
                            .push_id(("transfer_device", d.id), |ui| {
                                ui.add(
                                    egui::Button::new(&d.name)
                                        .frame(false)
                                        .selected(self.device == Some(d.id))
                                        .truncate(),
                                )
                            })
                            .inner
                            .on_hover_text(&d.name);
                        self.list_widgets.insert(response.id);
                        if response.has_focus() {
                            self.focus = 0;
                        }
                        if response.clicked() {
                            self.choose_device(d.id);
                        }
                    });
                    row.col(|ui| {
                        ui.label(&d.host);
                    });
                    row.col(|ui| {
                        design::badge(ui, d.state.label(), state_color(&d.state));
                    });
                    row.col(|ui| {
                        if ui
                            .button(d.protocol.label())
                            .on_hover_text("Choose transfer protocol")
                            .clicked()
                        {
                            self.choose_device(d.id);
                            self.protocol_picker = Some((d.id, false, false));
                        }
                    });
                    row.col(|ui| {
                        design::label(
                            ui,
                            d.facts
                                .version
                                .as_ref()
                                .and_then(|v| v.model.clone())
                                .unwrap_or_else(|| "—".into()),
                        );
                    });
                    if row.response().clicked() {
                        self.choose_device(d.id);
                    }
                })
            });
        if devices.len() > capacity {
            let start = (output.state.offset.y / h) as usize;
            ui.label(
                RichText::new(format!(
                    "{}–{} / {}  ↕ Scroll for more devices",
                    (start + 1).min(devices.len()),
                    (start + capacity).min(devices.len()),
                    devices.len()
                ))
                .small()
                .color(design::muted(ui)),
            );
        }
    }
    fn breadcrumb(&mut self, ui: &mut egui::Ui, remote: bool) {
        let dir = if remote {
            self.remote_dir.clone()
        } else {
            self.local_dir.clone()
        };
        ui.horizontal_wrapped(|ui| {
            if ui.small_button("Root").clicked() {
                if remote {
                    self.remote_dir.clear();
                    self.remote_selection = None;
                    self.remote_selected.clear();
                    self.remote_loaded = None;
                } else {
                    self.local_dir.clear();
                    self.local_selection = None;
                    self.local_selected.clear();
                    self.refresh_local();
                }
            }
            let mut path = String::new();
            for part in dir.split('/').filter(|p| !p.is_empty()) {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(part);
                ui.label(RichText::new("›").color(design::muted(ui)));
                if ui.small_button(part).clicked() {
                    if remote {
                        self.remote_dir = path.clone();
                        self.remote_selection = None;
                        self.remote_selected.clear();
                        self.remote_loaded = None;
                    } else {
                        self.local_dir = path.clone();
                        self.local_selection = None;
                        self.local_selected.clear();
                        self.refresh_local();
                    }
                }
            }
        });
    }
    fn file_filter(&mut self, ui: &mut egui::Ui) {
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter files…")
                .desired_width((ui.available_rect_before_wrap().width() - 130.0).max(80.0)),
        );
        egui::ComboBox::from_id_salt("local_sort")
            .selected_text(["Name", "Size", "Modified"][self.sort])
            .show_ui(ui, |ui| {
                for (i, s) in ["Name", "Size", "Modified"].iter().enumerate() {
                    ui.selectable_value(&mut self.sort, i, *s);
                }
            });
    }
    fn browser_frame<R>(
        ui: &mut egui::Ui,
        title: &str,
        focused: bool,
        compact: bool,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> egui::InnerResponse<R> {
        if compact {
            egui::Frame::new()
                .fill(ui.visuals().faint_bg_color)
                .inner_margin(6)
                .show(ui, contents)
        } else {
            design::panel(ui, title, focused, contents)
        }
    }
    fn browser(&mut self, ui: &mut egui::Ui, upgrade_browser: bool) {
        let title = if upgrade_browser {
            "UPGRADE IMAGES"
        } else {
            "LOCAL"
        };
        let compact = ui.available_height() < 230.0;
        Self::browser_frame(
            ui,
            title,
            self.focus == 1 && !upgrade_browser,
            compact,
            |ui| {
                ui.set_min_width(ui.available_width().max(0.0));
                ui.horizontal_wrapped(|ui| {
                    self.breadcrumb(ui, false);
                    if ui.small_button("↑ Parent").clicked() {
                        self.local_dir = parent(&self.local_dir);
                        self.local_selection = None;
                        self.local_selected.clear();
                        self.refresh_local();
                    }
                    if ui.small_button("Refresh").clicked() {
                        self.refresh_local();
                    }
                    if compact {
                        ui.menu_button("More", |ui| {
                            self.file_filter(ui);
                            if ui.button("Hashes").clicked() {
                                self.intent(Intent::Hash, ui.ctx());
                                ui.close();
                            }
                            if ui.button("Copy commands…").clicked() {
                                if let Some(file) = &self.local_selection {
                                    self.copy_commands(self.local_path(&file.name), ui.ctx());
                                }
                                ui.close();
                            }
                        });
                    }
                    if upgrade_browser {
                        self.file_filter(ui);
                    }
                });
                if !upgrade_browser && !compact {
                    ui.horizontal(|ui| self.file_filter(ui));
                    egui::Panel::bottom("local_browser_actions")
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("Hashes").clicked() {
                                    if let Some(f) =
                                        self.local_selection.clone().filter(|f| !f.is_dir)
                                    {
                                        let path = self.local_path(&f.name);
                                        self.hash_view = Some(path.clone());
                                        self.snapshot.hashes.remove(&path);
                                        self.hash_compare.clear();
                                        self.send(Command::Hash(path));
                                    }
                                }
                                if ui.button("Copy commands…").clicked() {
                                    if let Some(file) = &self.local_selection {
                                        self.copy_commands(self.local_path(&file.name), ui.ctx());
                                    }
                                }
                            });
                        });
                }
                table_area(ui, "local_table_area", |ui| {
                    let entries = self.cached_local_files();
                    let h = design::row_height(ui);
                    let max = (ui.available_height() - 32.0).max(h);
                    match entries {
                        Some(Ok(files)) => {
                            if files.is_empty() {
                                design::empty(
                                    ui,
                                    "No matching files",
                                    "Try another filter or folder.",
                                );
                            }
                            let mut file_table = table(ui, "local_files", &[-1.0, 80.0, 84.0]);
                            if self.scroll_files && self.focus == 1 {
                                self.scroll_files = false;
                                if let Some(index) = files.iter().position(|f| {
                                    self.local_selection
                                        .as_ref()
                                        .is_some_and(|s| s.name == f.name)
                                }) {
                                    file_table =
                                        file_table.scroll_to_row(index, Some(egui::Align::Center));
                                }
                            }
                            file_table
                                .max_scroll_height(max)
                                .header(28.0, |r| headers(r, &["Name", "Size", "Type"]))
                                .body(|mut body| {
                                    remember_columns(&mut body, "local_files");
                                    body.rows(h, files.len(), |mut row| {
                                        let f = &files[row.index()];
                                        let selected = self.local_selected.contains(&f.name);
                                        row.set_selected(selected);
                                        row.col(|ui| {
                                            let name = format!(
                                                "{} {}{}",
                                                if f.is_dir { "▸" } else { " " },
                                                f.name,
                                                if f.is_dir { "/" } else { "" }
                                            );
                                            let r = ui
                                                .scope_builder(
                                                    egui::UiBuilder::new().id(egui::Id::new((
                                                        "local_file",
                                                        self.local_path(&f.name),
                                                    ))),
                                                    |ui| {
                                                        ui.add(
                                                            egui::Button::selectable(
                                                                selected,
                                                                RichText::new(name).color(
                                                                    if upgrade::ios_file(&f.name) {
                                                                        design::accent(ui)
                                                                    } else {
                                                                        ui.visuals().text_color()
                                                                    },
                                                                ),
                                                            )
                                                            .frame(false)
                                                            .truncate()
                                                            .sense(egui::Sense::click_and_drag()),
                                                        )
                                                    },
                                                )
                                                .inner
                                                .on_hover_text(&f.name);
                                            self.list_widgets.insert(r.id);
                                            if r.has_focus() {
                                                self.focus = 1;
                                            }
                                            if !f.is_dir {
                                                r.dnd_set_drag_payload(workspace::FileDrag {
                                                    receive: false,
                                                    device: self.device,
                                                    directory: self.local_dir.clone(),
                                                    names: if self.local_selected.contains(&f.name)
                                                    {
                                                        self.selected_file_names(false)
                                                    } else {
                                                        vec![f.name.clone()]
                                                    },
                                                });
                                            }
                                            if r.clicked() {
                                                self.select_file(
                                                    false,
                                                    &f.name,
                                                    &files
                                                        .iter()
                                                        .map(|f| f.name.clone())
                                                        .collect::<Vec<_>>(),
                                                    ui.input(|i| i.modifiers),
                                                );
                                                self.local_selection = Some(f.clone());
                                                if upgrade_browser && !f.is_dir {
                                                    self.image = Some(self.local_path(&f.name));
                                                }
                                            }
                                            if r.double_clicked() {
                                                self.local_selection = Some(f.clone());
                                                if f.is_dir {
                                                    self.enter_local();
                                                } else if upgrade_browser {
                                                    self.image = Some(self.local_path(&f.name));
                                                } else {
                                                    self.enter_local();
                                                }
                                            }
                                        });
                                        row.col(|ui| {
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(if f.is_dir {
                                                        "—".into()
                                                    } else {
                                                        fmt_bytes(f.size)
                                                    });
                                                },
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                RichText::new(file_kind(&f.name, f.is_dir))
                                                    .small()
                                                    .color(if upgrade::ios_file(&f.name) {
                                                        design::accent(ui)
                                                    } else {
                                                        design::muted(ui)
                                                    }),
                                            );
                                        });
                                    })
                                });
                        }
                        Some(Err(error)) => {
                            ui.colored_label(design::semantic(ui, RED), error);
                        }
                        None => {
                            ui.spinner();
                            ui.label("Reading local folder…");
                        }
                    }
                });
            },
        );
    }
    fn remote_browser(&mut self, ui: &mut egui::Ui) {
        let Some(d) = self.selected() else {
            design::panel(ui, "REMOTE", false, |ui| {
                design::empty(ui, "No device selected", "Select a connected device above.")
            });
            return;
        };
        let path = self.remote_path("");
        if self.remote_loaded.as_ref() != Some(&(d.id, path.clone()))
            && d.state == SwitchState::Ready
            && !self.app.as_ref().unwrap().engine().busy(d.id)
            && self.send(Command::ListRemote(d.id, path.clone()))
        {
            self.remote_loaded = Some((d.id, path.clone()));
        }
        let listing = self
            .app
            .as_ref()
            .unwrap()
            .engine()
            .device(d.id)
            .ok()
            .and_then(|s| s.listing(&path));
        let compact = ui.available_height() < 230.0;
        Self::browser_frame(
            ui,
            &format!("REMOTE · {} / {}", d.name, d.facts.flash_device),
            self.focus == 2,
            compact,
            |ui| {
                ui.horizontal_wrapped(|ui| {
                    self.breadcrumb(ui, true);
                    if ui.small_button("↑ Parent").clicked() {
                        self.remote_dir = parent(&self.remote_dir);
                        self.remote_selection = None;
                        self.remote_selected.clear();
                        self.remote_loaded = None;
                    }
                    if ui.small_button("Refresh").clicked() {
                        self.remote_loaded = None;
                    }
                });
                if compact {
                    ui.menu_button("More", |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.remote_filter)
                                .hint_text("Filter remote files…"),
                        );
                        let selected = self
                            .remote_selection
                            .as_ref()
                            .is_some_and(|f| self.remote_selected.contains(&f.name));
                        if ui
                            .add_enabled(selected, egui::Button::new("Delete…"))
                            .clicked()
                        {
                            self.delete_selected();
                            ui.close();
                        }
                    });
                }
                if !compact {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.remote_filter)
                            .hint_text(format!("Filter {} / {}", d.name, path))
                            .desired_width(f32::INFINITY),
                    );
                    egui::Panel::bottom("remote_browser_actions")
                        .frame(egui::Frame::NONE)
                        .show(ui, |ui| {
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .add_enabled(
                                        self.remote_selection.as_ref().is_some_and(|f| {
                                            self.remote_selected.contains(&f.name)
                                        }),
                                        egui::Button::new(
                                            RichText::new("Delete…")
                                                .color(design::semantic(ui, RED)),
                                        ),
                                    )
                                    .clicked()
                                {
                                    self.delete_selected();
                                }
                            });
                        });
                }
                table_area(ui, "remote_table_area", |ui| {
                    let h = design::row_height(ui);
                    let max = (ui.available_height() - 32.0).max(h);
                    match listing {
                        Some(Ok(files)) => {
                            let files: Vec<_> = files
                                .into_iter()
                                .filter(|f| {
                                    self.remote_filter.is_empty()
                                        || f.name
                                            .to_lowercase()
                                            .contains(&self.remote_filter.to_lowercase())
                                })
                                .collect();
                            let mut file_table = table(ui, "remote_files", &[-1.0, 80.0, 84.0]);
                            if self.scroll_files && self.focus == 2 {
                                self.scroll_files = false;
                                if let Some(index) = files.iter().position(|f| {
                                    self.remote_selection
                                        .as_ref()
                                        .is_some_and(|s| s.name == f.name)
                                }) {
                                    file_table =
                                        file_table.scroll_to_row(index, Some(egui::Align::Center));
                                }
                            }
                            file_table
                                .max_scroll_height(max)
                                .header(28.0, |r| headers(r, &["Name", "Size", "Type"]))
                                .body(|mut body| {
                                    remember_columns(&mut body, "remote_files");
                                    body.rows(h, files.len(), |mut row| {
                                        let f = &files[row.index()];
                                        let selected = self.remote_selected.contains(&f.name);
                                        row.set_selected(selected);
                                        row.col(|ui| {
                                            let r = ui
                                                .scope_builder(
                                                    egui::UiBuilder::new().id(egui::Id::new((
                                                        "remote_file",
                                                        d.id,
                                                        self.remote_path(&f.name),
                                                    ))),
                                                    |ui| {
                                                        ui.add(
                                                            egui::Button::selectable(
                                                                selected,
                                                                RichText::new(format!(
                                                                    "{} {}{}",
                                                                    if f.is_dir {
                                                                        "▸"
                                                                    } else {
                                                                        " "
                                                                    },
                                                                    f.name,
                                                                    if f.is_dir { "/" } else { "" }
                                                                ))
                                                                .color(
                                                                    if upgrade::ios_file(&f.name) {
                                                                        design::accent(ui)
                                                                    } else {
                                                                        ui.visuals().text_color()
                                                                    },
                                                                ),
                                                            )
                                                            .frame(false)
                                                            .truncate()
                                                            .sense(egui::Sense::click_and_drag()),
                                                        )
                                                    },
                                                )
                                                .inner
                                                .on_hover_text(&f.name);
                                            self.list_widgets.insert(r.id);
                                            if r.has_focus() {
                                                self.focus = 2;
                                            }
                                            if !f.is_dir {
                                                r.dnd_set_drag_payload(workspace::FileDrag {
                                                    receive: true,
                                                    device: Some(d.id),
                                                    directory: self.remote_path(""),
                                                    names: if self.remote_selected.contains(&f.name)
                                                    {
                                                        self.selected_file_names(true)
                                                    } else {
                                                        vec![f.name.clone()]
                                                    },
                                                });
                                            }
                                            if r.clicked() {
                                                self.select_file(
                                                    true,
                                                    &f.name,
                                                    &files
                                                        .iter()
                                                        .map(|f| f.name.clone())
                                                        .collect::<Vec<_>>(),
                                                    ui.input(|i| i.modifiers),
                                                );
                                                self.remote_selection = Some(f.clone());
                                            }
                                            if r.double_clicked() {
                                                self.remote_selection = Some(f.clone());
                                                self.focus = 2;
                                                self.enter_remote();
                                            }
                                        });
                                        row.col(|ui| {
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(if f.is_dir {
                                                        "—".into()
                                                    } else {
                                                        fmt_bytes(f.size)
                                                    });
                                                },
                                            );
                                        });
                                        row.col(|ui| {
                                            ui.label(
                                                RichText::new(file_kind(&f.name, f.is_dir))
                                                    .small()
                                                    .color(if upgrade::ios_file(&f.name) {
                                                        design::accent(ui)
                                                    } else {
                                                        design::muted(ui)
                                                    }),
                                            );
                                        });
                                    })
                                });
                        }
                        Some(Err(error)) => {
                            ui.colored_label(design::semantic(ui, RED), error);
                        }
                        None => {
                            design::empty(
                                ui,
                                "Remote listing pending",
                                if d.state == SwitchState::Ready {
                                    "Reading device storage…"
                                } else {
                                    "Device must be ready to read its files."
                                },
                            );
                        }
                    }
                });
            },
        );
    }
    fn upgrade_assignments(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            if let Some(image) = self.image.clone() {
                design::label(ui, &image).on_hover_text(&image);
                if let Some(v) = upgrade::image_version(&image) {
                    design::badge(ui, &format!("TARGET {v}"), AMBER);
                }
            } else {
                ui.label(
                    RichText::new("Select an image above to assign it to your devices.")
                        .color(design::muted(ui)),
                );
            }
            if ui
                .add_enabled(
                    self.image.is_some() && self.device.is_some(),
                    egui::Button::new("Set image for selected devices"),
                )
                .clicked()
            {
                self.assign(false);
            }
            if ui
                .add_enabled(
                    self.image.is_some(),
                    egui::Button::new("Set image for all devices"),
                )
                .clicked()
            {
                self.assign(true);
            }
        });
    }
    fn upgrade_cards(&mut self, ui: &mut egui::Ui) {
        let devices = self.snapshot.devices.clone();
        egui::ScrollArea::vertical()
            .id_salt("compact_upgrade_jobs")
            .show_rows(ui, 196.0, devices.len(), |ui, range| {
                for index in range {
                    let d = &devices[index];
                    ui.scope_builder(
                        egui::UiBuilder::new().id(egui::Id::new(("upgrade_card", d.id))),
                        |ui| {
                            let ready = d.state == SwitchState::Ready
                                && !self.app.as_ref().is_some_and(|a| a.engine().busy(d.id));
                            let info = d.facts.version.clone().unwrap_or_default();
                            let reason = if !ready {
                                Some("Device is busy or disconnected".to_string())
                            } else {
                                upgrade::install_blocker(&d.upgrade, &info)
                            };
                            ui.horizontal(|ui| {
                                let r = ui.add(
                                    egui::Button::selectable(self.device == Some(d.id), &d.name)
                                        .frame(false),
                                );
                                self.list_widgets.insert(r.id);
                                if r.has_focus() {
                                    self.focus = 0;
                                }
                                if r.clicked() {
                                    self.choose_device(d.id);
                                }
                                let mut include = self.upgrade_selected.contains(&d.id);
                                if ui.checkbox(&mut include, "Include").changed() {
                                    if include {
                                        self.upgrade_selected.insert(d.id);
                                    } else {
                                        self.upgrade_selected.remove(&d.id);
                                    }
                                }
                                design::badge(ui, d.state.label(), state_color(&d.state));
                                ui.menu_button("Actions", |ui| {
                                    if ui
                                        .add_enabled(
                                            self.image.is_some() && ready,
                                            egui::Button::new("Assign selected image"),
                                        )
                                        .clicked()
                                    {
                                        if let Some(local) = self.image.clone() {
                                            self.send(Command::AssignImage {
                                                devices: vec![d.id],
                                                local,
                                            });
                                        }
                                        ui.close();
                                    }
                                    if ui
                                        .add_enabled(
                                            reason.is_none(),
                                            egui::Button::new("YOLO · automatic reload…"),
                                        )
                                        .on_disabled_hover_text(reason.as_deref().unwrap_or(""))
                                        .clicked()
                                    {
                                        self.request_install(d.id, true);
                                        ui.close();
                                    }
                                });
                            });
                            ui.add(
                                egui::Label::new(format!(
                                    "{} · IOS {} · {} free",
                                    info.model.as_deref().unwrap_or("Unknown model"),
                                    info.version.as_deref().unwrap_or("—"),
                                    d.facts
                                        .flash
                                        .map(|f| fmt_bytes(f.free))
                                        .unwrap_or_else(|| "—".into())
                                ))
                                .truncate(),
                            );
                            ui.add(
                                egui::Label::new(
                                    d.assigned
                                        .as_ref()
                                        .map(|(p, _)| p.as_str())
                                        .unwrap_or("No image assigned"),
                                )
                                .truncate(),
                            );
                            if let Some(stats) =
                                d.transfer.as_ref().and_then(|t| t.session.as_ref())
                            {
                                ui.add(
                                    egui::Label::new(format!(
                                        "{} · {:.0}% · {} · ETA {}",
                                        d.upgrade.label(),
                                        stats.progress().unwrap_or(0.0) * 100.0,
                                        fmt_speed(stats.current_speed, false),
                                        stats.eta().map(fmt_duration).unwrap_or_else(|| "—".into())
                                    ))
                                    .truncate(),
                                );
                            } else {
                                ui.label(d.upgrade.label());
                            }

                            ui.horizontal(|ui| {
                                if ui
                                    .add_enabled(
                                        ready && d.assigned.is_some(),
                                        egui::Button::new("Upload & verify"),
                                    )
                                    .clicked()
                                {
                                    self.send(Command::Deploy(d.id, self.upgrade_protocol));
                                }
                                if ui
                                    .add_enabled(
                                        ready && d.assigned.is_some(),
                                        egui::Button::new("Verify"),
                                    )
                                    .clicked()
                                {
                                    self.send(Command::Verify(d.id));
                                }
                                if ui
                                    .add_enabled(reason.is_none(), egui::Button::new("Upgrade…"))
                                    .on_disabled_hover_text(reason.as_deref().unwrap_or(""))
                                    .clicked()
                                {
                                    self.request_install(d.id, false);
                                }
                                if ui
                                    .add_enabled(ready, egui::Button::new("Remove inactive"))
                                    .clicked()
                                {
                                    self.send(Command::RemoveInactive(d.id));
                                }
                            });
                            ui.separator();
                        },
                    );
                }
            });
    }
    pub(super) fn upgrades(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            design::heading(ui, "UPGRADE", "INSTALL mode · verified images only");
            ui.label("Protocol");
            let options = self
                .app
                .as_ref()
                .map(|a| engine::protocol_options(a, false))
                .unwrap_or_default();
            egui::ComboBox::from_id_salt("upgrade_protocol")
                .selected_text(self.upgrade_protocol.label())
                .show_ui(ui, |ui| {
                    for p in options {
                        ui.selectable_value(&mut self.upgrade_protocol, p, p.label());
                    }
                });
        });
        if ui.available_height() < 390.0 {
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.upgrade_images_open, "Images")
                    .clicked()
                {
                    self.upgrade_images_open = true;
                    self.focus = 1;
                }
                if ui
                    .selectable_label(!self.upgrade_images_open, "Device jobs")
                    .clicked()
                {
                    self.upgrade_images_open = false;
                    self.focus = 0;
                }
            });
            if self.upgrade_images_open {
                self.browser(ui, true);
            } else {
                self.upgrade_assignments(ui);
                self.upgrade_cards(ui);
            }
            return;
        }
        ui.add_space(8.0);
        if ui
            .button(if self.upgrade_images_open {
                "Hide image browser"
            } else {
                "Show image browser"
            })
            .clicked()
        {
            self.upgrade_images_open = !self.upgrade_images_open;
        }
        if self.upgrade_images_open {
            let max = (ui.available_height() * 0.43).max(220.0);
            let files = egui::Panel::top("upgrade_files")
                .resizable(true)
                .default_size(self.prefs.files_height.min(max))
                .size_range(220.0..=max)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    ui.set_min_height(ui.available_height());
                    self.browser(ui, true);
                });
            self.prefs.files_height = files.response.rect.height();
        }
        ui.add_space(10.0);
        self.upgrade_assignments(ui);
        ui.add_space(8.0);
        let devices = self.snapshot.devices.clone();
        if devices.is_empty() {
            design::empty(
                ui,
                "No upgrade jobs yet",
                "Connect devices in 2 Connect, then assign an image here.",
            );
            return;
        }
        let wide = ui.available_width() > 1100.0;
        let widths = if wide {
            vec![170.0, 220.0, 105.0, 160.0, -1.0, 150.0]
        } else {
            vec![145.0, 175.0, 145.0, -1.0, 130.0]
        };
        let h = design::row_height(ui) * 4.2;
        let max_height = ui.available_height().max(100.0);
        let mut job_table = table(ui, "upgrade_jobs", &widths).max_scroll_height(max_height);
        if self.scroll_device {
            self.scroll_device = false;
            if let Some(index) = devices.iter().position(|d| Some(d.id) == self.device) {
                job_table = job_table.scroll_to_row(index, Some(egui::Align::Center));
            }
        }
        job_table
            .header(32.0, |r| {
                headers(
                    r,
                    if wide {
                        &[
                            "Device / model",
                            "Image / version",
                            "Free flash",
                            "Upload / speed",
                            "Workflow",
                            "Actions",
                        ]
                    } else {
                        &[
                            "Device / model",
                            "Image / version",
                            "Upload / speed",
                            "Workflow",
                            "Actions",
                        ]
                    },
                )
            })
            .body(|mut body| {
                remember_columns(&mut body, "upgrade_jobs");
                body.rows(h, devices.len(), |mut row| {
                    let d = &devices[row.index()];
                    let info = d.facts.version.clone().unwrap_or_default();
                    let low = d
                        .assigned
                        .as_ref()
                        .zip(d.facts.flash)
                        .is_some_and(|((_, size), flash)| !flash.fits(*size));
                    row.set_selected(self.device == Some(d.id));
                    row.col(|ui| {
                        let mut selected = self.upgrade_selected.contains(&d.id);
                        let include = ui
                            .push_id(("include_upgrade", d.id), |ui| {
                                ui.checkbox(&mut selected, "Include")
                            })
                            .inner
                            .on_hover_text(format!("Include {} in image assignment", d.name));
                        if include.changed() {
                            if selected {
                                self.upgrade_selected.insert(d.id);
                            } else {
                                self.upgrade_selected.remove(&d.id);
                            }
                        }
                        let response = ui
                            .scope_builder(
                                egui::UiBuilder::new().id(egui::Id::new(("device_name", d.id))),
                                |ui| {
                                    ui.add(
                                        egui::Button::new(&d.name)
                                            .frame(false)
                                            .selected(self.device == Some(d.id))
                                            .truncate(),
                                    )
                                },
                            )
                            .inner
                            .on_hover_text(&d.host);
                        self.list_widgets.insert(response.id);
                        if response.has_focus() {
                            self.focus = 0;
                        }
                        if response.clicked() {
                            self.choose_device(d.id);
                        }
                        design::label(ui, info.model.clone().unwrap_or_else(|| "—".into()));
                    });
                    row.col(|ui| {
                        design::label(
                            ui,
                            d.assigned
                                .as_ref()
                                .map(|(p, _)| p.as_str())
                                .unwrap_or("No image assigned"),
                        )
                        .on_hover_text(
                            d.assigned
                                .as_ref()
                                .map(|(p, _)| p.as_str())
                                .unwrap_or("Choose an image above"),
                        );
                        if let Some(target) = d
                            .assigned
                            .as_ref()
                            .and_then(|(path, _)| upgrade::image_version(path))
                        {
                            let old = info
                                .version
                                .as_deref()
                                .map(upgrade::normalize_version)
                                .unwrap_or_else(|| "?".into());
                            let color = if old == target {
                                GREEN
                            } else if release_order(&target) < release_order(&old) {
                                RED
                            } else {
                                AMBER
                            };
                            ui.label(
                                RichText::new(format!("{old} → {target}"))
                                    .strong()
                                    .color(design::semantic(ui, color)),
                            );
                        } else {
                            ui.label(
                                RichText::new(format!(
                                    "IOS {}",
                                    info.version.as_deref().unwrap_or("—")
                                ))
                                .small()
                                .color(design::muted(ui)),
                            );
                        }
                        if !wide {
                            ui.label(
                                RichText::new(format!(
                                    "Flash {}",
                                    d.facts
                                        .flash
                                        .map(|f| fmt_bytes(f.free))
                                        .unwrap_or_else(|| "—".into())
                                ))
                                .small()
                                .color(if low {
                                    design::semantic(ui, RED)
                                } else {
                                    design::muted(ui)
                                }),
                            );
                        }
                    });
                    if wide {
                        row.col(|ui| {
                            ui.colored_label(
                                if low {
                                    design::semantic(ui, RED)
                                } else {
                                    ui.visuals().text_color()
                                },
                                d.facts
                                    .flash
                                    .map(|f| fmt_bytes(f.free))
                                    .unwrap_or_else(|| "—".into()),
                            );
                            if low {
                                ui.label(
                                    RichText::new("SPACE LOW")
                                        .small()
                                        .color(design::semantic(ui, RED)),
                                );
                            }
                        });
                    }
                    row.col(|ui| {
                        let session = d.transfer.as_ref().and_then(|t| t.session.as_ref());
                        let uploaded = matches!(
                            d.upgrade,
                            upgrade::Progress::Verified { .. }
                                | upgrade::Progress::Installing
                                | upgrade::Progress::AwaitingReload { .. }
                                | upgrade::Progress::Rebooting { .. }
                                | upgrade::Progress::Complete { .. }
                        );
                        design::progress(
                            ui,
                            if uploaded {
                                Some(1.0)
                            } else {
                                session.and_then(|s| s.progress())
                            },
                            GREEN,
                        );
                        ui.label(
                            RichText::new(
                                session
                                    .map(|s| {
                                        format!(
                                            "{} · ETA {}",
                                            fmt_speed(
                                                s.current_speed,
                                                self.app
                                                    .as_ref()
                                                    .unwrap()
                                                    .config
                                                    .read()
                                                    .unwrap()
                                                    .speed_in_bits
                                            ),
                                            s.eta().map(fmt_duration).unwrap_or_else(|| "—".into())
                                        )
                                    })
                                    .unwrap_or_else(|| "— · ETA —".into()),
                            )
                            .small()
                            .color(design::muted(ui)),
                        );
                    });
                    row.col(|ui| {
                        let phase = match d.upgrade {
                            upgrade::Progress::Idle => 0,
                            upgrade::Progress::Copying => 1,
                            upgrade::Progress::Verifying => 2,
                            upgrade::Progress::Verified { .. } => 3,
                            upgrade::Progress::Installing
                            | upgrade::Progress::AwaitingReload { .. } => 4,
                            upgrade::Progress::Rebooting { .. } => 5,
                            upgrade::Progress::Complete { .. } => 6,
                            upgrade::Progress::Failed(_) => 0,
                        };
                        ui.horizontal_wrapped(|ui| {
                            for (i, name) in ["Upload", "Verify", "Install", "Restart", "Check"]
                                .iter()
                                .enumerate()
                            {
                                ui.label(RichText::new(*name).small().color(if phase > i + 1 {
                                    design::semantic(ui, GREEN)
                                } else if phase == i + 1 {
                                    design::semantic(ui, CYAN)
                                } else {
                                    design::muted(ui)
                                }));
                            }
                        });
                        let label = d.upgrade.label();
                        ui.add(
                            egui::Label::new(
                                RichText::new(&label)
                                    .color(design::semantic(ui, reboot_color(&d.upgrade))),
                            )
                            .truncate(),
                        )
                        .on_hover_text(&label);
                        if let Some(reason) =
                            upgrade::install_blocker(&d.upgrade, &info).filter(|_| {
                                matches!(
                                    d.upgrade,
                                    upgrade::Progress::Idle | upgrade::Progress::Verified { .. }
                                )
                            })
                        {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&reason).small().color(design::muted(ui)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&reason);
                        }
                        if let upgrade::Progress::Rebooting {
                            since, last_error, ..
                        } = &d.upgrade
                        {
                            if since.elapsed().as_secs() >= 600 {
                                ui.label(
                                    RichText::new("Reboot taking longer")
                                        .small()
                                        .color(design::semantic(ui, RED)),
                                );
                            }
                            if let Some(error) = last_error {
                                ui.add(egui::Label::new(RichText::new(error).small()).truncate())
                                    .on_hover_text(error);
                            }
                        }
                        if let Some(warning) = d
                            .assigned
                            .as_ref()
                            .and_then(|(p, _)| cisco::platform_warning(&info, p))
                        {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&warning).small().color(design::accent(ui)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&warning);
                        }
                    });
                    row.col(|ui| {
                        let blocker = upgrade::install_blocker(&d.upgrade, &info);
                        let ready = d.state == SwitchState::Ready
                            && !self.app.as_ref().is_some_and(|a| a.engine().busy(d.id));
                        let allowed = ready && blocker.is_none();
                        if d.assigned.is_some()
                            && matches!(
                                d.upgrade,
                                upgrade::Progress::Idle | upgrade::Progress::Failed(_)
                            )
                            && ui
                                .add_enabled(
                                    ready,
                                    egui::Button::new(
                                        RichText::new("Upload & verify").color(design::accent(ui)),
                                    ),
                                )
                                .clicked()
                        {
                            self.send(Command::Deploy(d.id, self.upgrade_protocol));
                        }
                        // Prioritize cleanup when space is low; the workflow
                        // column already explains why INSTALL is unavailable.
                        if !low || allowed {
                            let button = ui.add_enabled(
                                allowed,
                                egui::Button::new(
                                    RichText::new("Upgrade…").color(design::accent(ui)),
                                ),
                            );
                            if let Some(reason) = &blocker {
                                button.clone().on_disabled_hover_text(reason);
                            }
                            if button.clicked() {
                                self.request_install(d.id, false);
                            }
                        }
                        if low
                            && ui
                                .add_enabled(
                                    ready,
                                    egui::Button::new(
                                        RichText::new("Remove inactive")
                                            .color(design::semantic(ui, RED)),
                                    ),
                                )
                                .clicked()
                        {
                            self.send(Command::RemoveInactive(d.id));
                        }
                        ui.menu_button("Job actions", |ui| {
                            if ui.button("Assign selected image").clicked() {
                                if let Some(local) = self.image.clone() {
                                    self.send(Command::AssignImage {
                                        devices: vec![d.id],
                                        local,
                                    });
                                }
                                ui.close();
                            }
                            if ui
                                .add_enabled(ready, egui::Button::new("Upload & verify"))
                                .clicked()
                            {
                                self.send(Command::Deploy(d.id, self.upgrade_protocol));
                                ui.close();
                            }
                            if ui
                                .add_enabled(ready, egui::Button::new("Verify existing"))
                                .clicked()
                            {
                                self.send(Command::Verify(d.id));
                                ui.close();
                            }
                            if ui
                                .add_enabled(ready, egui::Button::new("Remove inactive"))
                                .clicked()
                            {
                                self.send(Command::RemoveInactive(d.id));
                                ui.close();
                            }
                            ui.separator();
                            if ui
                                .add_enabled(
                                    allowed,
                                    egui::Button::new(
                                        RichText::new("YOLO upgrade…")
                                            .color(design::semantic(ui, RED)),
                                    ),
                                )
                                .clicked()
                            {
                                self.request_install(d.id, true);
                                ui.close();
                            }
                            if ui.button("Console").clicked() {
                                self.console = Some(d.id);
                                ui.close();
                            }
                        });
                    });
                })
            });
    }
    pub(super) fn logs(&mut self, ui: &mut egui::Ui) {
        design::heading(ui, "LOGS", "Live diagnostic stream");
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.log_filter)
                    .id(egui::Id::new("log_search"))
                    .hint_text("Search messages / device…")
                    .desired_width(260.0),
            );
            if !ui.ctx().text_edit_focused() && ui.input(|i| i.key_pressed(egui::Key::Slash)) {
                search.request_focus();
            }
            egui::ComboBox::from_id_salt("log_level")
                .selected_text(["Debug", "Info", "Warning", "Error"][self.log_level])
                .show_ui(ui, |ui| {
                    for (i, label) in ["Debug", "Info", "Warning", "Error"].iter().enumerate() {
                        ui.selectable_value(&mut self.log_level, i, *label);
                    }
                });
            ui.add(
                egui::TextEdit::singleline(&mut self.log_protocol)
                    .hint_text("Protocol / subsystem…")
                    .desired_width(120.0),
            );
            ui.checkbox(&mut self.log_follow, "Follow");
        });
        let revision = self.app.as_ref().map(|a| a.logger.revision()).unwrap_or(0);
        let key = (
            revision,
            self.log_filter.clone(),
            self.log_level,
            self.log_protocol.clone(),
        );
        if self.log_cache_key.as_ref() != Some(&key) {
            let entries = self
                .app
                .as_ref()
                .map(|a| a.logger.entries())
                .unwrap_or_default();
            self.log_total = entries.len();
            let levels = transferbuddy_core::logging::LogLevel::ALL;
            let query = self.log_filter.to_lowercase();
            self.log_cache = std::sync::Arc::new(
                entries
                    .into_iter()
                    .filter(|e| {
                        e.level >= levels[self.log_level]
                            && (query.is_empty() || e.render_line().to_lowercase().contains(&query))
                            && (self.log_protocol.is_empty()
                                || e.proto
                                    .to_lowercase()
                                    .contains(&self.log_protocol.to_lowercase())
                                || log_protocol_label(&e.proto)
                                    .to_lowercase()
                                    .contains(&self.log_protocol.to_lowercase()))
                    })
                    .collect(),
            );
            self.log_cache_key = Some(key);
        }
        let filtered = self.log_cache.clone();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("{} entries", filtered.len()))
                    .small()
                    .color(design::muted(ui)),
            );
            if ui.button("Copy logs").clicked() {
                ui.ctx().copy_text(
                    filtered
                        .iter()
                        .map(|e| e.render_line())
                        .collect::<Vec<_>>()
                        .join("\n"),
                );
            }
            if !self.log_follow
                && self.log_total > self.log_seen
                && ui.button("New entries ↓").clicked()
            {
                self.log_follow = true;
            }
        });
        if self.log_follow {
            self.log_seen = self.log_total;
        }
        if let Some(detail) = self.log_detail.clone() {
            egui::Panel::bottom("log_detail")
                .resizable(true)
                .default_size(150.0)
                .size_range(100.0..=260.0)
                .frame(egui::Frame::NONE)
                .show(ui, |ui| {
                    design::panel(ui, "ENTRY DETAILS", false, |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("Copy entry").clicked() {
                                ui.ctx().copy_text(detail.clone());
                            }
                            if ui.button("Close details").clicked() {
                                self.log_detail = None;
                            }
                        });
                        egui::ScrollArea::vertical().show(ui, |ui| {
                            design::copy_command(ui, &detail);
                        });
                    });
                });
        }
        if filtered.is_empty() {
            design::empty(
                ui,
                "No matching log entries",
                "Messages appear here as services, connections and jobs run.",
            );
            return;
        }
        let max = ui.available_height();
        let height = design::row_height(ui) * 1.8;
        let output = table_area(ui, "log_table_area", |ui| {
            table(
                ui,
                "log_entries",
                &[120.0, 100.0, 150.0, 165.0, 100.0, -1.0],
            )
            .stick_to_bottom(self.log_follow)
            .max_scroll_height(max)
            .header(28.0, |r| {
                headers(
                    r,
                    &[
                        "Time",
                        "Level",
                        "Hostname / IP",
                        "Model",
                        "Protocol",
                        "Message",
                    ],
                )
            })
            .body(|mut body| {
                remember_columns(&mut body, "log_entries");
                body.rows(height, filtered.len(), |mut row| {
                    let entry = &filtered[row.index()];
                    row.col(|ui| {
                        ui.label(entry.time.format("%H:%M:%S%.3f").to_string());
                    });
                    row.col(|ui| {
                        design::badge(
                            ui,
                            entry.level.label(),
                            match entry.level {
                                transferbuddy_core::logging::LogLevel::Error => RED,
                                transferbuddy_core::logging::LogLevel::Warning => AMBER,
                                transferbuddy_core::logging::LogLevel::Debug => design::muted(ui),
                                _ => CYAN,
                            },
                        );
                    });
                    let device = self
                        .snapshot
                        .devices
                        .iter()
                        .find(|d| entry.source_ip.is_some_and(|ip| d.host == ip.to_string()));
                    row.col(|ui| {
                        design::label(
                            ui,
                            entry
                                .hostname
                                .as_deref()
                                .or_else(|| device.map(|d| d.name.as_str()))
                                .unwrap_or("—"),
                        );
                        ui.label(
                            RichText::new(
                                entry.source_ip.map(|ip| ip.to_string()).unwrap_or_default(),
                            )
                            .small()
                            .color(design::muted(ui)),
                        );
                    });
                    row.col(|ui| {
                        design::label(
                            ui,
                            entry
                                .model
                                .as_deref()
                                .or_else(|| {
                                    device.and_then(|d| d.facts.version.as_ref()?.model.as_deref())
                                })
                                .unwrap_or("—"),
                        );
                    });
                    row.col(|ui| {
                        design::badge(ui, &log_protocol_label(&entry.proto), CYAN);
                    });
                    row.col(|ui| {
                        let full = entry.render_line();
                        let response = if let Some(command) = &entry.command {
                            design::copy_command(ui, command)
                        } else {
                            design::label(ui, &entry.action)
                        }
                        .on_hover_text(&full);
                        response.context_menu(|ui| {
                            if ui.button("Copy entry").clicked() {
                                ui.ctx().copy_text(full.clone());
                                ui.close();
                            }
                        });
                        if response.clicked() {
                            self.log_detail = Some(full);
                        }
                    });
                })
            })
        });
        if ui.input(|i| i.smooth_scroll_delta.y > 0.0)
            && output
                .inner_rect
                .contains(ui.input(|i| i.pointer.hover_pos().unwrap_or_default()))
        {
            self.log_follow = false;
        }
    }
}
