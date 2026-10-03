use super::*;
use std::collections::BTreeSet;

#[derive(Default, Clone)]
pub(super) struct DeviceWorkspace {
    local_dir: String,
    remote_dir: String,
    local_selection: Option<FileEntry>,
    remote_selection: Option<cisco::RemoteFile>,
    local_selected: BTreeSet<String>,
    remote_selected: BTreeSet<String>,
    filter: String,
    remote_filter: String,
}
#[derive(Clone)]
pub(super) struct FileDrag {
    pub receive: bool,
    pub device: Option<u64>,
    pub directory: String,
    pub names: Vec<String>,
}
impl Desktop {
    pub(super) fn capture_workspace(&self) -> DeviceWorkspace {
        DeviceWorkspace {
            local_dir: self.local_dir.clone(),
            remote_dir: self.remote_dir.clone(),
            local_selection: self.local_selection.clone(),
            remote_selection: self.remote_selection.clone(),
            local_selected: self.local_selected.clone(),
            remote_selected: self.remote_selected.clone(),
            filter: self.filter.clone(),
            remote_filter: self.remote_filter.clone(),
        }
    }
    pub(super) fn restore_workspace(&mut self, device: u64) {
        let state = self
            .device_workspaces
            .get(&device)
            .cloned()
            .unwrap_or_default();
        self.local_dir = state.local_dir;
        self.remote_dir = state.remote_dir;
        self.local_selection = state.local_selection;
        self.remote_selection = state.remote_selection;
        self.local_selected = state.local_selected;
        self.remote_selected = state.remote_selected;
        self.filter = state.filter;
        self.remote_filter = state.remote_filter;
        self.local_anchor = None;
        self.remote_anchor = None;
        self.refresh_local();
    }
    pub(super) fn select_file(
        &mut self,
        receive: bool,
        name: &str,
        ordered: &[String],
        modifiers: egui::Modifiers,
    ) {
        let (selection, anchor) = if receive {
            (&mut self.remote_selected, &mut self.remote_anchor)
        } else {
            (&mut self.local_selected, &mut self.local_anchor)
        };
        if modifiers.shift {
            let from = anchor
                .as_ref()
                .and_then(|a| ordered.iter().position(|n| n == a));
            let to = ordered.iter().position(|n| n == name);
            if let (Some(from), Some(to)) = (from, to) {
                if !modifiers.command {
                    selection.clear();
                }
                selection.extend(ordered[from.min(to)..=from.max(to)].iter().cloned());
            }
        } else if modifiers.command {
            if !selection.remove(name) {
                selection.insert(name.into());
            }
            *anchor = Some(name.into());
        } else {
            selection.clear();
            selection.insert(name.into());
            *anchor = Some(name.into());
        }
        self.focus = if receive { 2 } else { 1 };
    }
    pub(super) fn selected_file_names(&self, receive: bool) -> Vec<String> {
        if receive {
            let entries = self
                .selected()
                .and_then(|d| {
                    self.app
                        .as_ref()?
                        .engine()
                        .device(d.id)
                        .ok()?
                        .listing(&self.remote_path(""))
                })
                .and_then(Result::ok)
                .unwrap_or_default();
            if self.remote_selected.is_empty() {
                return Vec::new();
            }
            entries
                .iter()
                .filter(|f| !f.is_dir && self.remote_selected.contains(&f.name))
                .map(|f| f.name.clone())
                .collect()
        } else {
            let entries = self
                .snapshot
                .listings
                .get(&self.local_dir)
                .and_then(|e| e.as_ref().ok());
            if self.local_selected.is_empty() {
                return Vec::new();
            }
            entries
                .into_iter()
                .flatten()
                .filter(|f| !f.is_dir && self.local_selected.contains(&f.name))
                .map(|f| f.name.clone())
                .collect()
        }
    }
    pub(super) fn drop_files(&mut self, payload: &FileDrag, target_remote: bool) {
        if payload.receive == target_remote || payload.names.is_empty() {
            return;
        }
        let Some(device) = self.selected() else {
            self.status = "Connect a device first".into();
            return;
        };
        if payload.receive && payload.device != Some(device.id) {
            self.status = "Select the source device before receiving its files".into();
            return;
        }
        self.pending_transfers = Some(
            payload
                .names
                .iter()
                .map(|name| {
                    let source = if payload.directory.is_empty() {
                        name.clone()
                    } else {
                        format!("{}/{name}", payload.directory.trim_end_matches('/'))
                    };
                    engine::TransferRequest {
                        device: device.id,
                        local: if payload.receive {
                            self.local_path(name)
                        } else {
                            source.clone()
                        },
                        remote: if payload.receive {
                            if payload.directory.is_empty() {
                                self.remote_path(name)
                            } else {
                                format!("{}/{name}", payload.directory.trim_end_matches('/'))
                            }
                        } else {
                            self.remote_path(name)
                        },
                        receive: payload.receive,
                        protocol: if payload.receive {
                            Protocol::Ftp
                        } else {
                            device.protocol
                        },
                        overwrite: false,
                        platform_check: false,
                    }
                })
                .collect(),
        );
        self.transfer(payload.receive);
    }
    pub(super) fn transfer_actions(&mut self, ui: &mut egui::Ui) {
        let receive = self.focus == 2;
        let count = self.selected_file_names(receive).len();
        let device = self.selected();
        {
            let choice = self
                .app
                .as_ref()
                .map(|a| {
                    let cfg = a.config.read().unwrap();
                    let protocol = if receive {
                        Protocol::Ftp
                    } else {
                        device
                            .as_ref()
                            .map(|d| d.protocol)
                            .unwrap_or(Protocol::Sftp)
                    };
                    let bind = &cfg.service(cisco::service_of(protocol)).bind;
                    if bind
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| !ip.is_unspecified())
                    {
                        format!("{bind} · fixed {} service bind", protocol.label())
                    } else {
                        let selection = cfg.advertise.as_deref();
                        let ip = if let Some(value) = selection {
                            self.snapshot
                                .network
                                .interfaces
                                .iter()
                                .find(|i| i.name == value || i.ip.to_string() == value)
                                .map(|i| i.ip)
                        } else {
                            device
                                .as_ref()
                                .and_then(|d| self.snapshot.network.routes.get(&d.id))
                                .copied()
                                .flatten()
                        };
                        format!(
                            "{} · {}",
                            selection.unwrap_or("Automatic"),
                            ip.map(|ip| ip.to_string())
                                .unwrap_or_else(|| if selection.is_some() {
                                    "unavailable".into()
                                } else {
                                    "resolving route to device".into()
                                })
                        )
                    }
                })
                .unwrap_or_else(|| "Automatic · route to device".into());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Copy URL interface: {choice}"))
                        .small()
                        .color(design::accent(ui)),
                );
                if ui.small_button("Change interface").clicked() {
                    self.tab = Tab::Dashboard;
                }
            });
        }

        if ui.available_width() < 700.0 {
            ui.horizontal(|ui| {
                ui.label(if receive {
                    "Remote → local"
                } else {
                    "Local → device"
                });
                let enabled = count > 0 && device.as_ref().is_some_and(|d| !d.state.is_over());
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(format!(
                            "Copy {count} file{}",
                            if count == 1 { "" } else { "s" }
                        )),
                    )
                    .on_hover_text(format!(
                        "Destination: {} · Cmd/Ctrl+Enter",
                        if receive {
                            self.local_dir.clone()
                        } else {
                            self.remote_path("")
                        }
                    ))
                    .clicked()
                {
                    self.transfer(receive);
                }
            });
            return;
        }
        ui.horizontal_wrapped(|ui| {
            ui.label(if receive {
                "REMOTE → LOCAL"
            } else {
                "LOCAL → REMOTE"
            });
            if let Some(d) = &device {
                design::label(
                    ui,
                    format!(
                        "{} · {} · {}",
                        d.name,
                        self.remote_path(""),
                        if receive { "FTP" } else { d.protocol.label() }
                    ),
                );
            }
            let label = if receive {
                format!(
                    "← Copy {count} file{} to local",
                    if count == 1 { "" } else { "s" }
                )
            } else {
                format!(
                    "Copy {count} file{} to device →",
                    if count == 1 { "" } else { "s" }
                )
            };
            let can_copy = count > 0 && device.as_ref().is_some_and(|d| !d.state.is_over());
            ui.add_enabled_ui(can_copy, |ui| {
                if design::primary(ui, &label)
                    .on_hover_text("Copy selected files · Cmd/Ctrl+Enter")
                    .clicked()
                {
                    self.transfer(receive);
                }
            });
            if !can_copy {
                ui.label(
                    RichText::new("Select files and a connected device")
                        .small()
                        .color(design::muted(ui)),
                );
            }
        });
    }
    pub(super) fn jobs_panel(&mut self, ui: &mut egui::Ui) {
        let operations: Vec<_> = self
            .snapshot
            .operations
            .iter()
            .filter(|o| o.transfer.is_some())
            .cloned()
            .collect();
        ui.horizontal(|ui| {
            design::heading(
                ui,
                "JOBS",
                &format!(
                    "{} active · session only",
                    operations.iter().filter(|o| o.state.is_active()).count()
                ),
            );
            if ui.small_button("Hide jobs").clicked() {
                self.jobs_open = false;
            }
        });
        if operations.is_empty() {
            ui.label("Copy files or upload an upgrade image to create jobs.");
            return;
        }
        if let Some(op) = operations.iter().find(|o| Some(o.id) == self.job_selected) {
            let transfer = op.transfer.as_ref().unwrap();
            ui.horizontal(|ui| {
                ui.label(format!("{} · {}", op.label, transfer.protocol.label()));
                let path = if transfer.receive {
                    format!("{} → {}", transfer.remote, transfer.local)
                } else {
                    format!("{} → {}", transfer.local, transfer.remote)
                };
                design::label(ui, &path).on_hover_text(path);
            });
            if let Some(command) = &transfer.command {
                if ui.available_height() < 240.0 {
                    let address = design::copy_address_range(command)
                        .map(|range| &command[range])
                        .unwrap_or("unknown");
                    ui.collapsing(
                        RichText::new(format!("Copy command · server IP {address}"))
                            .color(design::semantic(ui, AMBER)),
                        |ui| {
                            design::copy_command(ui, command);
                        },
                    );
                } else {
                    design::copy_command(ui, command);
                }
            }
            if let Some(inspection) = &transfer.inspection {
                ui.colored_label(
                    design::semantic(
                        ui,
                        if *inspection == engine::TransferInspection::Verified {
                            GREEN
                        } else {
                            AMBER
                        },
                    ),
                    inspection.label(),
                );
            }
            if let Some(error) = op.state.error() {
                ui.add(
                    egui::Label::new(RichText::new(error).color(design::semantic(ui, RED)))
                        .truncate(),
                )
                .on_hover_text(error);
                let failure = engine::Failure::from_message(error);
                ui.add(egui::Label::new(RichText::new(failure.remedy).small()).truncate())
                    .on_hover_text(failure.remedy);
                ui.horizontal_wrapped(|ui| {
                    let ready = self
                        .snapshot
                        .devices
                        .iter()
                        .any(|d| Some(d.id) == op.device && d.state == SwitchState::Ready);
                    if ui
                        .add_enabled(ready, egui::Button::new("Inspect destination"))
                        .on_disabled_hover_text("Wait for SSH reconnect first")
                        .clicked()
                    {
                        self.send(Command::InspectOperation(op.id));
                    }
                    if ui.button("Choose interface & retry…").clicked() {
                        self.open_retry_interface(op.id);
                    }
                    if ui
                        .add_enabled(ready, egui::Button::new("Overwrite & retry…"))
                        .clicked()
                    {
                        self.send(Command::RequestOverwriteRetry(op.id));
                    }
                });
            }
        }
        let paused: Vec<_> = operations
            .iter()
            .filter(|o| o.state == engine::OperationState::Paused)
            .filter_map(|o| o.device)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if !paused.is_empty() {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Review pending jobs").clicked() {
                    self.send(Command::ReviewPending(paused.clone()));
                }
                let reviewed = operations
                    .iter()
                    .filter(|o| o.state == engine::OperationState::Paused)
                    .all(|o| {
                        self.snapshot.queue_reviews.iter().any(|r| {
                            r.operation == o.id
                                && r.ready
                                && r.revision == self.snapshot.network.revision
                        })
                    });
                if ui
                    .add_enabled(reviewed, egui::Button::new("Resume checked jobs"))
                    .on_disabled_hover_text(
                        "Review pending jobs after reconnect and the latest network change",
                    )
                    .clicked()
                {
                    self.send(Command::ResumeReviewed(paused.clone()));
                }
            });
        }
        for review in &self.snapshot.queue_reviews {
            ui.add(
                egui::Label::new(format!("#{} · {}", review.operation, review.message)).truncate(),
            );
        }
        let height = design::row_height(ui) * 1.8;
        let narrow = ui.available_width() < 850.0;
        let max_height = ui.available_height().max(0.0);
        let mut table = egui_extras::TableBuilder::new(ui)
            .id_salt("jobs_list")
            .striped(true)
            .column(egui_extras::Column::remainder().at_least(90.0).clip(true))
            .column(egui_extras::Column::exact(if narrow {
                130.0
            } else {
                185.0
            }))
            .column(egui_extras::Column::exact(if narrow {
                140.0
            } else {
                230.0
            }))
            .column(egui_extras::Column::exact(180.0))
            .min_scrolled_height(0.0)
            .max_scroll_height(max_height)
            .auto_shrink([false, false]);
        if self.jobs_last_selection != self.job_selected {
            self.jobs_last_selection = self.job_selected;
            if let Some(index) = operations
                .iter()
                .position(|o| Some(o.id) == self.job_selected)
            {
                table = table.scroll_to_row(index, Some(egui::Align::Center));
            }
        }
        table.body(|body| {
            body.rows(height, operations.len(), |mut row| {
                let op = &operations[row.index()];
                let transfer = op.transfer.as_ref().unwrap();
                row.set_selected(self.job_selected == Some(op.id));
                row.col(|ui| {
                    ui.push_id(op.id, |ui| {
                        let device = self
                            .snapshot
                            .devices
                            .iter()
                            .find(|d| Some(d.id) == op.device)
                            .map(|d| d.name.as_str())
                            .unwrap_or("Removed device");
                        let name = transfer.local.rsplit('/').next().unwrap_or(&transfer.local);
                        if ui
                            .add(
                                egui::Button::selectable(
                                    self.job_selected == Some(op.id),
                                    format!("#{:03} {device} · {name}", op.id),
                                )
                                .frame(false)
                                .truncate(),
                            )
                            .on_hover_text(format!("{} → {}", transfer.local, transfer.remote))
                            .clicked()
                        {
                            self.job_selected = Some(op.id);
                        }
                    });
                });
                row.col(|ui| {
                    ui.add(egui::Label::new(op.state.label()).truncate());
                });
                row.col(|ui| {
                    if matches!(
                        op.state,
                        engine::OperationState::Running | engine::OperationState::Stopping
                    ) {
                        let progress = if transfer.size > 0 {
                            Some((transfer.bytes as f64 / transfer.size as f64).min(1.0))
                        } else {
                            None
                        };
                        let text = format!(
                            "{:.0}% · {} · ETA {}",
                            progress.unwrap_or(0.0) * 100.0,
                            fmt_speed(transfer.speed, false),
                            transfer.eta.map(fmt_duration).unwrap_or_else(|| "—".into())
                        );
                        ui.add(
                            egui::ProgressBar::new(progress.unwrap_or(0.0) as f32)
                                .desired_width(ui.available_width())
                                .text(if narrow {
                                    format!(
                                        "{:.0}% · {}",
                                        progress.unwrap_or(0.0) * 100.0,
                                        fmt_speed(transfer.speed, false)
                                    )
                                } else {
                                    text.clone()
                                }),
                        )
                        .on_hover_text(text);
                    } else if op.state == engine::OperationState::Complete {
                        ui.label(fmt_bytes(transfer.size));
                    }
                });
                row.col(|ui| {
                    ui.push_id(op.id, |ui| {
                        if op.state.is_active()
                            && transfer.cancellable
                            && !matches!(
                                op.state,
                                engine::OperationState::Stopping | engine::OperationState::Paused
                            )
                            && ui.small_button("Cancel").clicked()
                        {
                            self.send(Command::CancelOperation(op.id));
                        }
                        if matches!(
                            op.state,
                            engine::OperationState::Failed(_)
                                | engine::OperationState::Cancelled
                                | engine::OperationState::Uncertain(_)
                        ) && ui.small_button("Retry").clicked()
                        {
                            self.send(Command::RetryOperation(op.id));
                        }
                        if matches!(
                            op.state,
                            engine::OperationState::Failed(_)
                                | engine::OperationState::Cancelled
                                | engine::OperationState::Uncertain(_)
                        ) && ui.small_button("Retry with interface…").clicked()
                        {
                            self.open_retry_interface(op.id);
                        }
                        if op.state == engine::OperationState::Paused {
                            let ready =
                                self.snapshot.devices.iter().any(|d| {
                                    Some(d.id) == op.device && d.state == SwitchState::Ready
                                });
                            ui.menu_button("Actions", |ui| {
                                if ui
                                    .add_enabled(ready, egui::Button::new("Review queue"))
                                    .on_disabled_hover_text("Reconnect the device first")
                                    .clicked()
                                {
                                    if let Some(id) = op.device {
                                        self.send(Command::ReviewPending(vec![id]));
                                    }
                                    ui.close();
                                }
                                if ui.button("Cancel job").clicked() {
                                    self.send(Command::CancelOperation(op.id));
                                    ui.close();
                                }
                            });
                        }
                    });
                });
            })
        });
    }
}
