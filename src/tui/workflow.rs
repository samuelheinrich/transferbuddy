//! Keyboard adapter for the same assistant, preflight and recovery used by egui.
use super::*;
use ratatui::widgets::{Clear, Paragraph, Wrap};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc;
use transferbuddy_core::engine::{self, WorkflowGoal, WorkflowSpec, WorkflowTarget};
#[derive(Default)]
pub struct Editor {
    pub step: usize,
    pub workflow: Option<u64>,
    pub devices: BTreeSet<u64>,
    pub source: String,
    pub destination: String,
    pub receive: bool,
    pub upgrade: bool,
    pub protocol_index: usize,
    pub overrides: BTreeMap<u64, String>,
    pub edit: Option<Field>,
    pub profile_name: String,
    pub report_name: String,
    pub checks: Vec<engine::Preflight>,
    pub checked_spec: Option<WorkflowSpec>,
    pub checked_revision: u64,
    pending: Option<mpsc::Receiver<(WorkflowSpec, u64, Vec<engine::Preflight>)>>,
    question: Option<engine::Confirmation>,
    profile_index: usize,
    profile_targets: Option<Vec<engine::ProfileTarget>>,
    profile_seen: BTreeSet<(String, u16)>,
}
#[derive(Clone, Copy)]
pub enum Field {
    Source,
    Destination,
    Profile,
    Report,
    Override(u64),
}
impl Editor {
    fn text(&mut self, field: Field) -> &mut String {
        match field {
            Field::Source => {
                self.profile_targets = None;
                &mut self.source
            }
            Field::Destination => {
                self.profile_targets = None;
                &mut self.destination
            }
            Field::Profile => &mut self.profile_name,
            Field::Report => &mut self.report_name,
            Field::Override(device) => {
                self.profile_targets = None;
                self.overrides.entry(device).or_default()
            }
        }
    }
}
pub fn open(ui: &mut Ui) {
    if ui.workflow.destination.is_empty() {
        ui.workflow.destination = "flash:".into();
    }
    if ui.workflow.source.is_empty() {
        if let Some(file) = ui
            .files
            .entries
            .get(ui.files.selected)
            .filter(|f| !f.is_dir)
        {
            ui.workflow.source = if ui.files.cwd.is_empty() {
                file.name.clone()
            } else {
                format!("{}/{}", ui.files.cwd, file.name)
            };
        }
    }
    if ui.workflow.devices.is_empty() {
        if let Some(sw) = selected_switch(ui) {
            ui.workflow.devices.insert(sw.id);
        }
    }
    ui.modal = Some(Modal::Workflow);
}
fn spec(ui: &Ui) -> WorkflowSpec {
    let mut targets = Vec::new();
    if let Some(profile) = &ui.workflow.profile_targets {
        let devices = ui.app.engine().snapshot().devices;
        targets = profile
            .iter()
            .filter_map(|t| {
                devices
                    .iter()
                    .find(|d| {
                        d.host == t.host && d.port == t.port && ui.workflow.devices.contains(&d.id)
                    })
                    .map(|d| WorkflowTarget {
                        device: d.id,
                        local: t.local.clone(),
                        remote: t.remote.clone(),
                    })
            })
            .collect();
    }
    for device in &ui.workflow.devices {
        if ui.workflow.profile_targets.is_some() {
            break;
        }
        if let Ok(sw) = ui.app.engine().device(*device) {
            let sources = if ui.workflow.upgrade {
                ui.workflow
                    .overrides
                    .get(device)
                    .unwrap_or(&ui.workflow.source)
                    .clone()
            } else {
                ui.workflow.source.clone()
            };
            for source in sources.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let name = source.rsplit(['/', ':']).next().unwrap_or(source);
                let (local, remote) = if ui.workflow.receive {
                    (
                        format!(
                            "{}/{}/{}",
                            ui.workflow.destination.trim_matches('/'),
                            sw.host,
                            name
                        ),
                        source.into(),
                    )
                } else {
                    let dest = if ui.workflow.upgrade {
                        sw.facts().flash_device
                    } else {
                        ui.workflow.destination.clone()
                    };
                    (
                        source.into(),
                        format!(
                            "{}{}{}",
                            dest,
                            if dest.ends_with([':', '/']) { "" } else { "/" },
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
    }
    WorkflowSpec {
        goal: if ui.workflow.upgrade {
            WorkflowGoal::Upgrade
        } else {
            WorkflowGoal::Transfer
        },
        receive: ui.workflow.receive,
        protocol: if ui.workflow.receive {
            Protocol::Ftp
        } else {
            engine::PROTOCOL_PRIORITY[ui.workflow.protocol_index]
        },
        targets,
    }
}
pub fn tick(ui: &mut Ui) {
    if let Some(profile) = &ui.workflow.profile_targets {
        let devices = ui.app.engine().snapshot().devices;
        for target in profile {
            if let Some(d) = devices
                .iter()
                .find(|d| d.host == target.host && d.port == target.port)
            {
                if ui
                    .workflow
                    .profile_seen
                    .insert((target.host.clone(), target.port))
                {
                    ui.workflow.devices.insert(d.id);
                }
            }
        }
    }
    if let Some(pending) = &ui.workflow.pending {
        if let Ok((spec, revision, checks)) = pending.try_recv() {
            ui.workflow.checked_spec = Some(spec);
            ui.workflow.checked_revision = revision;
            ui.workflow.checks = checks;
            ui.workflow.pending = None;
        }
    }
    ui.workflow.question = ui
        .app
        .engine()
        .pending_confirmations()
        .into_iter()
        .find(|c| ui.workflow.devices.contains(&c.device));
}
pub fn key(ui: &mut Ui, key: KeyEvent) {
    if let Some(field) = ui.workflow.edit {
        match key.code {
            KeyCode::Enter | KeyCode::Esc => ui.workflow.edit = None,
            KeyCode::Backspace => {
                ui.workflow.text(field).pop();
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                ui.workflow.text(field).clear()
            }
            KeyCode::Char(c) => ui.workflow.text(field).push(c),
            _ => {}
        }
        return;
    }
    if let Some(question) = ui.workflow.question.clone() {
        if matches!(key.code, KeyCode::Char('y') | KeyCode::Esc) {
            submit(
                ui,
                Command::Reply {
                    request: question.id,
                    input: if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                        "y"
                    } else {
                        "n"
                    }
                    .into(),
                },
            );
            ui.workflow.question = None;
        }
        return;
    }
    match key.code {
        KeyCode::Esc => ui.modal = None,
        KeyCode::Right | KeyCode::Tab => {
            let executed = ui.workflow.workflow.is_some_and(|id| {
                ui.app
                    .engine()
                    .snapshot()
                    .workflows
                    .iter()
                    .any(|w| w.id == id && !w.jobs.is_empty())
            });
            ui.workflow.step = (ui.workflow.step + 1).min(if !executed {
                3
            } else if ui.workflow.upgrade {
                6
            } else {
                5
            })
        }
        KeyCode::Left | KeyCode::BackTab => {
            let executed = ui.workflow.workflow.is_some_and(|id| {
                ui.app
                    .engine()
                    .snapshot()
                    .workflows
                    .iter()
                    .any(|w| w.id == id && !w.jobs.is_empty())
            });
            ui.workflow.step = ui
                .workflow
                .step
                .saturating_sub(1)
                .max(if executed { 4 } else { 0 });
        }
        KeyCode::Up => ui.switch_sel = ui.switch_sel.saturating_sub(1),
        KeyCode::Down => {
            ui.switch_sel = (ui.switch_sel + 1).min(ui.app.switches.list().len().saturating_sub(1))
        }
        KeyCode::Char('t') if ui.workflow.step == 0 => {
            ui.workflow.profile_targets = None;
            ui.workflow.upgrade = false;
            ui.workflow.receive = false;
        }
        KeyCode::Char('u') if ui.workflow.step == 0 => {
            ui.workflow.profile_targets = None;
            ui.workflow.upgrade = true;
            ui.workflow.receive = false;
        }
        KeyCode::Char('d') if ui.workflow.step == 0 => {
            ui.workflow.profile_targets = None;
            ui.workflow.upgrade = false;
            ui.workflow.receive = !ui.workflow.receive;
            ui.workflow.source.clear();
            ui.workflow.destination = if ui.workflow.receive {
                "downloads".into()
            } else {
                "flash:".into()
            };
        }
        KeyCode::Char(' ') if ui.workflow.step == 1 => {
            if let Some(sw) = selected_switch(ui) {
                if !ui.workflow.devices.remove(&sw.id) {
                    ui.workflow.devices.insert(sw.id);
                }
            }
        }
        KeyCode::Char('a') if ui.workflow.step == 1 => {
            activate_upgrade_action(ui, 1);
        }
        KeyCode::Char('b') if ui.workflow.step == 1 => {
            activate_upgrade_action(ui, 2);
        }
        KeyCode::Char('r') if ui.workflow.step == 1 => {
            submit(
                ui,
                Command::ReconnectMany(ui.workflow.devices.iter().copied().collect()),
            );
        }
        KeyCode::Char('e') if ui.workflow.step == 1 => {
            if let Some(sw) = selected_switch(ui) {
                ui.modal = Some(Modal::WorkflowLogin {
                    device: sw.id,
                    form: DeployForm {
                        host: sw.host.clone(),
                        username: sw.connection_username(),
                        password: String::new(),
                        enable_password: String::new(),
                        field: 0,
                        ..Default::default()
                    },
                });
            }
        }
        KeyCode::Char('f') if ui.workflow.step == 2 => ui.workflow.edit = Some(Field::Source),
        KeyCode::Char('D') if ui.workflow.step == 2 => ui.workflow.edit = Some(Field::Destination),
        KeyCode::Char('o') if ui.workflow.step == 2 => {
            if let Some(sw) = selected_switch(ui) {
                let source = ui.workflow.source.clone();
                ui.workflow.overrides.entry(sw.id).or_insert(source);
                ui.workflow.edit = Some(Field::Override(sw.id));
            }
        }
        KeyCode::Char('p') if ui.workflow.step == 3 => {
            ui.workflow.protocol_index =
                (ui.workflow.protocol_index + 1) % engine::PROTOCOL_PRIORITY.len()
        }
        KeyCode::Char('i') if ui.workflow.step == 3 => cycle_advertise(ui),
        KeyCode::Char('c') if ui.workflow.step == 3 => {
            let spec = spec(ui);
            let revision = ui.app.engine().snapshot().network.revision;
            let (tx, rx) = mpsc::channel();
            let app = ui.app.clone();
            ui.workflow.pending = Some(rx);
            let runtime = app.runtime.clone();
            runtime.spawn_blocking(move || {
                let checks = app.engine().preflight(&spec);
                let _ = tx.send((spec, revision, checks));
            });
        }
        KeyCode::Enter if ui.workflow.step == 3 => {
            let current = spec(ui);
            let checked = ui.workflow.checked_spec.as_ref() == Some(&current)
                && ui.workflow.checked_revision == ui.app.engine().snapshot().network.revision
                && !ui.workflow.checks.is_empty()
                && ui.workflow.checks.iter().all(|c| c.blocker.is_none());
            if !checked {
                ui.status_msg =
                    Some("Press c to check this task and resolve blockers first".into());
                return;
            }
            match ui.app.engine().submit(Command::SaveWorkflow {
                previous: ui.workflow.workflow,
                spec: current,
            }) {
                Ok(workflow) => {
                    ui.workflow.workflow = Some(workflow);
                    submit(ui, Command::StartWorkflow(workflow));
                    ui.workflow.step = 4;
                }
                Err(error) => ui.status_msg = Some(error),
            }
        }
        KeyCode::Char('j') if ui.workflow.step >= 4 => ui.modal = Some(Modal::Jobs { selected: 0 }),
        KeyCode::Char('i') if ui.workflow.upgrade && ui.workflow.step == 5 => {
            if let Some(sw) = selected_switch(ui) {
                submit(
                    ui,
                    Command::RequestWorkflowInstall {
                        workflow: ui.workflow.workflow.unwrap_or(0),
                        device: sw.id,
                        yolo: false,
                    },
                );
            }
        }
        KeyCode::Char('v') if ui.workflow.upgrade && ui.workflow.step == 5 => {
            if let Some(sw) = selected_switch(ui) {
                submit(ui, Command::Verify(sw.id));
            }
        }
        KeyCode::Char('x') if ui.workflow.step >= 5 => {
            if let Some(sw) = selected_switch(ui) {
                submit(ui, Command::RemoveInactive(sw.id));
            }
        }
        KeyCode::Char('P') if ui.workflow.step >= 5 => ui.workflow.edit = Some(Field::Profile),
        KeyCode::Char('E') if ui.workflow.step >= 5 => ui.workflow.edit = Some(Field::Report),
        KeyCode::Char('s') if ui.workflow.step >= 5 => {
            if let Some(workflow) = ui.workflow.workflow {
                submit(
                    ui,
                    Command::SaveProfile {
                        name: ui.workflow.profile_name.clone(),
                        workflow,
                    },
                );
            }
        }
        KeyCode::Char('e') if ui.workflow.step >= 5 => {
            if let Some(workflow) = ui.workflow.workflow {
                let local = if ui.workflow.report_name.is_empty() {
                    format!("workflow-{workflow}.csv")
                } else {
                    ui.workflow.report_name.clone()
                };
                submit(ui, Command::ExportWorkflow { workflow, local });
            }
        }
        KeyCode::Char('z') if ui.workflow.step >= 5 => {
            if let Some(workflow) = ui.workflow.workflow {
                submit(ui, Command::StopWorkflowServices(workflow));
            }
        }
        KeyCode::Char('n') => {
            ui.workflow = Editor::default();
            open(ui);
        }
        KeyCode::Char('p') if ui.workflow.step == 0 => {
            let snapshot = ui.app.engine().snapshot();
            if let Some(profile) = snapshot
                .profiles
                .get(ui.workflow.profile_index % snapshot.profiles.len().max(1))
            {
                ui.workflow.profile_index += 1;
                ui.workflow.upgrade = profile.goal == WorkflowGoal::Upgrade;
                ui.workflow.receive = profile.receive;
                ui.workflow.protocol_index = engine::PROTOCOL_PRIORITY
                    .iter()
                    .position(|p| p.label() == profile.protocol)
                    .unwrap_or(0);
                ui.workflow.devices.clear();
                ui.workflow.source.clear();
                ui.workflow.overrides.clear();
                ui.workflow.profile_seen.clear();
                ui.workflow.profile_targets = Some(profile.targets.clone());
                for target in &profile.targets {
                    if let Some(sw) = snapshot
                        .devices
                        .iter()
                        .find(|d| d.host == target.host && d.port == target.port)
                    {
                        ui.workflow.devices.insert(sw.id);
                        ui.workflow
                            .profile_seen
                            .insert((target.host.clone(), target.port));
                        ui.workflow.overrides.insert(sw.id, target.local.clone());
                        if !ui.workflow.source.is_empty() {
                            ui.workflow.source.push(',');
                        }
                        ui.workflow.source.push_str(if profile.receive {
                            &target.remote
                        } else {
                            &target.local
                        });
                    }
                }
                submit(
                    ui,
                    Command::Set(Setting::Advertise(profile.advertise.clone())),
                );
                ui.status_msg = Some(format!(
                    "Profile {} loaded; connect missing devices in Connect",
                    profile.name
                ));
            }
        }
        _ => {}
    }
}
fn submit(ui: &mut Ui, command: Command) {
    ui.status_msg = Some(match ui.app.engine().submit(command) {
        Ok(_) => "Action submitted".into(),
        Err(error) => error,
    });
}
pub fn paste(ui: &mut Ui, text: String) {
    if let Some(field) = ui.workflow.edit {
        ui.workflow.text(field).push_str(&text);
    } else {
        ui.status_msg = Some("Paste cannot approve an action".into());
    }
}
pub fn draw(f: &mut Frame, ui: &Ui) {
    let area = views::centered_rect(110, f.area().height.saturating_sub(4).max(12), f.area());
    f.render_widget(Clear, area);
    let mut lines = vec![
        Line::from(Span::styled(
            format!(
                " {} · step {} · {}",
                if ui.workflow.upgrade {
                    "Firmware upgrade"
                } else {
                    "File transfer"
                },
                ui.workflow.step + 1,
                if ui.workflow.receive {
                    "Device → local"
                } else {
                    "Local → device"
                }
            ),
            Style::default().fg(theme::ACCENT).bold(),
        )),
        Line::from(" Task → Devices → Files → Check → Transfer → Install / Result"),
        Line::from(" ←/→ steps · Esc background · n new task · W reopen"),
        Line::from(""),
    ];
    if let Some(question) = &ui.workflow.question {
        lines.push(Line::from(Span::styled(
            format!("CONFIRM: {:?}", question.kind),
            Style::default().fg(theme::ERR),
        )));
        lines.push(Line::from(
            "Type lowercase y to approve; Esc cancels. Paste cannot approve.",
        ));
    } else {
        match ui.workflow.step {
            0 => {
                lines.push(Line::from(" t Transfer files · u Upgrade firmware · d Toggle download · p Load next profile"));
            }
            1 => {
                lines.push(Line::from(" ↑↓ device · Space include · a add · b bulk · r reconnect selected · e edit login"));
                device_lines(ui, &mut lines);
            }
            2 => {
                lines.push(Line::from(" f edit source(s), comma separated · D destination · o image override for selected device"));
                lines.push(Line::from(format!(" Source: {}", ui.workflow.source)));
                lines.push(Line::from(format!(
                    " Destination: {}",
                    ui.workflow.destination
                )));
            }
            3 => {
                lines.push(Line::from(format!(
                    " Protocol: {} · p cycle · i interface · c check · Enter start",
                    engine::PROTOCOL_PRIORITY[ui.workflow.protocol_index].label()
                )));
                lines.push(Line::from(format!(
                    " Interface: {}",
                    ui.app
                        .config
                        .read()
                        .unwrap()
                        .advertise
                        .as_deref()
                        .unwrap_or("Automatic")
                )));
                if ui.workflow.pending.is_some() {
                    lines.push(Line::from(" Checking…"));
                }
                for check in &ui.workflow.checks {
                    lines.push(Line::from(Span::styled(
                        format!(
                            " Device {} · {}",
                            check.device,
                            check.blocker.as_deref().unwrap_or("Ready to transfer")
                        ),
                        Style::default().fg(if check.blocker.is_some() {
                            theme::ERR
                        } else {
                            theme::OK
                        }),
                    )));
                    for warning in &check.warnings {
                        lines.push(Line::from(warning.clone()));
                    }
                    if let Some(command) = &check.command {
                        lines.push(Line::from(command.clone()));
                    }
                }
            }
            4 => {
                lines.push(Line::from(" j opens jobs and recovery actions. Transfers never start an install automatically."));
                let snapshot = ui.app.engine().snapshot();
                if let Some(workflow) = snapshot
                    .workflows
                    .iter()
                    .find(|w| Some(w.id) == ui.workflow.workflow)
                {
                    for op in snapshot
                        .operations
                        .iter()
                        .filter(|o| workflow.jobs.contains(&o.id))
                    {
                        lines.push(Line::from(format!(
                            " #{} · {} · {}",
                            op.id,
                            op.label,
                            op.state.label()
                        )));
                    }
                }
            }
            _ => {
                lines.push(Line::from(
                    " ↑↓ device · i install · v verify existing · x cleanup · j jobs",
                ));
                lines.push(Line::from(" P profile name · s save profile · E report path · e export CSV · z stop owned services"));
                lines.push(Line::from(format!(
                    " Profile: {} · report: {}",
                    ui.workflow.profile_name, ui.workflow.report_name
                )));
                device_lines(ui, &mut lines);
            }
        }
    }
    if let Some(field) = ui.workflow.edit {
        let text = match field {
            Field::Source => ui.workflow.source.as_str(),
            Field::Destination => ui.workflow.destination.as_str(),
            Field::Profile => ui.workflow.profile_name.as_str(),
            Field::Report => ui.workflow.report_name.as_str(),
            Field::Override(id) => ui
                .workflow
                .overrides
                .get(&id)
                .map(String::as_str)
                .unwrap_or(""),
        };
        lines.push(Line::from(Span::styled(
            format!(" Editing: {text}▌ · Enter done · Ctrl+A clear"),
            Style::default().fg(theme::ACCENT),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .block(theme::panel(" WORKFLOW ASSISTANT "))
            .wrap(Wrap { trim: false }),
        area,
    );
}
fn device_lines(ui: &Ui, lines: &mut Vec<Line<'static>>) {
    let snapshot = ui.app.engine().snapshot();
    let start = ui.switch_sel.saturating_sub(2);
    for (index, d) in snapshot.devices.iter().enumerate().skip(start).take(5) {
        let version = d.facts.version.clone().unwrap_or_default();
        lines.push(Line::from(Span::styled(
            format!(
                "{} [{}] {} · {} · {} · {} · {}",
                if index == ui.switch_sel { "▶" } else { " " },
                if ui.workflow.devices.contains(&d.id) {
                    "x"
                } else {
                    " "
                },
                d.name,
                d.host,
                version.model.unwrap_or_default(),
                version.version.unwrap_or_default(),
                d.upgrade.label()
            ),
            Style::default().fg(if index == ui.switch_sel {
                theme::ACCENT
            } else {
                theme::TEXT
            }),
        )));
    }
}
pub fn login_key(ui: &mut Ui, device: u64, form: &mut DeployForm, key: KeyEvent) -> bool {
    let selected = form.field.min(2);
    match key.code {
        KeyCode::Esc => return false,
        KeyCode::Tab => form.field = (selected + 1) % 3,
        KeyCode::BackTab => form.field = (selected + 2) % 3,
        KeyCode::Enter => {
            match ui.app.engine().submit(Command::UpdateCredentials(
                device,
                Credentials {
                    username: form.username.clone(),
                    password: form.password.clone(),
                    enable_password: form.enable_password.clone(),
                },
            )) {
                Ok(_) => {
                    submit(ui, Command::Reconnect(device));
                    return false;
                }
                Err(error) => ui.status_msg = Some(error),
            }
        }
        KeyCode::Backspace => {
            match form.field {
                1 => &mut form.password,
                2 => &mut form.enable_password,
                _ => &mut form.username,
            }
            .pop();
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => match form.field {
            1 => &mut form.password,
            2 => &mut form.enable_password,
            _ => &mut form.username,
        }
        .clear(),
        KeyCode::Char(c) => match form.field {
            1 => &mut form.password,
            2 => &mut form.enable_password,
            _ => &mut form.username,
        }
        .push(c),
        _ => {}
    }
    true
}
pub fn draw_login(f: &mut Frame, form: &DeployForm) {
    let area = views::centered_rect(76, 12, f.area());
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(vec![
            Line::from(format!(
                " {} · Tab fields · Enter save & reconnect · Esc cancel",
                form.host
            )),
            Line::from(format!(" Username: {}", form.username)),
            Line::from(format!(
                " Password: {}",
                "•".repeat(form.password.chars().count())
            )),
            Line::from(format!(
                " Enable: {}",
                "•".repeat(form.enable_password.chars().count())
            )),
        ])
        .block(theme::panel(" EDIT LOGIN ")),
        area,
    );
}
