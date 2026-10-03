//! Optional assistant. Business decisions and job ownership live in the core.
use super::*;
use engine::{WorkflowGoal, WorkflowSpec, WorkflowTarget};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Default)]
pub(super) struct Wizard {
    pub open: bool,
    pub generation: u64,
    pub step: usize,
    pub workflow: Option<u64>,
    pub start_after_save: bool,
    pub spec: WorkflowSpec,
    pub devices: BTreeSet<u64>,
    pub files: BTreeSet<String>,
    pub overrides: BTreeMap<u64, String>,
    pub destination: String,
    pub checks: Vec<engine::Preflight>,
    pub check_spec: Option<WorkflowSpec>,
    pub check_revision: u64,
    pub checking: bool,
    pub profile_name: String,
    pub report_name: String,
    pub profile: Option<engine::WorkProfile>,
    pub profile_targets: Option<Vec<engine::ProfileTarget>>,
    pub profile_connected: BTreeSet<(String, u16)>,
}
impl Wizard {
    fn new(files: Vec<String>, device: Option<u64>) -> Self {
        Self {
            open: true,
            generation: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            destination: "flash:".into(),
            files: files.into_iter().collect(),
            devices: device.into_iter().collect(),
            ..Self::default()
        }
    }
}
impl Desktop {
    pub(super) fn open_wizard(&mut self) {
        if self.wizard.workflow.is_some() || !self.wizard.devices.is_empty() {
            self.wizard.open = true;
            return;
        }
        self.wizard = Wizard::new(
            self.selected_file_names(false)
                .iter()
                .map(|n| self.local_path(n))
                .collect(),
            self.device,
        );
        self.wizard.spec.protocol = self
            .app
            .as_ref()
            .and_then(|a| engine::protocol_options(a, false).first().copied())
            .unwrap_or(Protocol::Sftp);
    }
    fn wizard_targets(&mut self) {
        if self.wizard.workflow.is_some_and(|id| {
            self.snapshot
                .workflows
                .iter()
                .any(|w| w.id == id && !w.jobs.is_empty())
        }) {
            return;
        }
        if let Some(profile) = &self.wizard.profile_targets {
            self.wizard.spec.targets = profile
                .iter()
                .filter_map(|target| {
                    self.snapshot
                        .devices
                        .iter()
                        .find(|d| {
                            d.host == target.host
                                && d.port == target.port
                                && self.wizard.devices.contains(&d.id)
                        })
                        .map(|d| WorkflowTarget {
                            device: d.id,
                            local: target.local.clone(),
                            remote: target.remote.clone(),
                        })
                })
                .collect();
            return;
        }
        let mut targets = Vec::new();
        for device in &self.wizard.devices {
            let Some(sw) = self.snapshot.devices.iter().find(|d| d.id == *device) else {
                continue;
            };
            let sources: Vec<_> = if self.wizard.spec.goal == WorkflowGoal::Upgrade {
                self.wizard
                    .overrides
                    .get(device)
                    .cloned()
                    .or_else(|| self.wizard.files.iter().next().cloned())
                    .into_iter()
                    .collect()
            } else {
                self.wizard.files.iter().cloned().collect()
            };
            for source in sources {
                let name = source.rsplit(['/', ':']).next().unwrap_or(&source);
                let (local, remote) = if self.wizard.spec.receive {
                    (
                        format!(
                            "{}/{}/{}",
                            self.wizard.destination.trim_matches('/'),
                            sw.host,
                            name
                        ),
                        source,
                    )
                } else {
                    let directory = if self.wizard.spec.goal == WorkflowGoal::Upgrade {
                        sw.facts.flash_device.as_str()
                    } else {
                        self.wizard.destination.as_str()
                    };
                    (
                        source.clone(),
                        format!(
                            "{}{}{}",
                            directory,
                            if directory.ends_with([':', '/']) {
                                ""
                            } else {
                                "/"
                            },
                            name
                        ),
                    )
                };
                targets.push(WorkflowTarget {
                    device: *device,
                    local,
                    remote,
                });
            }
        }
        self.wizard.spec.targets = targets;
        if self.wizard.spec.receive {
            self.wizard.spec.protocol = Protocol::Ftp;
        }
    }
    fn check_wizard(&mut self, ctx: &egui::Context) {
        let Some(app) = self.app.clone() else { return };
        if self.wizard.checking {
            return;
        }
        self.wizard.checking = true;
        let spec = self.wizard.spec.clone();
        let revision = self.snapshot.network.revision;
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.runtime.spawn_blocking(move || {
            let checks = app.engine().preflight(&spec);
            let _ = tx.send(UiEvent::Preflight {
                spec,
                revision,
                checks,
            });
            ctx.request_repaint();
        });
    }
    pub(super) fn wizard_dialog(&mut self, ctx: &egui::Context) {
        if !self.wizard.open {
            return;
        }
        self.wizard_targets();
        let executed = self
            .wizard
            .workflow
            .and_then(|id| self.snapshot.workflows.iter().find(|w| w.id == id))
            .is_some_and(|w| !w.jobs.is_empty());
        let mut open = true;
        egui::Window::new("Connect · Transfer · Upgrade")
            .id(egui::Id::new("workflow_assistant"))
            .open(&mut open)
            .collapsible(false)
            .default_width(860.0)
            .default_height((ctx.content_rect().height() - 80.0).max(240.0))
            .max_width((ctx.content_rect().width() - 32.0).max(300.0))
            .max_height((ctx.content_rect().height() - 32.0).max(200.0))
            .vscroll(false)
            .show(ctx, |ui| self.wizard_content(ui, ctx, executed));
        if !open
            || (self.connect.is_none()
                && self.confirmation.is_none()
                && ctx.input(|i| i.key_pressed(egui::Key::Escape)))
        {
            self.wizard.open = false;
        }
    }
    fn wizard_content(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, executed: bool) {
        let stages = if self.wizard.spec.goal == WorkflowGoal::Upgrade {
            vec![
                "Task", "Devices", "Images", "Check", "Transfer", "Install", "Result",
            ]
        } else {
            vec!["Task", "Devices", "Files", "Check", "Transfer", "Result"]
        };
        self.wizard.step = self.wizard.step.min(stages.len() - 1);
        ui.horizontal_wrapped(|ui| {
            for (index, label) in stages.iter().enumerate() {
                if ui
                    .add_enabled(
                        !self.wizard.start_after_save
                            && ((!executed && index <= 3) || (executed && index >= 4)),
                        egui::Button::selectable(
                            self.wizard.step == index,
                            format!("{}. {label}", index + 1),
                        ),
                    )
                    .clicked()
                {
                    self.wizard.step = index;
                }
            }
        });
        ui.separator();
        let content_height = (ui.available_height() - 82.0).max(80.0);
        egui::ScrollArea::vertical().id_salt("wizard_step_content").max_height(content_height).auto_shrink([false, false]).show(ui, |ui| {
        ui.add_enabled_ui(!self.wizard.start_after_save, |ui| {
        match self.wizard.step {
            0 => {
                ui.heading("What would you like to do?");
                ui.selectable_value(
                    &mut self.wizard.spec.goal,
                    WorkflowGoal::Transfer,
                    "Transfer files",
                );
                ui.selectable_value(
                    &mut self.wizard.spec.goal,
                    WorkflowGoal::Upgrade,
                    "Upgrade firmware",
                );
                if self.wizard.spec.goal == WorkflowGoal::Transfer {
                    let old = self.wizard.spec.receive;
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut self.wizard.spec.receive, false, "Local → device");
                        ui.selectable_value(&mut self.wizard.spec.receive, true, "Device → local");
                    });
                    if old != self.wizard.spec.receive {
                        self.wizard.profile_targets = None;
                        self.wizard.files.clear();
                        self.wizard.destination = if self.wizard.spec.receive {
                            "downloads".into()
                        } else {
                            "flash:".into()
                        };
                    }
                } else {
                    self.wizard.spec.receive = false;
                    if self.wizard.files.len() > 1 {
                        self.wizard.files = self.wizard.files.iter().take(1).cloned().collect();
                    }
                }
                let profiles = self.snapshot.profiles.clone();
                if !profiles.is_empty() {
                    ui.separator();
                    ui.label("Reuse a work profile (no SSH credentials stored)");
                    for profile in profiles {
                        if ui.button(&profile.name).clicked() {
                            self.wizard.spec.goal = profile.goal;
                            self.wizard.spec.receive = profile.receive;
                            self.wizard.spec.protocol = engine::PROTOCOL_PRIORITY
                                .into_iter()
                                .find(|p| p.label() == profile.protocol)
                                .unwrap_or(Protocol::Sftp);
                            self.wizard.devices.clear();
                            self.wizard.files.clear();
                            self.wizard.overrides.clear();
                            self.wizard.profile_connected.clear();
                            self.wizard.profile = Some(profile.clone());
                            self.wizard.profile_targets = Some(profile.targets.clone());
                            self.wizard.destination = if profile.receive {
                                "downloads".into()
                            } else {
                                "flash:".into()
                            };
                            self.send(Command::Set(Setting::Advertise(profile.advertise.clone())));
                            self.wizard.step = 1;
                        }
                    }
                }
            }
            1 => {
                ui.heading("Choose devices");
                ui.horizontal(|ui| {
                    if ui.button("Add device…").clicked() {
                        self.intent(Intent::Add(false), ctx);
                    }
                    if ui.button("Bulk add / subnet…").clicked() {
                        self.intent(Intent::Add(true), ctx);
                    }
                    if ui.button("Select connected").clicked() {
                        self.wizard.devices = self
                            .snapshot
                            .devices
                            .iter()
                            .filter(|d| d.state == SwitchState::Ready)
                            .map(|d| d.id)
                            .collect();
                    }
                });
                if let Some(profile) = self.wizard.profile.clone() {
                    let mut missing = Vec::new();
                    for target in &profile.targets {
                        if let Some(d) = self
                            .snapshot
                            .devices
                            .iter()
                            .find(|d| d.host == target.host && d.port == target.port)
                        {
                            if self.wizard.profile_connected.insert((target.host.clone(), target.port)) { self.wizard.devices.insert(d.id); }
                            self.wizard.files.insert(if profile.receive {
                                target.remote.clone()
                            } else {
                                target.local.clone()
                            });
                            if profile.goal == WorkflowGoal::Upgrade {
                                self.wizard.overrides.insert(d.id, target.local.clone());
                            }
                        } else {
                            missing.push(target);
                        }
                    }
                    if missing.is_empty() {
                        self.wizard.profile = None;
                    } else {
                        ui.colored_label(
                            design::semantic(ui, AMBER),
                            format!("{} profile devices need a connection", missing.len()),
                        );
                        if ui.button("Connect missing profile devices…").clicked() {
                            let port = missing[0].port;
                            let targets = missing
                                .iter()
                                .filter(|t| t.port == port)
                                .map(|t| t.host.as_str())
                                .collect::<Vec<_>>()
                                .join(", ");
                            self.credentials_device = None;
                            self.connect = Some(ConnectionForm {
                                targets,
                                port: port.to_string(),
                                bulk: true,
                                ..Default::default()
                            });
                        }
                    }
                }
                let row_height = if ui.available_width() < 650.0 { 64.0 } else { 32.0 };
                egui::ScrollArea::vertical()
                    .id_salt("wizard_devices")
                    .max_height(260.0)
                    .show_rows(ui, row_height, self.snapshot.devices.len(), |ui, range| {
                        for index in range {
                            let source = &self.snapshot.devices[index];
                            let (id,name,host,state,version) = (source.id,source.name.clone(),source.host.clone(),source.state.clone(),source.facts.version.clone().unwrap_or_default());
                            ui.push_id(id, |ui| {
                                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), row_height), egui::Sense::hover());
                                let mut row = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Min)));
                                row.set_clip_rect(rect.intersect(ui.clip_rect()));
                                row.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                let ui = &mut row;
                                let mut selected = self.wizard.devices.contains(&id);
                                ui.horizontal_wrapped(|ui| {
                                    if ui
                                        .checkbox(
                                            &mut selected,
                                            format!("{} · {}", name, host),
                                        )
                                        .changed()
                                    {
                                        if selected {
                                            self.wizard.devices.insert(id);
                                        } else {
                                            self.wizard.devices.remove(&id);
                                        }
                                    }
                                    design::badge(
                                        ui,
                                        state.label(),
                                        super::views::state_color(&state),
                                    );
                                    ui.label(format!(
                                        "{} · {}",
                                        version.model.unwrap_or_default(),
                                        version.version.unwrap_or_default()
                                    ));
                                    if state.is_over()
                                        && ui.small_button("Reconnect").clicked()
                                    {
                                        self.send(Command::Reconnect(id));
                                    }
                                });
                            });
                        }
                    });
            }
            2 => self.wizard_files(ui),
            3 => {
                ui.heading("Check the transfer path");
                if !self.wizard.spec.receive {
                    egui::ComboBox::from_id_salt("wizard_protocol")
                        .selected_text(self.wizard.spec.protocol.label())
                        .show_ui(ui, |ui| {
                            let options = self
                                .app
                                .as_ref()
                                .map(|a| engine::protocol_options(a, false))
                                .unwrap_or_else(|| engine::PROTOCOL_PRIORITY.to_vec());
                            for protocol in options {
                                ui.selectable_value(
                                    &mut self.wizard.spec.protocol,
                                    protocol,
                                    protocol.label(),
                                );
                            }
                        });
                } else {
                    ui.label("FTP · device → local");
                }
                self.address_selection(ui);
                if self.wizard.checking {
                    ui.spinner();
                    ui.label("Checking prerequisites…");
                }
                let current = self.wizard.check_spec.as_ref() == Some(&self.wizard.spec)
                    && self.wizard.check_revision == self.snapshot.network.revision;
                if !current && !self.wizard.checks.is_empty() {
                    ui.colored_label(
                        design::semantic(ui, AMBER),
                        "Selections or network changed. Check again before starting.",
                    );
                }
                for check in self.wizard.checks.clone() {
                    ui.push_id((check.device, check.command.clone()), |ui| {
                        let device = self
                            .snapshot
                            .devices
                            .iter()
                            .find(|d| d.id == check.device)
                            .map(|d| d.name.clone())
                            .unwrap_or_else(|| check.device.to_string());
                        ui.strong(device);
                        if let Some(blocker) = check.blocker {
                            ui.colored_label(design::semantic(ui, RED), blocker);
                        } else {
                            ui.colored_label(design::semantic(ui, GREEN), "Ready to transfer");
                        }
                        for warning in check.warnings {
                            ui.colored_label(design::semantic(ui, AMBER), warning);
                        }
                        if let Some(endpoint) = check.endpoint {
                            ui.label(format!(
                                "{} · {}{}",
                                endpoint.interface.as_deref().unwrap_or("interface"),
                                endpoint.ip,
                                if endpoint.fixed_bind {
                                    " · fixed service bind"
                                } else {
                                    ""
                                }
                            ));
                        }
                        if let Some(command) = check.command {
                            design::copy_command(ui, &command);
                        }
                        ui.separator();
                    });
                }
            }
            4 => {
                ui.heading("Transfer progress");
                self.wizard_jobs(ui);
                ui.label("Transfers run independently per device. Installs require a separate confirmation.");
            }
            step => {
                if self.wizard.spec.goal == WorkflowGoal::Transfer { ui.heading("Transfer results"); self.wizard_jobs(ui); }
                let install = self.wizard.spec.goal == WorkflowGoal::Upgrade && step == 5;
                if self.wizard.spec.goal == WorkflowGoal::Upgrade { ui.heading(if install {
                    "Ready to install / waiting for reboot"
                } else {
                    "Results"
                }); }
                let selected: Vec<_> = self
                    .snapshot
                    .devices
                    .iter()
                    .filter(|d| self.wizard.spec.goal == WorkflowGoal::Upgrade && self.wizard.devices.contains(&d.id))
                    .cloned()
                    .collect();
                for device in selected {
                    ui.push_id(device.id, |ui| {
                        let info = device.facts.version.clone().unwrap_or_default();
                        let old = self
                            .wizard
                            .workflow
                            .and_then(|id| self.snapshot.workflows.iter().find(|w| w.id == id))
                            .and_then(|w| w.old_versions.get(&device.id))
                            .cloned()
                            .unwrap_or_else(|| info.version.clone().unwrap_or_default());
                        let expected = self
                            .wizard
                            .spec
                            .targets
                            .iter()
                            .find(|t| t.device == device.id)
                            .map(|t| t.local.as_str());
                        let assigned_matches = device
                            .assigned
                            .as_ref()
                            .is_some_and(|(file, _)| Some(file.as_str()) == expected);
                        let target = expected
                            .and_then(upgrade::image_version)
                            .unwrap_or_default();
                        ui.strong(format!(
                            "{} · {} → {}",
                            device.name,
                            upgrade::normalize_version(&old),
                            target
                        ));
                        ui.colored_label(
                            design::semantic(ui, reboot_color(&device.upgrade)),
                            device.upgrade.label(),
                        );
                        if install {
                            let blocker = upgrade::install_blocker(&device.upgrade, &info);
                            if ui
                                .add_enabled(
                                    blocker.is_none()
                                        && assigned_matches
                                        && device.state == SwitchState::Ready,
                                    egui::Button::new("Upgrade…"),
                                )
                                .on_disabled_hover_text(
                                    blocker
                                        .as_deref()
                                        .unwrap_or("Wait until the device is ready"),
                                )
                                .clicked()
                            {
                                if let Some(workflow) = self.wizard.workflow {
                                    self.send(Command::RequestWorkflowInstall {
                                        workflow,
                                        device: device.id,
                                        yolo: false,
                                    });
                                }
                            }
                            if ui
                                .add_enabled(
                                    assigned_matches,
                                    egui::Button::new("Verify existing image"),
                                )
                                .on_disabled_hover_text(
                                    "This device has another image assigned; start a new workflow",
                                )
                                .clicked()
                            {
                                self.send(Command::Verify(device.id));
                            }
                        } else if self.wizard.spec.goal == WorkflowGoal::Upgrade
                            && matches!(device.upgrade, upgrade::Progress::Complete { .. })
                            && ui.button("Remove inactive…").clicked()
                        {
                            self.send(Command::RemoveInactive(device.id));
                        }
                        ui.separator();
                    });
                }
                if let Some(workflow) = self.wizard.workflow {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Profile name");
                        ui.text_edit_singleline(&mut self.wizard.profile_name);
                        if ui
                            .add_enabled(
                                !self.wizard.profile_name.trim().is_empty(),
                                egui::Button::new("Save work profile"),
                            )
                            .clicked()
                        {
                            self.send(Command::SaveProfile {
                                name: self.wizard.profile_name.clone(),
                                workflow,
                            });
                        }
                    });
                    if self.wizard.report_name.is_empty() {
                        self.wizard.report_name = format!("workflow-{workflow}.csv");
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Report file (relative to root)");
                        ui.text_edit_singleline(&mut self.wizard.report_name);
                        if ui.button("Export CSV report").clicked() {
                            self.send(Command::ExportWorkflow {
                                workflow,
                                local: self.wizard.report_name.clone(),
                            });
                        }
                    });
                    if ui
                        .button("Stop services started by this workflow")
                        .clicked()
                    {
                        self.send(Command::StopWorkflowServices(workflow));
                    }
                }
            }
        }
        });
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if self.wizard.start_after_save {
                ui.spinner();
                ui.label("Starting workflow…");
            }
            if ui
                .add_enabled(
                    !self.wizard.start_after_save
                        && self.wizard.step > 0
                        && (!executed || self.wizard.step > 4),
                    egui::Button::new("Back"),
                )
                .clicked()
            {
                self.wizard.step -= 1;
            }
            if self.wizard.step == 3 {
                if ui
                    .add_enabled(
                        !self.wizard.checking && !self.wizard.start_after_save,
                        egui::Button::new("Check prerequisites"),
                    )
                    .clicked()
                {
                    self.wizard_targets();
                    self.check_wizard(ctx);
                }
                let ready = self.wizard.check_spec.as_ref() == Some(&self.wizard.spec)
                    && self.wizard.check_revision == self.snapshot.network.revision
                    && !self.wizard.checks.is_empty()
                    && self.wizard.checks.iter().all(|c| c.blocker.is_none());
                if ui
                    .add_enabled_ui(ready && !self.wizard.start_after_save, |ui| {
                        design::primary(
                            ui,
                            if self.wizard.spec.goal == WorkflowGoal::Upgrade {
                                "Upload & verify selected images"
                            } else {
                                "Start selected transfers"
                            },
                        )
                    })
                    .inner
                    .on_disabled_hover_text("Check prerequisites and resolve blockers first")
                    .clicked()
                {
                    self.wizard.start_after_save = true;
                    if !self.send(Command::SaveWorkflow {
                        previous: self.wizard.workflow,
                        spec: self.wizard.spec.clone(),
                    }) {
                        self.wizard.start_after_save = false;
                    }
                }
            }
            let valid = match self.wizard.step {
                1 => !self.wizard.devices.is_empty(),
                2 => !self.wizard.files.is_empty(),
                3 => executed,
                _ => true,
            };
            if self.wizard.step != 3
                && ui
                    .add_enabled(
                        !self.wizard.start_after_save
                            && valid
                            && self.wizard.step != 3
                            && self.wizard.step + 1 < stages.len(),
                        egui::Button::new("Next"),
                    )
                    .clicked()
            {
                self.wizard.step += 1;
            }
            if ui.button("Continue in background").clicked() {
                self.wizard.open = false;
            }
            if ui.button("New workflow").clicked() {
                self.wizard = Wizard::new(Vec::new(), self.device);
            }
        });
    }

    fn wizard_jobs(&mut self, ui: &mut egui::Ui) {
        let jobs: std::collections::HashSet<_> = self
            .wizard
            .workflow
            .and_then(|id| self.snapshot.workflows.iter().find(|w| w.id == id))
            .map(|w| w.jobs.iter().copied().collect())
            .unwrap_or_default();
        let indices: Vec<_> = self
            .snapshot
            .operations
            .iter()
            .enumerate()
            .filter(|(_, o)| jobs.contains(&o.id))
            .map(|(i, _)| i)
            .collect();
        if indices.is_empty() {
            ui.label("No jobs started. Complete the prerequisite check first.");
            return;
        }
        let complete = indices
            .iter()
            .filter(|i| self.snapshot.operations[**i].state == engine::OperationState::Complete)
            .count();
        ui.label(format!("{complete}/{} jobs complete", indices.len()));
        let names: std::collections::HashMap<_, _> = self
            .snapshot
            .devices
            .iter()
            .map(|d| (d.id, d.name.clone()))
            .collect();
        egui::ScrollArea::vertical()
            .id_salt("wizard_jobs")
            .max_height(ui.available_height())
            .show_rows(ui, 164.0, indices.len(), |ui, range| {
                for index in range {
                    let op = self.snapshot.operations[indices[index]].clone();
                    ui.push_id(op.id, |ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 164.0),
                            egui::Sense::hover(),
                        );
                        let mut row = ui.new_child(
                            egui::UiBuilder::new()
                                .max_rect(rect)
                                .layout(egui::Layout::top_down(egui::Align::Min)),
                        );
                        row.set_clip_rect(rect.intersect(ui.clip_rect()));
                        row.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                        let ui = &mut row;
                        ui.strong(format!(
                            "#{} · {}",
                            op.id,
                            op.device
                                .and_then(|d| names.get(&d))
                                .map(String::as_str)
                                .unwrap_or("removed device")
                        ));
                        let color = match op.state {
                            engine::OperationState::Complete => GREEN,
                            engine::OperationState::Failed(_) => RED,
                            engine::OperationState::Paused
                            | engine::OperationState::Uncertain(_) => AMBER,
                            _ => CYAN,
                        };
                        ui.colored_label(design::semantic(ui, color), op.state.label());
                        if let Some(t) = &op.transfer {
                            let path = format!(
                                "{} → {}",
                                if t.receive { &t.remote } else { &t.local },
                                if t.receive { &t.local } else { &t.remote }
                            );
                            ui.add(egui::Label::new(&path).truncate())
                                .on_hover_text(&path);
                            design::progress(
                                ui,
                                if t.size > 0 {
                                    Some(t.bytes as f64 / t.size as f64)
                                } else {
                                    None
                                },
                                color,
                            );
                            ui.label(format!(
                                "{} · ETA {}",
                                fmt_speed(t.speed, false),
                                t.eta.map(fmt_duration).unwrap_or_else(|| "—".into())
                            ));
                        }
                        if let Some(error) = op.state.error() {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(error).color(design::semantic(ui, RED)),
                                )
                                .truncate(),
                            )
                            .on_hover_text(error);
                        }
                        if ui.small_button("Show job and recovery actions").clicked() {
                            self.job_selected = Some(op.id);
                            self.jobs_open = true;
                            self.wizard.open = false;
                        }
                    });
                }
            });
    }
    fn wizard_files(&mut self, ui: &mut egui::Ui) {
        ui.heading(if self.wizard.spec.goal == WorkflowGoal::Upgrade {
            "Choose release images"
        } else {
            "Choose source files"
        });
        if self.wizard.spec.goal == WorkflowGoal::Upgrade {
            ui.label(
                "Firmware destination: each device's flash filesystem (used by the installer)",
            );
        } else {
            ui.horizontal_wrapped(|ui| {
                ui.label(if self.wizard.spec.receive {
                    "Local destination folder"
                } else {
                    "Remote destination folder"
                });
                if ui
                    .text_edit_singleline(&mut self.wizard.destination)
                    .changed()
                {
                    self.wizard.profile_targets = None;
                }
            });
        }
        if self.wizard.spec.receive {
            ui.label("Choose remote files from the selected device. Downloads use separate device folders.");
            if let Some(device) = self.wizard.devices.iter().next().copied() {
                if self.device != Some(device) {
                    self.choose_device(device);
                }
                ui.horizontal(|ui| {
                    if ui.button("Remote root").clicked() {
                        self.remote_dir.clear();
                        self.remote_loaded = None;
                    }
                    if ui.button("↑ Remote parent").clicked() {
                        self.remote_dir = parent(&self.remote_dir);
                        self.remote_loaded = None;
                    }
                    ui.label(self.remote_path(""));
                });
                let path = self.remote_path("");
                if self.remote_loaded.as_ref() != Some(&(device, path.clone())) {
                    self.send(Command::ListRemote(device, path.clone()));
                    self.remote_loaded = Some((device, path));
                }
                if ui.button("Refresh remote source").clicked() {
                    self.send(Command::ListRemote(device, self.remote_path("")));
                }
                let listing = self
                    .app
                    .as_ref()
                    .and_then(|a| a.engine().device(device).ok())
                    .and_then(|sw| sw.listing(&self.remote_path("")));
                match &listing {
                    None => {
                        ui.spinner();
                        ui.label("Reading remote files…");
                    }
                    Some(Err(error)) => {
                        ui.colored_label(design::semantic(ui, RED), error);
                        ui.label("Reconnect if needed, then Refresh remote source.");
                    }
                    Some(Ok(entries)) if entries.is_empty() => {
                        ui.label("This remote folder is empty.");
                    }
                    _ => {}
                }
                if let Some(Ok(entries)) = self
                    .app
                    .as_ref()
                    .and_then(|a| a.engine().device(device).ok())
                    .and_then(|sw| sw.listing(&self.remote_path("")))
                {
                    egui::ScrollArea::vertical()
                        .id_salt("wizard_remote_files")
                        .max_height(240.0)
                        .show_rows(ui, 32.0, entries.len(), |ui, range| {
                            for index in range {
                                let entry = &entries[index];
                                if entry.is_dir {
                                    if ui.button(format!("▸ {}/", entry.name)).clicked() {
                                        self.remote_dir = if self.remote_dir.is_empty() {
                                            entry.name.clone()
                                        } else {
                                            format!("{}/{}", self.remote_dir, entry.name)
                                        };
                                        self.remote_loaded = None;
                                    }
                                    continue;
                                }
                                let source = self.remote_path(&entry.name);
                                let mut selected = self.wizard.files.contains(&source);
                                if ui.checkbox(&mut selected, &entry.name).changed() {
                                    self.wizard.profile_targets = None;
                                    if selected {
                                        self.wizard.files.insert(source);
                                    } else {
                                        self.wizard.files.remove(&source);
                                    }
                                }
                            }
                        });
                }
            }
        } else {
            ui.horizontal(|ui| {
                if ui.button("Root").clicked() {
                    self.local_dir.clear();
                    self.refresh_local();
                }
                if ui.button("↑ Parent").clicked() {
                    self.local_dir = self
                        .local_dir
                        .rsplit_once('/')
                        .map(|(p, _)| p.into())
                        .unwrap_or_default();
                    self.refresh_local();
                }
                ui.label(if self.local_dir.is_empty() {
                    "/"
                } else {
                    &self.local_dir
                });
            });
            let listing = self.cached_local_files();
            match &listing {
                None => {
                    ui.spinner();
                    ui.label("Reading local files…");
                }
                Some(Err(error)) => {
                    ui.colored_label(design::semantic(ui, RED), error);
                }
                Some(Ok(entries)) if entries.is_empty() => {
                    ui.label("This local folder is empty.");
                }
                _ => {}
            }
            if ui.small_button("Refresh local files").clicked() {
                self.refresh_local();
            }
            let entries = listing.and_then(Result::ok).unwrap_or_default();
            egui::ScrollArea::vertical()
                .id_salt("wizard_files")
                .max_height(240.0)
                .show_rows(ui, 32.0, entries.len(), |ui, range| {
                    for index in range {
                        let entry = &entries[index];
                        ui.push_id(&entry.name, |ui| {
                            if entry.is_dir {
                                if ui.button(format!("▸ {}/", entry.name)).clicked() {
                                    self.local_dir = self.local_path(&entry.name);
                                    self.refresh_local();
                                }
                            } else {
                                let source = self.local_path(&entry.name);
                                let mut selected = self.wizard.files.contains(&source);
                                if ui
                                    .checkbox(
                                        &mut selected,
                                        format!("{} · {}", entry.name, fmt_bytes(entry.size)),
                                    )
                                    .changed()
                                {
                                    self.wizard.profile_targets = None;
                                    if selected {
                                        if self.wizard.spec.goal == WorkflowGoal::Upgrade {
                                            self.wizard.files.clear();
                                        }
                                        self.wizard.files.insert(source);
                                    } else {
                                        self.wizard.files.remove(&source);
                                    }
                                }
                            }
                        });
                    }
                });
        }
        if self.wizard.profile_targets.is_some() {
            ui.label("Using the exact source and destination mappings from the profile. Editing a file or destination replaces these mappings.");
        }
        ui.label(format!(
            "{} selected source file(s)",
            self.wizard.files.len()
        ));
        if self.wizard.spec.goal == WorkflowGoal::Upgrade {
            ui.collapsing("Use a different image per device", |ui| {
                for device in self.wizard.devices.clone() {
                    ui.push_id(device, |ui| {
                        ui.label(
                            self.snapshot
                                .devices
                                .iter()
                                .find(|d| d.id == device)
                                .map(|d| d.name.as_str())
                                .unwrap_or("device"),
                        );
                        let default = self.wizard.files.iter().next().cloned().unwrap_or_default();
                        let mut image = self
                            .wizard
                            .overrides
                            .get(&device)
                            .cloned()
                            .unwrap_or(default.clone());
                        if ui.text_edit_singleline(&mut image).changed() {
                            self.wizard.profile_targets = None;
                            if image == default {
                                self.wizard.overrides.remove(&device);
                            } else {
                                self.wizard.overrides.insert(device, image);
                            }
                        }
                        if self.wizard.overrides.contains_key(&device)
                            && ui.small_button("Use shared image").clicked()
                        {
                            self.wizard.overrides.remove(&device);
                            self.wizard.profile_targets = None;
                        }
                    });
                }
            });
        }
    }
}
