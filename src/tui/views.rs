use chrono::{DateTime, Local};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap};

use crate::logging::LogLevel;
use crate::services::{ServiceId, ServiceStatus};
use crate::session::{fmt_bytes, fmt_duration, fmt_speed, SessionState};
use crate::switch::{LineKind, SwitchState};

use super::{theme, DeployField, EditField, Modal, Tab, Ui};

pub fn draw(f: &mut Frame, ui: &mut Ui) {
    ui.upgrade_buttons.clear();
    // Paint the retro background first; every panel keeps it.
    f.render_widget(Block::default().style(theme::screen()), f.area());

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_tabs(f, ui, chunks[0]);
    match ui.tab {
        Tab::Dashboard => draw_dashboard(f, ui, chunks[1]),
        Tab::Connect => draw_connections(f, ui, chunks[1]),
        Tab::Files => draw_files(f, ui, chunks[1]),
        Tab::Logs => draw_logs(f, ui, chunks[1]),
        Tab::Upgrade => draw_switches(f, ui, chunks[1]),
    }
    draw_footer(f, ui, chunks[2]);
    draw_modal(f, ui);
}

fn draw_tabs(f: &mut Frame, ui: &Ui, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| {
            let (num, name) = t.title().split_at(1);
            Line::from(vec![
                Span::styled(num.to_string(), Style::default().fg(theme::ACCENT).bold()),
                Span::styled(
                    format!(" {}", name.trim()),
                    Style::default().fg(theme::TEXT),
                ),
            ])
        })
        .collect();
    let idx = Tab::ALL.iter().position(|t| *t == ui.tab).unwrap_or(0);

    let sound = ui.app.config.read().unwrap().sound;
    let status = format!(" {} v{} ", if sound { "♪" } else { "×" }, crate::VERSION);
    let cols = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(status.chars().count() as u16),
    ])
    .split(area);

    let tabs = Tabs::new(titles)
        .select(idx)
        .style(Style::default().bg(theme::BG))
        .highlight_style(Style::default().fg(theme::BG).bg(theme::HILITE).bold())
        .divider(Span::styled("│", Style::default().fg(theme::FRAME)));
    f.render_widget(tabs, cols[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            status,
            Style::default().fg(if sound { theme::ACCENT } else { theme::DIM }),
        ))),
        cols[1],
    );
}

/// Footer key hints per view — keys highlighted, descriptions dimmed.
fn footer_keys(tab: Tab) -> Vec<(&'static str, &'static str)> {
    match tab {
        Tab::Dashboard => vec![
            ("Tab", "services/transfers"),
            ("Enter", "edit"),
            ("Space", "enable"),
            ("s", "start/stop"),
            ("S", "all"),
            ("1–5", "view"),
        ],
        Tab::Connect => vec![
            ("a", "add"),
            ("b", "bulk/subnet"),
            ("Enter", "console"),
            ("c", "CLI"),
            ("r", "refresh/reconnect"),
            ("x", "disconnect"),
            ("1–5", "view"),
        ],
        Tab::Files => vec![
            ("Tab", "pane"),
            ("Enter", "open/copy"),
            ("Del/D", "delete remote"),
            ("Bksp/Home", "up/root"),
            ("t", "transfer"),
            ("p", "protocol"),
            ("R", "refresh"),
            ("H", "hash"),
            ("1–5", "view"),
        ],
        Tab::Upgrade => vec![
            ("Tab", "files/jobs"),
            ("p", "protocol"),
            ("a/A", "assign one/all"),
            ("d/V", "deploy/verify"),
            ("u/Y", "install/YOLO"),
            ("i", "cleanup"),
            ("1–5", "view"),
        ],
        Tab::Logs => vec![
            ("↑↓", "scroll"),
            ("G", "follow"),
            ("/", "filter"),
            ("L", "level"),
            ("P", "protocol"),
            ("1–5", "view"),
        ],
    }
}

fn draw_footer(f: &mut Frame, ui: &Ui, area: Rect) {
    let mut spans = Vec::new();
    if let Some(msg) = &ui.status_msg {
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::default().fg(theme::BG).bg(theme::HILITE).bold(),
        ));
        spans.push(Span::raw(" "));
    } else {
        spans.push(Span::raw(" "));
    }
    spans.push(theme::key("W"));
    spans.push(Span::styled(" workflow  ", Style::default().fg(theme::DIM)));
    for (key, desc) in footer_keys(ui.tab) {
        spans.push(theme::key(key));
        spans.push(Span::styled(
            format!(" {desc}  "),
            Style::default().fg(theme::DIM),
        ));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(theme::BG)),
        area,
    );
}

fn status_style(status: &ServiceStatus) -> Style {
    match status {
        ServiceStatus::Running => Style::default().fg(theme::OK).bold(),
        ServiceStatus::Starting | ServiceStatus::Stopping => Style::default().fg(theme::WARN),
        ServiceStatus::Failed(_) => Style::default().fg(theme::ERR).bold(),
        ServiceStatus::Stopped => Style::default().fg(theme::ERR),
    }
}

fn level_color(level: LogLevel) -> Color {
    match level {
        LogLevel::Debug => theme::DIM,
        LogLevel::Info => theme::TEXT,
        LogLevel::Warning => theme::WARN,
        LogLevel::Error => theme::ERR,
    }
}

fn draw_dashboard(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let service_height = area.height.saturating_sub(11).clamp(9, 19);
    let chunks = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(service_height),
        Constraint::Min(6),
    ])
    .split(area);
    let cfg = ui.app.config.read().unwrap();
    let (address, source) = super::advertised_now(&cfg);
    let lines = vec![
        Line::from(vec![
            theme::label("Root: "),
            theme::value(cfg.root.display().to_string()),
        ]),
        Line::from(vec![
            theme::label("address in URLs: "),
            theme::value(format!(
                "{} ({source})",
                address.map(|ip| ip.to_string()).unwrap_or_default()
            )),
            theme::label(" | i changes address"),
        ]),
        Line::from(theme::label(format!(
            "1–5 tabs | Tab: services / transfers | {} active file sessions | logs: 5",
            ui.app.sessions.active_count()
        ))),
    ];
    f.render_widget(
        Paragraph::new(lines).block(theme::panel_double(&format!(
            " TRANSFERBUDDY v{} ",
            crate::VERSION
        ))),
        chunks[0],
    );
    drop(cfg);
    draw_services(f, ui, chunks[1]);
    draw_sessions(f, ui, chunks[2]);
}

pub fn reboot_color(progress: &crate::upgrade::Progress) -> Color {
    match progress {
        crate::upgrade::Progress::Rebooting { since, .. } if since.elapsed().as_secs() >= 600 => {
            theme::ERR
        }
        crate::upgrade::Progress::Rebooting { since, .. } if since.elapsed().as_secs() >= 300 => {
            theme::WARN
        }
        crate::upgrade::Progress::Rebooting { .. } | crate::upgrade::Progress::Complete { .. } => {
            theme::OK
        }
        crate::upgrade::Progress::Failed(_) => theme::ERR,
        _ => theme::CYAN,
    }
}
fn scroll_hint(selected: usize, capacity: usize, total: usize) -> String {
    let start = selected.saturating_sub(capacity.saturating_sub(1));
    if total > capacity {
        format!(
            " | {}–{} / {total} ↑↓ scroll{}{}",
            start + 1,
            (start + capacity).min(total),
            if start > 0 { " ↑ more" } else { "" },
            if start + capacity < total {
                " ↓ more"
            } else {
                ""
            }
        )
    } else {
        format!(" | {total} devices")
    }
}
fn device_mode(info: &crate::cisco::VersionInfo) -> String {
    info.members
        .first()
        .map(|m| m.mode.clone())
        .or_else(|| {
            info.image.as_ref().map(|i| {
                if i.ends_with("packages.conf") {
                    "INSTALL".into()
                } else if i.ends_with(".bin") {
                    "BUNDLE".into()
                } else {
                    "?".into()
                }
            })
        })
        .unwrap_or_else(|| "?".into())
}
fn draw_connections(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let panes = Layout::vertical([
        Constraint::Length(5),
        Constraint::Min(6),
        Constraint::Length(8),
    ])
    .split(area);
    let mut header=vec![Line::from(theme::value("a: add | b: Bulk | r: Reconnect / refresh | c: CLI | Delete: remove")),Line::from(theme::label("SSH connections are shared by Transfer and Upgrade. No transfer protocol is selected here."))];
    if let Some(scan) = ui.app.switches.scan() {
        let p = scan.progress();
        header.push(Line::from(theme::label(format!("Subnet {}: {}/{} checked, {} reachable | failed SSH attempts hidden; reasons in Logs | S cancels",p.subnet,p.checked,p.total,p.reachable))));
    }
    f.render_widget(
        Paragraph::new(header).block(theme::panel(" CONNECT ")),
        panes[0],
    );
    let switches = ui.app.switches.list();
    ui.switch_sel = ui.switch_sel.min(switches.len().saturating_sub(1));
    let capacity = panes[1].height.saturating_sub(3) as usize;
    let start = ui.switch_sel.saturating_sub(capacity.saturating_sub(1));
    let rows = switches
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .map(|(i, sw)| {
            let facts = sw.facts();
            let info = facts.version.unwrap_or_default();
            let state = sw.state();
            Row::new(vec![
                Cell::from(sw.display_name()),
                Cell::from(sw.host.clone()),
                Cell::from(info.model.clone().unwrap_or_else(|| "?".into())),
                Cell::from(device_mode(&info)),
                Cell::from(info.version.unwrap_or_else(|| "?".into())),
                Cell::from(
                    facts
                        .flash
                        .map(|f| crate::switch::fmt_mb(f.free))
                        .unwrap_or_else(|| "?".into()),
                ),
                Cell::from(state.label()),
            ])
            .style(if i == ui.switch_sel {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            })
        });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(17),
                Constraint::Length(18),
                Constraint::Length(20),
                Constraint::Length(9),
                Constraint::Length(14),
                Constraint::Length(12),
                Constraint::Length(13),
            ],
        )
        .header(
            Row::new([
                "device",
                "address",
                "model",
                "mode",
                "version",
                "flash free",
                "SSH",
            ])
            .style(theme::header()),
        )
        .block(theme::panel(&format!(
            " CONNECTIONS{} ",
            scroll_hint(ui.switch_sel, capacity, switches.len())
        ))),
        panes[1],
    );
    let mut detail=vec![Line::from(theme::label("Select a device. Enter: session transcript | c: interactive CLI | r: reconnect with stored credentials."))];
    if let Some(sw) = super::selected_switch(ui) {
        let facts = sw.facts();
        let info = facts.version.unwrap_or_default();
        let progress = sw.upgrade();
        detail.push(Line::from(theme::value(format!(
            "{}:{} | user {} | uptime {}",
            sw.host,
            sw.port,
            sw.connection_username(),
            info.uptime.unwrap_or_else(|| "?".into())
        ))));
        detail.push(Line::from(theme::value(format!(
            "Storage {}: {} | image: {}",
            facts.flash_device,
            facts
                .flash
                .map(|m| format!(
                    "{} free / {} total",
                    crate::switch::fmt_mb(m.free),
                    crate::switch::fmt_mb(m.total)
                ))
                .unwrap_or_else(|| "?".into()),
            info.image.unwrap_or_else(|| "?".into())
        ))));
        detail.push(Line::from(vec![
            theme::label("Reachability: "),
            ping_span(&sw.reach()),
        ]));
        if let Some(d) = ui
            .app
            .engine()
            .snapshot()
            .devices
            .into_iter()
            .find(|d| d.id == sw.id)
        {
            detail.push(Line::from(theme::value(format!(
                "SSH: {} · {} retry attempts",
                d.recovery.label(),
                d.recovery.attempts
            ))));
        }
        detail.push(Line::from(Span::styled(
            progress.label(),
            Style::default().fg(reboot_color(&progress)),
        )));
        if let SwitchState::Failed { reason } | SwitchState::Offline { reason } = sw.state() {
            detail.push(Line::from(Span::styled(
                reason,
                Style::default().fg(theme::ERR),
            )));
        }
    }
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" DEVICE DETAILS ")),
        panes[2],
    );
}

fn draw_services(f: &mut Frame, ui: &Ui, area: Rect) {
    let chunks = Layout::vertical([Constraint::Length(9), Constraint::Min(0)]).split(area);
    let cfg = ui.app.config.read().unwrap();
    let rows: Vec<Row> = ServiceId::ALL
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let sc = cfg.service(*id);
            let status = ui.app.services.status(*id);
            let style = if i == ui.service_sel {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    if sc.enabled { "[x]" } else { "[ ]" },
                    Style::default().fg(if sc.enabled { theme::OK } else { theme::DIM }),
                )),
                Cell::from(id.display_name()),
                Cell::from(Span::styled(status.to_string(), status_style(&status))),
                Cell::from(format!("{}:{}", sc.bind, sc.port)),
                Cell::from(Span::styled(
                    if id.encrypted() { "" } else { "cleartext!" },
                    Style::default().fg(theme::WARN),
                )),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(9),
            Constraint::Length(40),
            Constraint::Length(22),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec!["on", "service", "status", "listen", "note"]).style(theme::header()))
    .block(theme::panel(
        " SERVICES — ↑↓ select / Space on/off / s start-stop / Enter edit ",
    ));
    f.render_widget(table, chunks[0]);

    // Detail pane for the selected service + shared auth/upload settings.
    let id = ServiceId::ALL[ui.service_sel.min(ServiceId::ALL.len() - 1)];
    let mut lines = vec![
        Line::from(vec![
            theme::label("auth (FTP/SFTP/SCP): "),
            Span::styled(
                format!("{} / {}", cfg.auth.username, cfg.auth.password),
                Style::default().fg(theme::ACCENT),
            ),
        ]),
        Line::from(vec![
            theme::label("uploads: "),
            theme::value(if cfg.uploads.enabled {
                format!(
                    "enabled → {}  (overwrite: {}, limit: {})",
                    if cfg.uploads.dir.is_empty() {
                        "<root>".into()
                    } else {
                        cfg.uploads.dir.clone()
                    },
                    if cfg.uploads.overwrite { "yes" } else { "no" },
                    if cfg.uploads.max_upload_mib == 0 {
                        "none".into()
                    } else {
                        format!("{} MiB", cfg.uploads.max_upload_mib)
                    }
                )
            } else {
                "disabled (downloads only)".into()
            }),
        ]),
    ];
    if id == ServiceId::Https {
        let info = https_info(&cfg);
        lines.push(Line::from(vec![
            theme::label("certificate: "),
            theme::value(info),
        ]));
    }
    if id == ServiceId::Ssh {
        let fp = ssh_info(&cfg);
        lines.push(Line::from(vec![
            theme::label("host key: "),
            theme::value(fp),
        ]));
    }
    if let ServiceStatus::Failed(e) = ui.app.services.status(id) {
        lines.push(Line::from(Span::styled(
            format!("error: {e}"),
            Style::default().fg(theme::ERR).bold(),
        )));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(theme::panel(&format!(
                " {} ",
                id.display_name().to_uppercase()
            ))),
        chunks[1],
    );
}

fn https_info(cfg: &crate::config::Config) -> String {
    use std::sync::OnceLock;
    static INFO: OnceLock<String> = OnceLock::new();
    INFO.get_or_init(|| match crate::certs::info(cfg) {
        Some(i) => format!("{}\nSHA-256: {}", i.path.display(), i.fingerprint_sha256),
        None => "generated on first HTTPS start".into(),
    })
    .clone()
}

fn ssh_info(cfg: &crate::config::Config) -> String {
    use std::sync::OnceLock;
    static INFO: OnceLock<String> = OnceLock::new();
    INFO.get_or_init(|| match crate::sshkeys::fingerprint(cfg) {
        Some(fp) => format!(
            "{} (SHA256:{fp})",
            crate::sshkeys::host_key_path(cfg).display()
        ),
        None => "generated on first SFTP/SCP start".into(),
    })
    .clone()
}

fn transfer_panel(title: &str, focused: bool) -> Block<'_> {
    theme::panel(title).border_style(Style::default().fg(if focused {
        theme::ACCENT
    } else {
        theme::DIM
    }))
}

fn draw_transfer_entries(
    f: &mut Frame,
    entries: &[super::FileEntry],
    selected: usize,
    focused: bool,
    title: &str,
    error: Option<&str>,
    area: Rect,
) {
    let height = area.height.saturating_sub(3) as usize;
    let offset = selected.saturating_sub(height.saturating_sub(1));
    let rows = entries
        .iter()
        .enumerate()
        .skip(offset)
        .take(height)
        .map(|(i, e)| {
            Row::new(vec![
                Cell::from(format!("{}{}", e.name, if e.is_dir { "/" } else { "" })),
                Cell::from(if e.is_dir {
                    "dir".into()
                } else {
                    fmt_bytes(e.size)
                }),
                Cell::from(e.ext.clone()),
            ])
            .style(if i == selected && focused {
                theme::selected()
            } else {
                Style::default().fg(if e.is_dir {
                    theme::CYAN
                } else if crate::upgrade::ios_file(&e.name) {
                    theme::WARN
                } else {
                    theme::TEXT
                })
            })
        });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(12),
                Constraint::Length(10),
                Constraint::Length(5),
            ],
        )
        .header(Row::new(["name", "size", "type"]).style(theme::header()))
        .block(transfer_panel(title, focused)),
        area,
    );
    if let Some(error) = error {
        let inner = Rect {
            x: area.x.saturating_add(1),
            y: area.y.saturating_add(2),
            width: area.width.saturating_sub(2),
            height: area.height.saturating_sub(3),
        };
        f.render_widget(
            Paragraph::new(error)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(theme::WARN)),
            inner,
        );
    }
}

fn draw_files(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let sections = Layout::vertical([
        Constraint::Length(13.min(area.height.saturating_sub(10)).max(4)),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);
    let switches = ui.app.switches.list();
    let capacity = sections[0].height.saturating_sub(3) as usize;
    let offset = ui.switch_sel.saturating_sub(capacity.saturating_sub(1));
    let rows = switches
        .iter()
        .enumerate()
        .skip(offset)
        .take(capacity)
        .map(|(i, sw)| {
            Row::new(vec![
                Cell::from(if i == ui.switch_sel { ">" } else { " " }),
                Cell::from(sw.display_name()),
                Cell::from(sw.host.clone()),
                Cell::from(sw.state().label()),
                Cell::from(if sw.protocol_chosen() {
                    sw.protocol().label()
                } else {
                    "choose"
                }),
                Cell::from(
                    sw.facts()
                        .version
                        .and_then(|v| v.model)
                        .unwrap_or_else(|| "?".into()),
                ),
            ])
            .style(if i == ui.switch_sel && ui.transfer_ui.focus == 0 {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            })
        });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Min(15),
                Constraint::Length(18),
                Constraint::Length(12),
                Constraint::Length(9),
                Constraint::Min(16),
            ],
        )
        .header(
            Row::new(["", "device", "address", "state", "protocol", "platform"])
                .style(theme::header()),
        )
        .block(transfer_panel(
            &format!(
                " DEVICES — connections: 2 / p protocol{} ",
                scroll_hint(ui.switch_sel, capacity, switches.len())
            ),
            ui.transfer_ui.focus == 0,
        )),
        sections[0],
    );
    if switches.is_empty() {
        f.render_widget(
            Paragraph::new("Connect devices in 2 Connect to use them here.")
                .style(Style::default().fg(theme::DIM)),
            Rect {
                x: sections[0].x + 1,
                y: sections[0].y + 2,
                width: sections[0].width.saturating_sub(2),
                height: 1,
            },
        );
    }
    let panes = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);
    let local: Vec<_> = ui.files.visible().into_iter().cloned().collect();
    let title = format!(
        " LOCAL /{} — {}{} ",
        ui.files.cwd,
        ui.files.sort.label(),
        if ui.files.filter.is_empty() {
            String::new()
        } else {
            format!(", filter: {}", ui.files.filter)
        }
    );
    draw_transfer_entries(
        f,
        &local,
        ui.files.selected,
        ui.transfer_ui.focus == 1,
        &title,
        ui.files.error.as_deref(),
        panes[0],
    );
    let sw = super::selected_switch(ui);
    let path = super::remote_path(ui);
    let listing = sw.as_ref().and_then(|s| s.listing(&path));
    let mut remote = Vec::new();
    if !ui.transfer_ui.remote_cwd.is_empty() {
        remote.push(super::FileEntry {
            name: "..".into(),
            is_dir: true,
            size: 0,
            modified: None,
            ext: String::new(),
        });
    }
    let error = match listing {
        Some(Ok(entries)) => {
            remote.extend(entries.into_iter().map(|e| super::FileEntry {
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
                modified: None,
                ext: String::new(),
            }));
            None
        }
        Some(Err(e)) => Some(e),
        None => Some(if sw.is_none() {
            "Connect a device to browse remote storage.".into()
        } else {
            "Waiting for remote directory listing…".into()
        }),
    };
    ui.transfer_ui.remote_selected = ui
        .transfer_ui
        .remote_selected
        .min(remote.len().saturating_sub(1));
    draw_transfer_entries(
        f,
        &remote,
        ui.transfer_ui.remote_selected,
        ui.transfer_ui.focus == 2,
        &format!(" REMOTE {path} "),
        error.as_deref(),
        panes[1],
    );
    let direction = if ui.transfer_ui.focus == 2 {
        "REMOTE → LOCAL"
    } else {
        "LOCAL → REMOTE"
    };
    let mut detail = format!(
        "t: {direction} via {} | p: protocol | Enter: open/copy | Backspace: up | Home: root",
        sw.as_ref()
            .map(|s| s.protocol().label())
            .unwrap_or("choose")
    );
    if let Some(sw) = sw {
        if let Some(transfer) = sw.transfer(&ui.app.sessions) {
            let state = transfer
                .session
                .as_ref()
                .map(|s| format!("{} / {} bytes", s.bytes, transfer.size))
                .unwrap_or_else(|| {
                    if transfer.ended.is_some() {
                        "finished".into()
                    } else {
                        "waiting for data".into()
                    }
                });
            detail.push_str(&format!(" | {}: {state}", transfer.rel_path));
        }
    }
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" TRANSFER ")),
        sections[2],
    );
}

fn draw_sessions(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let sessions = ui.app.sessions.snapshot();
    if ui.session_sel >= sessions.len() {
        ui.session_sel = sessions.len().saturating_sub(1);
    }
    let bits = ui.app.config.read().unwrap().speed_in_bits;
    let chunks = Layout::vertical([Constraint::Min(6), Constraint::Length(7)]).split(area);

    let rows: Vec<Row> = sessions
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let style = if i == ui.session_sel {
                theme::selected()
            } else if s.state.is_active() {
                Style::default().fg(theme::TEXT)
            } else {
                Style::default().fg(theme::DIM)
            };
            let progress = s
                .progress()
                .map(|p| format!("{:>3.0}%", p * 100.0))
                .unwrap_or_else(|| "-".into());
            let state_style = match &s.state {
                SessionState::Completed => Style::default().fg(theme::OK),
                SessionState::Failed(_) => Style::default().fg(theme::ERR),
                SessionState::Aborted => Style::default().fg(theme::WARN),
                _ => Style::default().fg(theme::HILITE),
            };
            Row::new(vec![
                Cell::from(format!("#{}", s.id)),
                Cell::from(Span::styled(
                    s.protocol.label(),
                    Style::default().fg(theme::CYAN),
                )),
                Cell::from(format!("{}", s.peer)),
                Cell::from(s.username.clone().unwrap_or_default()),
                Cell::from(s.file.clone().unwrap_or_default()),
                Cell::from(s.direction.map(|d| d.label()).unwrap_or("-")),
                Cell::from(Span::styled(s.state.label().to_string(), state_style)),
                Cell::from(progress),
                Cell::from(fmt_speed(s.current_speed, bits)),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(5),
            Constraint::Length(6),
            Constraint::Length(21),
            Constraint::Length(10),
            Constraint::Min(18),
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec![
            "id", "proto", "source", "user", "file", "dir", "state", "prog", "speed",
        ])
        .style(theme::header()),
    )
    .block(theme::panel(&format!(
        " SESSIONS ({} active, {} shown) ",
        sessions.iter().filter(|s| s.state.is_active()).count(),
        sessions.len()
    )));
    f.render_widget(table, chunks[0]);

    // Detail pane.
    let mut lines = Vec::new();
    if let Some(s) = sessions.get(ui.session_sel) {
        let started = DateTime::<Local>::from(s.started_wall)
            .format("%H:%M:%S")
            .to_string();
        lines.push(Line::from(Span::styled(
            format!(
                "#{}  {}  {} → local port {}   user: {}",
                s.id,
                s.protocol.label(),
                s.peer,
                s.local_port,
                s.username.clone().unwrap_or_else(|| "-".into())
            ),
            Style::default().fg(theme::HILITE).bold(),
        )));
        lines.push(Line::from(vec![
            theme::label("file: "),
            theme::value(s.file.clone().unwrap_or_else(|| "-".into())),
            theme::label("   direction: "),
            theme::value(s.direction.map(|d| d.label()).unwrap_or("-")),
        ]));
        lines.push(Line::from(vec![
            theme::label("transferred: "),
            theme::value(format!(
                "{} / {}",
                fmt_bytes(s.bytes),
                s.total.map(fmt_bytes).unwrap_or_else(|| "?".into())
            )),
            theme::label("   progress: "),
            theme::value(
                s.progress()
                    .map(|p| format!("{:.1}%", p * 100.0))
                    .unwrap_or_else(|| "-".into()),
            ),
        ]));
        lines.push(Line::from(vec![
            theme::label("speed: "),
            theme::value(format!(
                "{} now, {} avg",
                fmt_speed(s.current_speed, bits),
                fmt_speed(s.avg_speed(), bits)
            )),
            theme::label("   started: "),
            theme::value(started),
            theme::label("   duration: "),
            theme::value(fmt_duration(s.duration())),
            theme::label("   eta: "),
            theme::value(s.eta().map(fmt_duration).unwrap_or_else(|| "-".into())),
        ]));
        if let SessionState::Failed(e) = &s.state {
            lines.push(Line::from(Span::styled(
                format!("error: {e}"),
                Style::default().fg(theme::ERR).bold(),
            )));
        }
    } else {
        lines.push(Line::from(theme::label("no sessions yet")));
    }
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" DETAILS ")),
        chunks[1],
    );
}

fn wrap_log_line(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let columns = Span::raw(ch.to_string()).width();
        if used + columns > width && !line.is_empty() {
            lines.push(std::mem::take(&mut line));
            used = 0;
        }
        line.push(ch);
        used += columns;
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn draw_logs(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let entries = ui.app.logger.entries();
    let filter = ui.log_filter.to_lowercase();
    let filtered: Vec<&crate::logging::LogEntry> = entries
        .iter()
        .filter(|e| e.level >= ui.log_min_level)
        .filter(|e| match ui.log_proto_filter {
            Some(id) => {
                e.proto == id.log_proto()
                    || (id == ServiceId::Ssh && (e.proto == "sftp" || e.proto == "scp"))
            }
            None => true,
        })
        .filter(|e| {
            if filter.is_empty() {
                return true;
            }
            e.render_line().to_lowercase().contains(&filter)
        })
        .collect();

    let height = area.height.saturating_sub(2) as usize;
    // Wrap long structured entries so size and speed remain visible on normal
    // terminal widths; scroll and tail follow operate on the rendered rows.
    let width = area.width.saturating_sub(2).max(1) as usize;
    let wrapped: Vec<Line> = filtered
        .iter()
        .flat_map(|entry| {
            wrap_log_line(&entry.render_line(), width)
                .into_iter()
                .map(|line| {
                    Line::from(Span::styled(
                        line,
                        Style::default().fg(level_color(entry.level)),
                    ))
                })
        })
        .collect();
    let max_scroll = wrapped.len().saturating_sub(height);
    if ui.log_follow || ui.log_scroll > max_scroll {
        ui.log_scroll = max_scroll;
    }
    let lines: Vec<Line> = wrapped
        .into_iter()
        .skip(ui.log_scroll)
        .take(height.max(1))
        .collect();
    let title = format!(
        " LOGS (level ≥ {}{}{}){} ",
        ui.log_min_level.label(),
        match ui.log_proto_filter {
            Some(id) => format!(", proto: {}", id.log_proto()),
            None => String::new(),
        },
        if ui.log_filter.is_empty() {
            String::new()
        } else {
            format!(", filter: \"{}\"", ui.log_filter)
        },
        if ui.log_follow { " ▼ follow" } else { "" }
    );
    f.render_widget(Paragraph::new(lines).block(theme::panel(&title)), area);
}

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2));
    Rect::new(
        area.x + (area.width.saturating_sub(w)) / 2,
        area.y + (area.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

/// One key and what it does. Rendered one per line, keys highlighted.
type KeyRows = &'static [(&'static str, &'static str)];

/// Left help table.
const HELP_LEFT: &[(&str, KeyRows)] = &[
    (
        "DASHBOARD (1)",
        &[
            ("Tab", "services / transfer monitor"),
            ("↑ ↓", "select service"),
            ("Enter", "edit service"),
            ("Space", "enable / disable"),
            ("s / r", "start-stop / restart"),
            ("S / X", "start enabled / stop all"),
            ("p / b", "port / bind address"),
            ("n / w", "username / password"),
            ("u / d", "uploads / upload directory"),
            ("i / L", "URL address / service logs"),
        ],
    ),
    (
        "CONNECT (2)",
        &[
            ("a / b", "add / bulk or subnet"),
            ("Enter", "device console"),
            ("c", "interactive switch CLI"),
            ("Esc", "leave interactive CLI"),
            ("r", "refresh / reconnect"),
            ("x / X", "disconnect / clear closed"),
            ("S", "cancel subnet scan"),
            ("y", "trust SSH host key"),
        ],
    ),
    (
        "TRANSFER (3)",
        &[
            ("Tab", "devices / local / remote"),
            ("↑ ↓", "select device or entry"),
            ("Enter", "open dir / copy file"),
            ("⌫/Home", "parent / root directory"),
            ("t / p", "transfer / choose protocol"),
            ("Del/D", "delete remote; y confirms"),
            ("H", "hashes + compare"),
            ("s / R", "sort / refresh listing"),
            ("/", "filter by name"),
        ],
    ),
];
const HELP_RIGHT: &[(&str, KeyRows)] = &[
    (
        "GLOBAL",
        &[
            ("J", "jobs: cancel / retry / resume"),
            ("1–5", "select tab"),
            ("h / ?", "this help"),
            ("m", "sound on / off"),
            ("q / ^C", "quit"),
        ],
    ),
    (
        "UPGRADE (4)",
        &[
            ("Tab", "file browser / device jobs"),
            ("Enter", "choose file / device actions"),
            ("a / A", "assign image to one / all"),
            ("p", "choose transfer protocol"),
            ("d / V", "deploy / verify existing"),
            ("u", "install; confirm reload"),
            ("Y", "YOLO; automatic reload"),
            ("i", "install remove inactive"),
            ("v", "device console / confirmation"),
            ("⌫/Home", "parent / root directory"),
        ],
    ),
    (
        "CONSOLE / CONNECTION FORM",
        &[
            ("↑ ↓", "scroll / form field"),
            ("Enter", "next field / submit"),
            ("^Enter", "submit from any field"),
            ("y", "confirm reload / cleanup"),
            ("Esc", "close / decline confirmation"),
        ],
    ),
    (
        "LOGS (5)",
        &[
            ("↑ ↓", "scroll"),
            ("PgUp", "page up"),
            ("PgDn", "page down"),
            ("G", "follow tail"),
            ("/", "filter text"),
            ("L", "minimum level"),
            ("P", "protocol filter"),
        ],
    ),
];

/// Render one help column: section headers plus one key per line, with the
/// same indentation and key column width in both columns.
fn help_column(sections: &[(&str, KeyRows)]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (title, rows) in sections {
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            format!(" {title}"),
            Style::default().fg(theme::HILITE).bold(),
        )));
        for (key, desc) in rows.iter() {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{key:<7}"),
                    Style::default().fg(theme::ACCENT).bold(),
                ),
                Span::styled(desc.to_string(), Style::default().fg(theme::TEXT)),
            ]));
        }
    }
    lines
}

fn draw_help(f: &mut Frame, help_scroll: usize) {
    let left = help_column(HELP_LEFT);
    let right = help_column(HELP_RIGHT);
    let rows = left.len().max(right.len());

    let mut area = centered_rect(80, rows as u16 + 3, f.area());
    // Give the key tables the extra row when they fit the terminal exactly.
    area.height = (rows as u16 + 3).min(f.area().height);
    area.y = f.area().y + f.area().height.saturating_sub(area.height) / 2;
    f.render_widget(Clear, area);
    let block = theme::panel_double(" HELP — one key per line ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Scroll when the terminal is too short for the full tables.
    let visible = inner.height.saturating_sub(1) as usize;
    let max_scroll = rows.saturating_sub(visible);
    let scroll = help_scroll.min(max_scroll);
    let take = |lines: Vec<Line<'static>>| -> Vec<Line<'static>> {
        lines
            .into_iter()
            .skip(scroll)
            .take(visible.max(1))
            .collect()
    };

    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).split(
        Rect::new(
            inner.x,
            inner.y,
            inner.width,
            inner.height.saturating_sub(1),
        ),
    );
    f.render_widget(Paragraph::new(take(left)), cols[0]);
    f.render_widget(
        Paragraph::new(take(right)).block(
            Block::default()
                .borders(Borders::LEFT)
                .border_style(Style::default().fg(theme::FRAME)),
        ),
        cols[1],
    );

    let hint = if max_scroll > 0 {
        " ↑↓ scroll · any other key closes "
    } else {
        " press any key to close "
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(theme::DIM),
        )))
        .alignment(Alignment::Center),
        Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
    );
}

/// The deploy popup: the form that starts a session. What happens afterwards
/// is shown by [`draw_session`], which the switches view opens as well.
fn draw_deploy(f: &mut Frame, ui: &Ui) {
    let Some(view) = &ui.deploy else { return };
    let width = f.area().width.min(112);
    // Fields, the command preview, an optional error and the two hints.
    let height = f
        .area()
        .height
        .saturating_sub(2)
        .min(super::deploy_fields(ui.deploy_mode).len() as u16 + 14);
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let title = match ui.deploy_mode {
        super::DeployMode::Copy => format!(" DEPLOY  /{} ", view.rel_path),
        super::DeployMode::Add => " ADD DEVICE — SSH connection check ".into(),
        super::DeployMode::Bulk => " BULK IMPORT — SSH connection check ".into(),
    };
    let block = theme::panel_double(&title);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let open_session = ui.app.switches.find_live(
        view.form.host.trim(),
        view.form.port.trim().parse().unwrap_or(0),
        view.form.username.trim(),
    );
    let mut lines = Vec::new();
    for (i, field) in super::deploy_fields(ui.deploy_mode).iter().enumerate() {
        let value = match field {
            DeployField::Host => view.form.host.replace(['\n', '\r'], " ; "),
            DeployField::Port => view.form.port.clone(),
            DeployField::Username => view.form.username.clone(),
            DeployField::Password | DeployField::EnablePassword => {
                let typed = if *field == DeployField::Password {
                    &view.form.password
                } else {
                    &view.form.enable_password
                };
                if typed.is_empty() && open_session.is_some() {
                    "— session already open".to_string()
                } else {
                    "•".repeat(typed.chars().count())
                }
            }
            DeployField::Destination => view.form.dest.clone(),
            DeployField::Overwrite => {
                if view.form.overwrite {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            DeployField::Submit => match ui.deploy_mode {
                super::DeployMode::Bulk => "[ Enter ]  Add devices".into(),
                _ => "[ Enter ]  Add device".into(),
            },
            DeployField::Protocol => {
                let id = crate::cisco::service_of(view.form.proto);
                let status = ui.app.services.status(id);
                format!("{}  ({status})", view.form.proto.label())
            }
        };
        let selected = i == view.form.field;
        let limit = inner.width.saturating_sub(21) as usize;
        let chars: Vec<char> = value.chars().collect();
        let value = if chars.len() > limit && limit > 1 {
            if selected {
                format!(
                    "…{}",
                    chars[chars.len() - limit + 1..].iter().collect::<String>()
                )
            } else {
                format!("{}…", chars[..limit - 1].iter().collect::<String>())
            }
        } else {
            value
        };
        let shown = if selected && field.is_text() {
            format!("{value}{}", if cursor_on() { "_" } else { " " })
        } else {
            value
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    " {:<18}",
                    if *field == DeployField::Host && ui.deploy_mode == super::DeployMode::Bulk {
                        "device IP / Subnet:"
                    } else {
                        field.label()
                    }
                ),
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled(
                shown,
                if *field == DeployField::Protocol {
                    let status = ui
                        .app
                        .services
                        .status(crate::cisco::service_of(view.form.proto));
                    let style = status_style(&status);
                    if selected {
                        style.bg(theme::SEL_BG)
                    } else {
                        style
                    }
                } else if selected {
                    theme::selected_strong()
                } else {
                    Style::default().fg(theme::TEXT)
                },
            ),
        ]));
    }
    if ui.deploy_mode == super::DeployMode::Copy {
        if ui.tab == Tab::Upgrade {
            if let Some(warning) = open_session
                .as_ref()
                .and_then(|s| s.facts().version)
                .as_ref()
                .and_then(|v| crate::cisco::platform_warning(v, &view.rel_path))
            {
                lines.push(Line::from(Span::styled(
                    warning,
                    Style::default().fg(theme::WARN).bold(),
                )));
            }
        }
        // What the device has room for, when a session already told us.
        if let Some(usage) = open_session.as_ref().and_then(|s| s.facts().flash) {
            let fits = usage.fits(view.size);
            lines.push(Line::from(vec![
                theme::label(" image             "),
                theme::value(crate::switch::fmt_mb(view.size)),
                theme::label("   free  "),
                theme::value(crate::switch::fmt_mb(usage.free)),
                Span::styled(
                    if fits { "   fits" } else { "   DOES NOT FIT" },
                    Style::default()
                        .fg(if fits { theme::OK } else { theme::ERR })
                        .bold(),
                ),
            ]));
        }

        lines.push(Line::from(""));
        match super::deploy_command_for(ui, &view.form, &view.rel_path) {
            Ok(cmd) => lines.push(Line::from(vec![
                theme::label(" the switch will run  "),
                Span::styled(cmd, Style::default().fg(theme::CYAN).bold()),
            ])),
            Err(e) => lines.push(Line::from(Span::styled(
                format!(" {e}"),
                Style::default().fg(theme::WARN),
            ))),
        }
    } else {
        lines.push(Line::from(""));
        if ui.deploy_mode == super::DeployMode::Bulk {
            lines.push(Line::from(theme::label(" IPs and/or subnets: 192.168.22.0/24, 192.168.11.11; credentials apply to all devices.")));
            lines.push(Line::from(Span::styled(
                " Host-key acceptance follows saved settings (automatic by default).",
                Style::default().fg(theme::WARN),
            )));
        }
        lines.push(Line::from(theme::label(
            " Opens and maintains SSH; transfer protocols are selected in Transfer or Upgrade.",
        )));
    }
    if ui.pending_deploy.is_some() {
        lines.push(Line::from(theme::value(
            " Starting server… deploy begins once it is listening. Esc cancels.",
        )));
    }
    if let Some(err) = &view.error {
        lines.push(Line::from(Span::styled(
            format!(" {err}"),
            Style::default().fg(theme::ERR).bold(),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        theme::key("↑↓"),
        theme::label(" field  "),
        theme::key("←→/Space"),
        theme::label(if ui.deploy_mode == super::DeployMode::Copy {
            " protocol, overwrite  "
        } else {
            " "
        }),
        theme::key("Enter"),
        theme::label(if ui.deploy_mode == super::DeployMode::Copy {
            " protocol / submit  "
        } else {
            " next / select / confirm  "
        }),
        theme::key("Ctrl+Enter"),
        theme::label(" submit  "),
        theme::key("Esc"),
        theme::label(" close"),
    ]));
    if ui.deploy_mode == super::DeployMode::Copy {
        lines.push(Line::from(theme::label(" Ctrl+s starts the selected server. Ctrl+Enter deploys from any field — never a config command.")));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// Live view of one switch session: what it knows about the device, and the
/// transcript of everything typed and answered.
fn draw_session(f: &mut Frame, ui: &Ui) {
    let Some(view) = &ui.session_view else { return };
    let switch = &view.switch;
    let width = f.area().width.min(120);
    let height = f.area().height.saturating_sub(2).min(38);
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = theme::panel_double(&format!(
        " SWITCH  {}  ({}) ",
        switch.display_name(),
        switch.host
    ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if let SwitchState::ReloadConfirm { prompt } = switch.state() {
        f.render_widget(
            Paragraph::new(format!(
                "\nDevice: {} ({})\n\n{prompt}\n\nThe device will reboot. y: confirm reload | any other key: decline",
                switch.display_name(), switch.host
            ))
            .wrap(Wrap { trim: false })
            .block(theme::panel_double(" CONFIRM RELOAD ")),
            area,
        );
        return;
    }
    if let SwitchState::HostKey { fingerprint } = switch.state() {
        draw_host_key(f, switch, &fingerprint, inner);
        return;
    }

    if let SwitchState::CleanupConfirm { files, warning } = switch.state() {
        let regions = Layout::vertical([
            Constraint::Length(8),
            Constraint::Min(1),
            Constraint::Length(2),
        ])
        .split(inner);
        let version = switch.facts().version.unwrap_or_default();
        let mut lines = vec![
            Line::from(theme::value(" install remove inactive — confirm deletion")),
            Line::from(theme::label(format!(
                " Running IOS: {} | stack: {}",
                version.version.as_deref().unwrap_or("unknown"),
                version
                    .members
                    .iter()
                    .map(|m| format!("{}: {}", m.number, m.version))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        ];
        lines.push(Line::from(Span::styled(
            warning.unwrap_or_else(|| {
                "No candidate matches the running IOS or active system image.".into()
            }),
            Style::default()
                .fg(if cleanup_has_warning(switch) {
                    theme::ERR
                } else {
                    theme::OK
                })
                .bold(),
        )));
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), regions[0]);
        let lines: Vec<Line> = files
            .into_iter()
            .map(|file| Line::from(theme::value(format!(" {file}"))))
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .block(theme::panel(" FILES TO DELETE ")),
            regions[1],
        );
        f.render_widget(
            Paragraph::new(
                " y: confirm deletion     ANY OTHER KEY: abort
 Warning: files are permanently removed from the switch.",
            )
            .style(Style::default().fg(theme::WARN).bold()),
            regions[2],
        );
        return;
    }

    let mut summary = switch_summary(switch);
    summary.extend(transfer_summary(ui, switch));
    let summary_height = (summary.len() as u16)
        .max(7)
        .min(inner.height.saturating_sub(5));
    let chunks = Layout::vertical([
        Constraint::Length(summary_height),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: false }),
        chunks[0],
    );

    // Transcript, tail-following unless the user scrolled up.
    let rows = chunks[1].height.saturating_sub(2) as usize;
    let transcript = switch.transcript();
    let live = switch.live();
    let mut body: Vec<(LineKind, &str)> = transcript
        .iter()
        .map(|l| (l.kind, l.text.as_str()))
        .collect();
    if !live.is_empty() {
        body.push((LineKind::Output, live.as_str()));
    }
    let start = match view.scroll {
        Some(s) => s.min(body.len().saturating_sub(1)),
        None => body.len().saturating_sub(rows),
    };
    let lines: Vec<Line> = body[start..]
        .iter()
        .take(rows)
        .map(|(kind, text)| {
            let style = match kind {
                LineKind::Sent => Style::default().fg(theme::ACCENT).bold(),
                LineKind::Info => Style::default().fg(theme::CYAN),
                LineKind::Error => Style::default().fg(theme::ERR).bold(),
                LineKind::Output if text.starts_with('%') => Style::default().fg(theme::WARN),
                LineKind::Output => Style::default().fg(theme::TEXT),
            };
            let marker = if *kind == LineKind::Sent { " > " } else { " " };
            Line::from(Span::styled(format!("{marker}{text}"), style))
        })
        .collect();
    f.render_widget(
        Paragraph::new(lines).block(theme::panel(" SESSION ")),
        chunks[1],
    );

    let mut hint = vec![Span::raw(" ")];
    if matches!(switch.state(), SwitchState::Busy { .. }) {
        hint.push(theme::key("c"));
        hint.push(theme::label(" cancel   "));
    }
    for (key, desc) in [
        ("i", " remove inactive   "),
        ("r", " refresh   "),
        ("x", " disconnect   "),
        ("↑↓", " scroll   "),
    ] {
        hint.push(theme::key(key));
        hint.push(theme::label(desc));
    }
    hint.push(theme::key("Esc"));
    hint.push(theme::label(" close"));
    f.render_widget(Paragraph::new(Line::from(hint)), chunks[2]);
}

fn cleanup_has_warning(switch: &crate::switch::Switch) -> bool {
    matches!(
        switch.state(),
        SwitchState::CleanupConfirm {
            warning: Some(_),
            ..
        }
    )
}

/// Header of the session view: state, device facts, flash and ping.
fn switch_summary(switch: &crate::switch::Switch) -> Vec<Line<'static>> {
    let state = switch.state();
    let (state_text, state_color) = match &state {
        SwitchState::Busy { what } => (what.clone(), theme::HILITE),
        SwitchState::Ready => match switch.last_result() {
            Some(Ok(summary)) => (format!("ready — {summary}"), theme::OK),
            Some(Err(e)) => (format!("ready — last job failed: {e}"), theme::ERR),
            None => ("ready".into(), theme::OK),
        },
        SwitchState::Failed { reason } | SwitchState::Offline { reason } => {
            (format!("{} — {reason}", state.label()), theme::ERR)
        }
        other => (other.label().to_string(), theme::TEXT),
    };
    let facts = switch.facts();
    let reach = switch.reach();

    let mut lines = vec![Line::from(vec![
        theme::label(" state    "),
        Span::styled(state_text, Style::default().fg(state_color).bold()),
    ])];

    let progress = switch.upgrade();
    if !matches!(progress, crate::upgrade::Progress::Idle) {
        lines.push(Line::from(vec![
            theme::label(" upgrade  "),
            Span::styled(
                progress.label(),
                Style::default().fg(reboot_color(&progress)).bold(),
            ),
        ]));
    }

    let version = facts.version.clone().unwrap_or_default();
    lines.push(Line::from(vec![
        theme::label(" device   "),
        theme::value(version.model.clone().unwrap_or_else(|| "?".into())),
        theme::label("   IOS-XE "),
        Span::styled(
            version.version.clone().unwrap_or_else(|| "?".into()),
            Style::default().fg(theme::CYAN).bold(),
        ),
        theme::label("   uptime "),
        theme::value(version.uptime.clone().unwrap_or_else(|| "?".into())),
    ]));

    let mut third = vec![theme::label(" flash    ")];
    match facts.flash {
        Some(usage) => {
            third.push(Span::styled(
                crate::switch::fmt_mb(usage.free),
                Style::default()
                    .fg(if usage.used_fraction() > 0.9 {
                        theme::WARN
                    } else {
                        theme::OK
                    })
                    .bold(),
            ));
            third.push(theme::label(format!(
                " free of {} ({:.0}% used) on {}",
                crate::switch::fmt_mb(usage.total),
                usage.used_fraction() * 100.0,
                facts.flash_device
            )));
        }
        None => third.push(theme::label("not read yet")),
    }
    third.push(theme::label("   ping "));
    third.push(ping_span(&reach));
    lines.push(Line::from(third));

    if !version.members.is_empty() {
        let odd = version.members_off_version();
        let text = version
            .members
            .iter()
            .map(|m| {
                format!(
                    "{}{}:{}",
                    if m.active { "*" } else { "" },
                    m.number,
                    m.version
                )
            })
            .collect::<Vec<_>>()
            .join("  ");
        lines.push(Line::from(vec![
            theme::label(" stack    "),
            Span::styled(
                text,
                Style::default()
                    .fg(if odd.is_empty() {
                        theme::TEXT
                    } else {
                        theme::WARN
                    })
                    .bold(),
            ),
            theme::label(if odd.is_empty() {
                ""
            } else {
                "   ← members differ from the system version"
            }),
        ]));
    }
    lines
}

/// Ping state as one coloured span.
fn ping_span(reach: &crate::switch::Reach) -> Span<'static> {
    match (reach.online, reach.rtt, reach.down_since) {
        (true, Some(rtt), _) => Span::styled(
            format!("{:.1} ms", rtt.as_secs_f64() * 1000.0),
            Style::default().fg(theme::OK),
        ),
        (true, None, _) => Span::styled("up", Style::default().fg(theme::OK)),
        (false, _, Some(since)) => Span::styled(
            format!("down {}", fmt_duration(since.elapsed())),
            Style::default().fg(theme::ERR).bold(),
        ),
        (false, _, None) => Span::styled("—", Style::default().fg(theme::DIM)),
    }
}

fn draw_host_key(f: &mut Frame, switch: &crate::switch::Switch, fingerprint: &str, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            " First connection to this switch — unknown host key:",
            Style::default().fg(theme::WARN).bold(),
        )),
        Line::from(""),
        Line::from(vec![
            theme::label("   host         "),
            theme::value(format!("{}:{}", switch.host, switch.port)),
        ]),
        Line::from(vec![
            theme::label("   fingerprint  "),
            Span::styled(
                fingerprint.to_string(),
                Style::default().fg(theme::CYAN).bold(),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            theme::key("y"),
            theme::label(" trust and remember it      "),
            theme::key("any other key"),
            theme::label(" abort"),
        ]),
    ];
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// The switches view: one row per open SSH session to a device.
fn draw_switches(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let sections = Layout::vertical([
        Constraint::Length(4),
        Constraint::Percentage(38),
        Constraint::Min(6),
        Constraint::Length(7),
    ])
    .split(area);
    let protocol = ui.upgrade_protocol;
    let service = ui.app.services.status(crate::cisco::service_of(protocol));
    let selected = ui
        .upgrade_file
        .as_ref()
        .map(|(p, _)| p.as_str())
        .unwrap_or("select a file above");
    f.render_widget(Paragraph::new(vec![Line::from(theme::value(format!("Protocol: {} [{}] — p selects | file: /{selected}",protocol.label(),service))),Line::from(theme::label("Tab: files / jobs | Enter: choose file | a: assign selected device | A: assign all | d: deploy + MD5"))]).block(theme::panel(" UPGRADE — INSTALL MODE ")),sections[0]);
    let files: Vec<_> = ui.files.visible().into_iter().cloned().collect();
    draw_transfer_entries(
        f,
        &files,
        ui.files.selected,
        ui.upgrade_menu.is_some(),
        &format!(
            " FILE BROWSER /{} — Backspace up / Home root ",
            ui.files.cwd
        ),
        ui.files.error.as_deref(),
        sections[1],
    );
    let switches = ui.app.switches.list();
    let capacity = (sections[2].height.saturating_sub(3) as usize / 2).max(1);
    let offset = ui.switch_sel.saturating_sub(capacity.saturating_sub(1));
    // Match the fixed final table column, accounting for the frame and header.
    if sections[2].width >= 110 {
        let x = sections[2].right().saturating_sub(25);
        for (row, sw) in switches.iter().skip(offset).take(capacity).enumerate() {
            let y = sections[2].y + 2 + row as u16 * 2;
            if y + 1 >= sections[2].bottom().saturating_sub(1) {
                break;
            }
            let info = sw.facts().version.unwrap_or_default();
            let progress = sw.upgrade();
            let allowed = sw.state() == SwitchState::Ready
                && crate::upgrade::install_blocker(&progress, &info).is_none();
            if allowed {
                ui.upgrade_buttons.push(super::UpgradeButton {
                    area: Rect::new(x, y, 11, 1),
                    switch: sw.clone(),
                    action: 2,
                });
            } else if !matches!(
                progress,
                crate::upgrade::Progress::Verified { .. }
                    | crate::upgrade::Progress::Complete { .. }
            ) {
                ui.upgrade_buttons.push(super::UpgradeButton {
                    area: Rect::new(x, y, 10, 1),
                    switch: sw.clone(),
                    action: 0,
                });
                ui.upgrade_buttons.push(super::UpgradeButton {
                    area: Rect::new(x + 11, y, 10, 1),
                    switch: sw.clone(),
                    action: 1,
                });
            }
            let assigned = ui
                .upgrade_assignments
                .get(&sw.id)
                .or(ui.upgrade_file.as_ref());
            let no_space = assigned
                .zip(sw.facts().flash)
                .is_some_and(|((_, size), flash)| !flash.fits(*size));
            ui.upgrade_buttons.push(super::UpgradeButton {
                area: Rect::new(x, y + 1, if no_space { 19 } else { 16 }, 1),
                switch: sw.clone(),
                action: if no_space { 4 } else { usize::MAX },
            });
        }
    }
    let rows = switches
        .iter()
        .enumerate()
        .skip(offset)
        .take(capacity)
        .map(|(i, sw)| {
            let facts = sw.facts();
            let info = facts.version.clone().unwrap_or_default();
            let progress = sw.upgrade();
            let assigned = ui
                .upgrade_assignments
                .get(&sw.id)
                .or(ui.upgrade_file.as_ref());
            let no_space = assigned
                .zip(facts.flash)
                .is_some_and(|((_, size), flash)| !flash.fits(*size));
            let allowed = sw.state() == SwitchState::Ready
                && crate::upgrade::install_blocker(&progress, &info).is_none();
            let (percent, speed, eta) =
                upgrade_metrics(ui, sw, assigned.map(|(path, _)| path.as_str()));
            let action = if allowed {
                "[u Upgrade]"
            } else if matches!(
                progress,
                crate::upgrade::Progress::Verified { .. }
                    | crate::upgrade::Progress::Complete { .. }
            ) {
                "[Upgrade disabled]"
            } else {
                "[d Deploy] [V Verify]"
            };
            let extra = if no_space {
                "[i Remove inactive]"
            } else {
                "[Enter: actions]"
            };
            Row::new(vec![
                Cell::from(Text::from(vec![
                    Line::from(sw.display_name()),
                    Line::from(info.model.unwrap_or_else(|| "?".into())),
                ])),
                Cell::from(Text::from(vec![
                    Line::from(
                        assigned
                            .map(|(p, _)| p.clone())
                            .unwrap_or_else(|| "assign image: a / A".into()),
                    ),
                    Line::from(Span::styled(
                        format!(
                            "IOS {} | {}",
                            info.version.unwrap_or_else(|| "?".into()),
                            progress.label()
                        ),
                        Style::default().fg(reboot_color(&progress)),
                    )),
                ])),
                Cell::from(Text::from(vec![
                    Line::from(
                        facts
                            .flash
                            .map(|f| crate::switch::fmt_mb(f.free))
                            .unwrap_or_else(|| "?".into()),
                    ),
                    Line::from(Span::styled(
                        if no_space { "NO SPACE" } else { "" },
                        Style::default().fg(theme::ERR),
                    )),
                ])),
                Cell::from(Text::from(vec![
                    Line::from(format!("{percent} | {speed}")),
                    Line::from(format!("ETA {eta}")),
                ])),
                Cell::from(Text::from(vec![
                    Line::from(Span::styled(
                        action,
                        Style::default().fg(if allowed { theme::OK } else { theme::CYAN }),
                    )),
                    Line::from(Span::styled(
                        extra,
                        Style::default().fg(if no_space { theme::ERR } else { theme::DIM }),
                    )),
                ])),
            ])
            .height(2)
            .style(if i == ui.switch_sel && ui.upgrade_menu.is_none() {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            })
        });
    f.render_widget(
        Table::new(
            rows,
            [
                Constraint::Min(18),
                Constraint::Percentage(30),
                Constraint::Length(12),
                Constraint::Min(20),
                Constraint::Length(24),
            ],
        )
        .header(
            Row::new([
                "device / model",
                "assigned image / version",
                "flash free",
                "upload % / speed / ETA",
                "actions",
            ])
            .style(theme::header()),
        )
        .block(transfer_panel(
            &format!(
                " DEVICE JOBS{} ",
                scroll_hint(ui.switch_sel, capacity, switches.len())
            ),
            ui.upgrade_menu.is_none(),
        )),
        sections[2],
    );
    let mut detail=vec![Line::from(theme::label("Enter: device actions | d: deploy | V: verify existing | u/Y: upgrade/YOLO | i: remove inactive | v: console"))];
    if let Some(sw) = super::selected_switch(ui) {
        let progress = sw.upgrade();
        detail.push(Line::from(Span::styled(
            progress.label(),
            Style::default().fg(reboot_color(&progress)),
        )));
        if let Some(reason) =
            crate::upgrade::install_blocker(&progress, &sw.facts().version.unwrap_or_default())
        {
            detail.push(Line::from(Span::styled(
                reason,
                Style::default().fg(theme::WARN),
            )));
        }
        if let Some((path, size)) = ui
            .upgrade_assignments
            .get(&sw.id)
            .or(ui.upgrade_file.as_ref())
        {
            if let Some(usage) = sw.facts().flash {
                detail.push(Line::from(Span::styled(
                    format!(
                        "Image {} | storage {} free / {} total | {}",
                        crate::switch::fmt_mb(*size),
                        crate::switch::fmt_mb(usage.free),
                        crate::switch::fmt_mb(usage.total),
                        if usage.fits(*size) {
                            "fits"
                        } else {
                            "NO SPACE"
                        }
                    ),
                    Style::default().fg(if usage.fits(*size) {
                        theme::TEXT
                    } else {
                        theme::ERR
                    }),
                )));
            }
            if let Some(warning) = sw
                .facts()
                .version
                .as_ref()
                .and_then(|v| crate::cisco::platform_warning(v, path))
            {
                detail.push(Line::from(Span::styled(
                    warning,
                    Style::default().fg(theme::WARN),
                )));
            }
        }
    }
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" SELECTED JOB ")),
        sections[3],
    );
}

fn upgrade_metrics(
    ui: &Ui,
    sw: &crate::switch::Switch,
    assigned: Option<&str>,
) -> (String, String, String) {
    let verified = matches!(
        sw.upgrade(),
        crate::upgrade::Progress::Verified { .. }
            | crate::upgrade::Progress::Installing
            | crate::upgrade::Progress::AwaitingReload { .. }
            | crate::upgrade::Progress::Rebooting { .. }
            | crate::upgrade::Progress::Complete { .. }
    );
    let transfer = sw
        .transfer(&ui.app.sessions)
        .filter(|t| assigned.is_some_and(|path| path == t.rel_path));
    let bits = ui.app.config.read().unwrap().speed_in_bits;
    if let Some(transfer) = transfer {
        let session = transfer.session;
        let percent = if verified {
            "100%".into()
        } else if let Some(session) = &session {
            if transfer.size > 0 {
                format!(
                    "{:.0}%",
                    (session.bytes as f64 / transfer.size as f64 * 100.0).min(100.0)
                )
            } else {
                "—".into()
            }
        } else {
            "0%".into()
        };
        let speed = session
            .as_ref()
            .map(|s| {
                fmt_speed(
                    if transfer.ended.is_some() {
                        s.avg_speed()
                    } else {
                        s.current_speed
                    },
                    bits,
                )
            })
            .unwrap_or_else(|| "—".into());
        let eta = if transfer.ended.is_none() {
            session
                .and_then(|s| s.eta())
                .map(fmt_duration)
                .unwrap_or_else(|| "—".into())
        } else {
            "—".into()
        };
        (percent, speed, eta)
    } else {
        (
            if verified { "100%" } else { "—" }.into(),
            "—".into(),
            "—".into(),
        )
    }
}

fn draw_file_picker(f: &mut Frame, ui: &Ui, area: Rect) {
    let rows = area.height.saturating_sub(1) as usize;
    let entries = ui.files.visible();
    let start = ui.files.selected.saturating_sub(rows.saturating_sub(1));
    let mut lines = vec![Line::from(theme::label(format!(" /{}", ui.files.cwd)))];
    lines.extend(
        entries
            .iter()
            .enumerate()
            .skip(start)
            .take(rows)
            .map(|(i, entry)| {
                Line::from(Span::styled(
                    format!(
                        " {} {}{}  {}",
                        if i == ui.files.selected { ">" } else { " " },
                        entry.name,
                        if entry.is_dir { "/" } else { "" },
                        if entry.is_dir {
                            String::new()
                        } else {
                            fmt_bytes(entry.size)
                        }
                    ),
                    if i == ui.files.selected {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::TEXT)
                    },
                ))
            }),
    );
    f.render_widget(Paragraph::new(lines), area);
}

fn cursor_on() -> bool {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_millis() / 500) % 2 == 0)
        .unwrap_or(true)
}

fn transfer_summary(ui: &Ui, switch: &crate::switch::Switch) -> Vec<Line<'static>> {
    let Some(transfer) = switch.transfer(&ui.app.sessions) else {
        return Vec::new();
    };
    let bits = ui.app.config.read().unwrap().speed_in_bits;
    let (bytes, speed, avg, eta) = match transfer.session.as_ref() {
        Some(session) => (
            session.bytes,
            if transfer.ended.is_some() {
                0.0
            } else {
                session.current_speed
            },
            session.avg_speed(),
            if transfer.ended.is_some() {
                None
            } else {
                session.eta()
            },
        ),
        None => (0, 0.0, 0.0, None),
    };
    let progress = if transfer.size > 0 {
        (bytes as f64 / transfer.size as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    vec![
        Line::from(vec![
            theme::label(" file     "),
            theme::value(format!(
                "/{}  ({})",
                transfer.rel_path,
                transfer.protocol.label()
            )),
        ]),
        Line::from(vec![
            theme::label(" transfer "),
            theme::value(format!(
                "{} / {}  ({progress:.0}%)",
                fmt_bytes(bytes),
                fmt_bytes(transfer.size)
            )),
        ]),
        Line::from(vec![
            theme::label(" speed    "),
            theme::value(fmt_speed(speed, bits)),
            theme::label("   avg "),
            theme::value(fmt_speed(avg, bits)),
            theme::label("   ETA "),
            theme::value(eta.map(fmt_duration).unwrap_or_else(|| "—".into())),
        ]),
    ]
}

fn draw_modal(f: &mut Frame, ui: &Ui) {
    let Some(modal) = &ui.modal else { return };
    match modal {
        Modal::Workflow => super::workflow::draw(f, ui),
        Modal::WorkflowLogin { form, .. } => super::workflow::draw_login(f, form),
        Modal::CoreQuestion(question) => {
            let area = centered_rect(100, 12, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(format!(
                    "{:?}\n\nType lowercase y to confirm. Any other key cancels.",
                    question.kind
                ))
                .block(theme::panel(" CONFIRM "))
                .wrap(Wrap { trim: false }),
                area,
            );
        }
        Modal::Jobs { selected } => {
            let snapshot = ui.app.engine().snapshot();
            let jobs: Vec<_> = snapshot
                .operations
                .iter()
                .filter(|o| o.transfer.is_some())
                .collect();
            let area = centered_rect(110, f.area().height.saturating_sub(4).max(8), f.area());
            f.render_widget(Clear, area);
            let rows = area.height.saturating_sub(5) as usize;
            let start = selected.saturating_sub(rows.saturating_sub(1));
            let mut lines = vec![
                Line::from(
                    " ↑↓ select · v inspect · r retry · o overwrite · c cancel · s check · S resume checked · Esc close",
                ),
                Line::from(""),
            ];
            for (index, operation) in jobs.iter().enumerate().skip(start).take(rows) {
                let transfer = operation.transfer.as_ref().unwrap();
                let name = snapshot
                    .devices
                    .iter()
                    .find(|d| Some(d.id) == operation.device)
                    .map(|d| d.name.as_str())
                    .unwrap_or("removed device");
                let text = format!(
                    "{} #{:03} {} · {} · {} {} → {}",
                    if index == *selected { "▸" } else { " " },
                    operation.id,
                    name,
                    operation.state.label(),
                    transfer.protocol.label(),
                    if transfer.receive {
                        &transfer.remote
                    } else {
                        &transfer.local
                    },
                    if transfer.receive {
                        &transfer.local
                    } else {
                        &transfer.remote
                    }
                );
                lines.push(Line::from(Span::styled(
                    text,
                    Style::default().fg(if operation.state.error().is_some() {
                        theme::ERR
                    } else if index == *selected {
                        theme::ACCENT
                    } else {
                        theme::TEXT
                    }),
                )));
            }
            if jobs.is_empty() {
                lines.push(Line::from(" No transfer jobs in this session."));
            }
            if let Some(error) = jobs.get(*selected).and_then(|o| o.state.error()) {
                lines.push(Line::from(Span::styled(
                    error.to_string(),
                    Style::default().fg(theme::ERR),
                )));
            }
            f.render_widget(
                Paragraph::new(lines)
                    .block(theme::panel(" JOBS · session only "))
                    .wrap(Wrap { trim: false }),
                area,
            );
        }
        Modal::Help => draw_help(f, ui.help_scroll),
        Modal::ConfirmQuit => {
            let running = ServiceId::ALL
                .iter()
                .filter(|id| ui.app.services.status(**id).is_running())
                .count();
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  Stop {running} running service(s) and quit?"),
                    Style::default().fg(theme::TEXT),
                )),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  "),
                    theme::key("y"),
                    theme::label(" stop and quit      "),
                    theme::key("any other key"),
                    theme::label(" cancel"),
                ]),
            ];
            let area = centered_rect(56, 7, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(theme::panel_double(" QUIT ")),
                area,
            );
        }
        Modal::ConfirmUploads => {
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Enable uploads (write access)?",
                    Style::default().fg(theme::WARN).bold(),
                )),
                Line::from(theme::label("  Clients will be able to store files in the")),
                Line::from(theme::label("  upload directory.")),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  "),
                    theme::key("y"),
                    theme::label(" enable      "),
                    theme::key("any other key"),
                    theme::label(" cancel"),
                ]),
            ];
            let area = centered_rect(56, 9, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(theme::panel_double(" UPLOADS ")),
                area,
            );
        }
        Modal::Input { title, value, .. } => {
            let area = centered_rect(60, 5, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(value.clone(), Style::default().fg(theme::HILITE).bold()),
                    Span::styled("█", Style::default().fg(theme::ACCENT)),
                ]))
                .block(theme::panel_double(&format!(
                    " {title} (Enter confirm, Esc cancel) "
                ))),
                area,
            );
        }
        Modal::Cisco {
            rel_path,
            commands,
            selected,
            copied,
        } => {
            let (ip, source) = super::advertised_now(&ui.app.config.read().unwrap());
            let mut lines = vec![
                Line::from(vec![
                    theme::label("copy commands for "),
                    Span::styled(
                        format!("/{rel_path}"),
                        Style::default().fg(theme::CYAN).bold(),
                    ),
                    theme::label("   (↑↓ select, y/Enter copy, Esc close)"),
                ]),
                Line::from(vec![
                    theme::label("from "),
                    Span::styled(
                        ip.map(|i| i.to_string()).unwrap_or_else(|| "-".into()),
                        Style::default().fg(theme::HILITE).bold(),
                    ),
                    theme::label(format!(" ({source})   ")),
                    theme::key("i"),
                    theme::label(" cycles:  "),
                    theme::label(
                        crate::netif::candidates()
                            .iter()
                            .map(|c| format!("{} {}", c.name, c.ip))
                            .collect::<Vec<_>>()
                            .join(" · "),
                    ),
                ]),
            ];
            for (i, (proto, cmd)) in commands.iter().enumerate() {
                let style = if i == *selected {
                    theme::selected()
                } else {
                    Style::default().fg(theme::TEXT)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {:<6}", proto.label()),
                        Style::default().fg(theme::ACCENT),
                    ),
                    Span::styled(cmd.clone(), style),
                ]));
            }
            if *copied {
                lines.push(Line::from(Span::styled(
                    " ✓ copied to clipboard",
                    Style::default().fg(theme::OK).bold(),
                )));
            }
            let width = (f.area().width).min(120);
            let area = centered_rect(width, commands.len() as u16 + 5, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(theme::panel_double(" CISCO COPY ")),
                area,
            );
        }
        Modal::Message(msg) => {
            let area = centered_rect(60, msg.lines().count() as u16 + 4, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(format!("\n{msg}\n\npress any key to close"))
                    .style(Style::default().fg(theme::TEXT))
                    .wrap(Wrap { trim: false })
                    .block(theme::panel_double(" NOTICE ")),
                area,
            );
        }
        Modal::ServiceEdit { id, field, editing } => {
            let cfg = ui.app.config.read().unwrap();
            let status = ui.app.services.status(*id);
            let sc = cfg.service(*id);
            let mut lines = vec![Line::from(theme::label(
                " ↑↓ select   Enter edit / toggle   Esc close",
            ))];
            for (i, fld) in EditField::ALL.iter().enumerate() {
                let value: String = match fld {
                    EditField::StartStop => status.to_string(),
                    EditField::Enabled => {
                        if sc.enabled {
                            "on".into()
                        } else {
                            "off".into()
                        }
                    }
                    EditField::Port => sc.port.to_string(),
                    EditField::Bind => sc.bind.clone(),
                    EditField::Username => cfg.auth.username.clone(),
                    EditField::Password => cfg.auth.password.clone(),
                    EditField::Uploads => {
                        if cfg.uploads.enabled {
                            "on".into()
                        } else {
                            "off".into()
                        }
                    }
                    EditField::UploadDir => {
                        if cfg.uploads.dir.is_empty() {
                            "<root>".into()
                        } else {
                            cfg.uploads.dir.clone()
                        }
                    }
                };
                let selected = i == *field;
                let shown = if selected {
                    match editing {
                        Some(buf) => format!("{buf}█"),
                        None => value,
                    }
                } else {
                    value
                };
                let row_style = if *fld == EditField::StartStop {
                    let style = status_style(&status);
                    if selected {
                        style.bg(theme::SEL_BG)
                    } else {
                        style
                    }
                } else if selected {
                    theme::selected_strong()
                } else {
                    Style::default().fg(theme::TEXT)
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {:<14}", fld.label()),
                        Style::default().fg(theme::ACCENT),
                    ),
                    Span::styled(shown, row_style),
                ]));
            }
            drop(cfg);
            let area = centered_rect(62, EditField::ALL.len() as u16 + 4, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines).block(theme::panel_double(&format!(
                    " EDIT {} ",
                    id.display_name().to_uppercase()
                ))),
                area,
            );
        }
        Modal::DeployProtocol { selected } => {
            draw_deploy(f, ui);
            let lines: Vec<Line> = super::DEPLOY_PROTOCOLS
                .iter()
                .enumerate()
                .map(|(i, proto)| {
                    let status = ui.app.services.status(crate::cisco::service_of(*proto));
                    let selected = i == *selected;
                    let style = if selected {
                        theme::selected()
                    } else {
                        Style::default().fg(theme::TEXT)
                    };
                    let status_style = if selected {
                        status_style(&status).bg(theme::SEL_BG)
                    } else {
                        status_style(&status)
                    };
                    Line::from(vec![
                        Span::styled(
                            format!(
                                " {} {:<6} ",
                                if selected { ">" } else { " " },
                                proto.label()
                            ),
                            style,
                        ),
                        Span::styled(status.to_string(), status_style),
                    ])
                })
                .chain(std::iter::once(Line::from(theme::label(
                    " s: start server   Enter: select   Esc: back",
                ))))
                .collect();
            let area = centered_rect(48, 10, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines)
                    .block(theme::panel_double(" PROTOCOL — ↑↓ / s start / Enter ")),
                area,
            );
        }
        Modal::ConfirmStart { id } => {
            draw_deploy(f, ui);
            let area = centered_rect(64, 7, f.area());
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(format!("\n {} not started. Do you want to start it?\n\n y: yes, start and deploy   n / Esc: no", id.display_name()))
                .wrap(Wrap { trim: false }).block(theme::panel_double(" START SERVER ")), area);
        }
        Modal::UpgradeFiles => {
            let area = centered_rect(
                f.area().width.saturating_sub(4),
                f.area().height.saturating_sub(4),
                f.area(),
            );
            f.render_widget(Clear, area);
            // This picker uses the same shared-root browser and size metadata.
            let block =
                theme::panel_double(" CHOOSE FILE — ↑↓ / Enter select / Backspace up / Esc ");
            let inner = block.inner(area);
            f.render_widget(block, area);
            draw_file_picker(f, ui, inner);
        }
        Modal::TransferProtocol {
            options,
            selected,
            upgrade,
            ..
        } => {
            let area = centered_rect(64, options.len() as u16 + 5, f.area());
            f.render_widget(Clear, area);
            let mut lines = vec![Line::from(theme::label(
                "Enabled / running services first. Enter selects; Esc cancels.",
            ))];
            for (i, protocol) in options.iter().enumerate() {
                let status = ui.app.services.status(crate::cisco::service_of(*protocol));
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {} — {}",
                        if i == *selected { ">" } else { " " },
                        protocol.label(),
                        status
                    ),
                    if i == *selected {
                        theme::selected()
                    } else {
                        status_style(&status)
                    },
                )));
            }
            f.render_widget(
                Paragraph::new(lines).block(theme::panel_double(if *upgrade {
                    " UPGRADE PROTOCOL "
                } else {
                    " TRANSFER PROTOCOL "
                })),
                area,
            );
        }
        Modal::ConfirmDelete {
            switch,
            path,
            recursive,
            ..
        } => {
            let area = centered_rect(86, 12, f.area());
            f.render_widget(Clear, area);
            let lines = vec![
                Line::from(""),
                Line::from(format!(
                    "Device: {} ({})",
                    switch.display_name(),
                    switch.host
                )),
                Line::from(format!(
                    "{}: {path}",
                    if *recursive {
                        "DIRECTORY AND ALL CONTENTS"
                    } else {
                        "FILE"
                    }
                )),
                Line::from(""),
                Line::from("DANGER: deletion is permanent and cannot be undone."),
                Line::from(if *recursive {
                    "All files and subdirectories inside this directory will be removed."
                } else {
                    "The selected file will be permanently removed."
                }),
                Line::from(""),
                Line::from("y: permanently delete | ANY OTHER KEY: cancel"),
            ];
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .style(Style::default().fg(theme::ERR).bold())
                    .block(
                        theme::panel_double(" CONFIRM REMOTE DELETE ")
                            .border_style(Style::default().fg(theme::ERR)),
                    ),
                area,
            );
        }
        Modal::UpgradeActions { switch, selected } => {
            let area = centered_rect(86, 12, f.area());
            f.render_widget(Clear, area);
            let reason = crate::upgrade::install_blocker(
                &switch.upgrade(),
                &switch.facts().version.unwrap_or_default(),
            );
            let ready = switch.state() == SwitchState::Ready;
            let mut lines = vec![Line::from(theme::value(format!(
                "Device: {} ({})",
                switch.display_name(),
                switch.host
            )))];
            for (i, label) in super::UPGRADE_JOB_ACTIONS.iter().enumerate() {
                let enabled = ready && (!(i == 2 || i == 3) || reason.is_none());
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} [{}]{}",
                        if i == *selected { ">" } else { " " },
                        label,
                        if enabled { "" } else { " (unavailable)" }
                    ),
                    if i == *selected {
                        theme::selected()
                    } else {
                        Style::default().fg(if enabled { theme::TEXT } else { theme::DIM })
                    },
                )));
            }
            if let Some(reason) = reason {
                lines.push(Line::from(Span::styled(
                    reason,
                    Style::default().fg(theme::WARN),
                )));
            }
            lines.push(Line::from(theme::label(
                "↑↓ select | Enter opens action | Esc closes | upgrades require y confirmation",
            )));
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(theme::panel_double(" DEVICE ACTIONS ")),
                area,
            );
        }
        Modal::ConfirmInstall { switch, yolo, .. } => {
            let progress = switch.upgrade();
            let remote = match progress {
                crate::upgrade::Progress::Verified { remote, .. } => remote,
                _ => "unverified".into(),
            };

            let running = switch
                .facts()
                .version
                .and_then(|v| v.version)
                .unwrap_or_else(|| "?".into());
            let target = crate::upgrade::image_version(&remote).unwrap_or_else(|| "?".into());
            let text=format!("Device: {} ({})\nImage: {remote}\nVersion: {running} → {target} | local/device MD5 matched\n\nwrite memory → install add file … activate commit{}\n\n{}\n\ny: START UPGRADE | ANY OTHER KEY: cancel",switch.display_name(),switch.host,if *yolo{" prompt-level none"}else{""},if *yolo{"YOLO: the device will reload automatically after this confirmation."}else{"You will also confirm the device's reload prompt in the console."});
            let area = centered_rect(86, 14, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .block(theme::panel_double(" CONFIRM UPGRADE ")),
                area,
            );
        }
        Modal::Cli => {
            let area = f.area();
            f.render_widget(Clear, area);
            if let Some(view) = &ui.session_view {
                let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(1));
                let _ = ui
                    .app
                    .engine()
                    .submit(transferbuddy_core::engine::Command::ResizeCli(
                        view.switch.id,
                        body.height,
                        body.width,
                    ));
                if let Some(screen) = view.switch.terminal_screen() {
                    f.render_widget(TerminalGrid(&screen), body);
                    let (row, col) = screen.cursor_position();
                    if !screen.hide_cursor() && row < body.height && col < body.width {
                        f.set_cursor_position((body.x + col, body.y + row));
                    }
                }
            }
            f.render_widget(
                Paragraph::new(
                    " INTERACTIVE SSH CLI — keys go to switch | ESC returns to TransferBuddy ",
                )
                .style(Style::default().fg(theme::BG).bg(theme::WARN)),
                Rect {
                    x: area.x,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width,
                    height: 1,
                },
            );
        }
        Modal::Deploy => draw_deploy(f, ui),
        Modal::Session => draw_session(f, ui),
        Modal::Hashes => {
            let mut lines = Vec::new();
            if let Some(info) = &ui.hashes {
                lines.push(Line::from(Span::styled(
                    format!("/{}", info.rel_path),
                    Style::default().fg(theme::CYAN).bold(),
                )));
                lines.push(Line::from(""));
                for (label, value) in [
                    ("MD5    ", &info.md5),
                    ("SHA-256", &info.sha256),
                    ("SHA-512", &info.sha512),
                ] {
                    lines.push(Line::from(vec![
                        Span::styled(format!("{label}  "), Style::default().fg(theme::ACCENT)),
                        theme::value(value.clone()),
                    ]));
                }
                lines.push(Line::from(""));
                if let Some(c) = &info.compare {
                    let (txt, color) = if c.kind == "?" {
                        (
                            "unrecognized hash — paste 32/64/128 hex chars".to_string(),
                            theme::WARN,
                        )
                    } else if c.matched {
                        (format!("✓ MATCH  ({})", c.kind), theme::OK)
                    } else {
                        (format!("✗ NO MATCH  ({})", c.kind), theme::ERR)
                    };
                    lines.push(Line::from(Span::styled(
                        txt,
                        Style::default().fg(color).bold(),
                    )));
                    lines.push(Line::from(theme::label(format!("  compared: {}", c.input))));
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(vec![
                    Span::raw(" "),
                    theme::key("c"),
                    theme::label(" compare a hash    "),
                    theme::key("Esc"),
                    theme::label(" close"),
                ]));
            }
            let width = f.area().width.min(148);
            let height = (lines.len() as u16 + 6).min(f.area().height.saturating_sub(2));
            let area = centered_rect(width, height, f.area());
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(theme::panel_double(" FILE HASHES ")),
                area,
            );
        }
    }
}

struct TerminalGrid<'a>(&'a transferbuddy_core::terminal_types::Screen);
impl ratatui::widgets::Widget for TerminalGrid<'_> {
    fn render(self, area: Rect, buffer: &mut ratatui::buffer::Buffer) {
        let (rows, cols) = self.0.size();
        for row in 0..rows.min(area.height) {
            for col in 0..cols.min(area.width) {
                if let Some(cell) = self.0.cell(row, col) {
                    if cell.is_wide_continuation() {
                        continue;
                    }
                    let content = cell.contents();
                    let mut style = Style::default();
                    if let Some([r, g, b]) = transferbuddy_core::terminal::rgb(cell.fgcolor()) {
                        style = style.fg(Color::Rgb(r, g, b));
                    }
                    if let Some([r, g, b]) = transferbuddy_core::terminal::rgb(cell.bgcolor()) {
                        style = style.bg(Color::Rgb(r, g, b));
                    }
                    if cell.bold() {
                        style = style.add_modifier(Modifier::BOLD)
                    }
                    if cell.italic() {
                        style = style.add_modifier(Modifier::ITALIC)
                    }
                    if cell.underline() {
                        style = style.add_modifier(Modifier::UNDERLINED)
                    }
                    if cell.inverse() {
                        style = style.add_modifier(Modifier::REVERSED)
                    }
                    if let Some(out) = buffer.cell_mut((area.x + col, area.y + row)) {
                        out.set_symbol(if content.is_empty() { " " } else { &content })
                            .set_style(style);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A minimal but real `App`, enough to render every view. The runtime has
    /// to outlive it, so it is handed back to the caller.
    fn test_app(root: std::path::PathBuf) -> (tokio::runtime::Runtime, crate::SharedApp) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut cfg = crate::config::Config {
            config_dir: root.join(".test-config"),
            sound: false,
            root,
            ..Default::default()
        };
        cfg.http.enabled = true;
        cfg.http.port = 8080;
        cfg.http.bind = "0.0.0.0".into();
        let logger = Arc::new(crate::logging::Logger::new(LogLevel::Debug, None, false));
        let logger2 = logger.clone();
        let sessions = Arc::new(crate::session::SessionManager::new(600));
        let services = crate::services::ServiceManager::new(
            rt.handle().clone(),
            logger.clone(),
            sessions.clone(),
        );
        let app: crate::SharedApp = Arc::new(crate::App {
            config: std::sync::RwLock::new(cfg),
            logger,
            sessions,
            services,
            switches: crate::switch::SwitchManager::new(rt.handle().clone(), logger2),
            privileged: false,
            runtime: rt.handle().clone(),
            engine_state: Default::default(),
        });
        app.services.attach_app(&app);
        (rt, app)
    }

    fn test_ui(app: crate::SharedApp) -> Ui {
        Ui {
            app,
            workflow: super::super::workflow::Editor::default(),
            tab: Tab::Dashboard,
            service_sel: 0,
            dashboard_transfers: false,
            files: super::super::FileBrowser::new(),
            transfer_ui: super::super::TransferUi::default(),
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
            upgrade_protocol: crate::session::Protocol::Http,
            upgrade_buttons: Vec::new(),
            deploy_mode: super::super::DeployMode::Copy,
            pending_deploy: None,
        }
    }

    /// A session with realistic facts, as the C9200L reports them.
    fn test_switch(state: crate::switch::SwitchState) -> std::sync::Arc<crate::switch::Switch> {
        let version = crate::cisco::parse_show_version(include_str!(
            "../../testdata/show_version_c9200l.txt"
        ));
        let flash = crate::cisco::parse_dir_totals(include_str!("../../testdata/dir_flash.txt"));
        let facts = crate::switch::Facts {
            hostname: Some("SG-AS-OG5-01".into()),
            version: Some(version),
            flash,
            flash_device: "flash:".into(),
            updated: None,
        };
        let transcript = vec![
            crate::switch::Line {
                kind: LineKind::Info,
                text: "connecting to 10.20.30.40:22".into(),
            },
            crate::switch::Line {
                kind: LineKind::Sent,
                text: "terminal length 0".into(),
            },
            crate::switch::Line {
                kind: LineKind::Output,
                text: "SG-AS-OG5-01#".into(),
            },
        ];
        crate::switch::Switch::for_test("10.20.30.40", state, facts, transcript)
    }

    /// Render the whole TUI into a test terminal and return it as plain text.
    fn render_ui(ui: &mut Ui) -> String {
        let backend = ratatui::backend::TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, ui)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    #[ignore = "Generate documentation screenshots from the real TUI renderer"]
    fn tui_documentation_screenshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("IOS-images");
        std::fs::create_dir(&root).unwrap();
        for name in [
            "cat9k_lite_iosxe.17.15.06.SPA.bin",
            "cat9k_lite-rpbase.17.15.03.SPA.pkg",
            "packages.conf",
            "switch-backup.cfg",
        ] {
            std::fs::write(root.join(name), b"demonstration").unwrap();
        }
        let (_rt, app) = test_app(root.clone());
        {
            let mut config = app.config.write().unwrap();
            for (id, port) in [
                (ServiceId::Https, 8443),
                (ServiceId::Ftp, 2121),
                (ServiceId::Ssh, 2222),
                (ServiceId::Tftp, 69),
            ] {
                let service = config.service_mut(id);
                service.port = port;
                service.bind = "0.0.0.0".into();
                service.enabled = matches!(id, ServiceId::Ftp | ServiceId::Ssh);
            }
        }
        let mut jobs = Vec::new();
        for n in 1..=3 {
            let mut sw = test_switch(SwitchState::Ready);
            Arc::get_mut(&mut sw).unwrap().id = n;
            sw.facts_for_test(|facts| {
                facts.hostname = Some(format!("access-{n:02}"));
                facts.flash = Some(crate::cisco::FlashUsage {
                    total: 4_000_000_000,
                    free: if n == 3 { 20_000 } else { 2_300_000_000 },
                });
            });
            sw.test_listing(
                "flash:",
                vec![
                    crate::cisco::RemoteFile {
                        name: "logs".into(),
                        is_dir: true,
                        size: 0,
                    },
                    crate::cisco::RemoteFile {
                        name: "packages.conf".into(),
                        is_dir: false,
                        size: 1124,
                    },
                    crate::cisco::RemoteFile {
                        name: "startup-config".into(),
                        is_dir: false,
                        size: 14280,
                    },
                ],
            );
            if n == 2 {
                sw.set_upgrade(crate::upgrade::Progress::Verified {
                    remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
                    md5: "900150983cd24fb0d6963f7d28e17f72".into(),
                    version: Some("17.15.6".into()),
                });
            }
            jobs.push(sw.test_job_receiver());
            app.switches.add_for_test(sw);
        }
        app.logger.log(
            crate::logging::Event::new(LogLevel::Info, "switch", "SSH session established")
                .ip("10.20.30.40".parse().unwrap())
                .device("access-01".into(), Some("C9200L-48P-4X".into())),
        );
        app.logger.log(
            crate::logging::Event::new(LogLevel::Info, "http", "Remote MD5 matches local image")
                .ip("10.20.30.40".parse().unwrap())
                .result("verified"),
        );
        let mut ui = test_ui(app);
        ui.files.refresh(&root);
        ui.upgrade_menu = None;
        for n in 1..=3 {
            ui.upgrade_assignments
                .insert(n, ("cat9k_lite_iosxe.17.15.06.SPA.bin".into(), 600_000_000));
        }
        let output =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots/tui");
        for (tab, name) in [
            (Tab::Dashboard, "dashboard"),
            (Tab::Connect, "connect"),
            (Tab::Files, "transfer"),
            (Tab::Upgrade, "upgrade"),
            (Tab::Logs, "logs"),
        ] {
            ui.tab = tab;
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            super::super::screenshots::save(
                terminal.backend().buffer(),
                &output.join(format!("{name}.svg")),
            );
        }
        ui.tab = Tab::Dashboard;
        for (name, modal) in [("help", Modal::Help), ("service-edit", Modal::ServiceEdit {
            id: ServiceId::Http, field: 0, editing: None,
        }), ("cisco-copy", Modal::Cisco {
            rel_path: "cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
            commands: vec![(crate::session::Protocol::Http,
                "copy http://192.168.22.23:8080/cat9k_lite_iosxe.17.15.06.SPA.bin flash:".into()),
                (crate::session::Protocol::Sftp,
                "copy sftp://cisco:cisco123@192.168.22.23:2222/cat9k_lite_iosxe.17.15.06.SPA.bin flash:".into())],
            selected: 0,
            copied: false,
        })] {
            ui.modal = Some(modal);
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
            terminal.draw(|frame| draw(frame, &mut ui)).unwrap();
            super::super::screenshots::save(terminal.backend().buffer(), &output.join(format!("{name}.svg")));
        }
    }

    #[test]
    fn escape_leaves_cli_and_remote_exit_returns_without_another_key() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let sw = test_switch(SwitchState::Busy { what: "CLI".into() });
        let mut input = sw.test_cli_receiver();
        ui.app.switches.add_for_test(sw.clone());
        ui.session_view = Some(super::super::SessionView {
            switch: sw.clone(),
            scroll: None,
            jobs_seen: 0,
        });
        ui.modal = Some(Modal::Cli);
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(input.try_recv().unwrap(), b"x");
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Esc));
        assert!(ui.modal.is_none());
        assert!(!sw.cli_open());
        assert!(input.try_recv().is_err());
        let _input = sw.test_cli_receiver();
        ui.modal = Some(Modal::Cli);
        sw.close_cli();
        super::super::pump_session(&mut ui);
        assert!(ui.modal.is_none());
    }

    #[test]
    fn delete_confirmation_is_red_targets_one_entry_and_accepts_only_plain_y() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (key, modifiers, yes) in [
            (KeyCode::Char('y'), KeyModifiers::NONE, true),
            (KeyCode::Char('Y'), KeyModifiers::NONE, false),
            (KeyCode::Char('y'), KeyModifiers::CONTROL, false),
            (KeyCode::Enter, KeyModifiers::NONE, false),
            (KeyCode::Esc, KeyModifiers::NONE, false),
            (KeyCode::Char('c'), KeyModifiers::CONTROL, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (_rt, app) = test_app(dir.path().to_path_buf());
            let mut ui = test_ui(app);
            let sw = test_switch(SwitchState::Ready);
            let mut jobs = sw.test_job_receiver();
            ui.app.switches.add_for_test(sw.clone());
            sw.test_listing(
                "flash:",
                vec![crate::cisco::RemoteFile {
                    name: "folder".into(),
                    is_dir: true,
                    size: 10,
                }],
            );
            ui.tab = Tab::Files;
            ui.transfer_ui.focus = 2;
            super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Delete));
            assert!(
                matches!(&ui.modal,Some(Modal::ConfirmDelete{path,recursive:true,..})if path=="flash:folder")
            );
            assert!(jobs.try_recv().is_err());
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            terminal.draw(|f| draw(f, &mut ui)).unwrap();
            let buf = terminal.backend().buffer();
            let warning = (0..40)
                .find_map(|y| {
                    let line: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
                    line.find("DANGER")
                        .map(|b| (line[..b].chars().count() as u16, y))
                })
                .unwrap();
            assert_eq!(buf[warning].fg, theme::ERR);
            super::super::handle_key(&mut ui, KeyEvent::new(key, modifiers));
            if yes {
                assert!(
                    matches!(jobs.try_recv().unwrap().untracked(),crate::switch::Job::Delete{path,recursive:true}if path=="flash:folder")
                );
            } else {
                assert!(jobs.try_recv().is_err());
                assert!(ui.modal.is_none());
            }
        }
    }

    #[test]
    fn enter_copies_local_and_remote_files_and_device_enter_selects_protocol() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "data").unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let sw = test_switch(SwitchState::Ready);
        let _jobs = sw.test_job_receiver();
        ui.app.switches.add_for_test(sw.clone());
        ui.tab = Tab::Files;
        ui.files.refresh(dir.path());
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            ui.modal,
            Some(Modal::TransferProtocol { start: true, .. })
        ));
        ui.modal = None;
        ui.transfer_ui.focus = 0;
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            ui.modal,
            Some(Modal::TransferProtocol { start: false, .. })
        ));
        ui.modal = None;
        ui.transfer_ui.focus = 2;
        sw.test_listing(
            "flash:",
            vec![crate::cisco::RemoteFile {
                name: "remote.txt".into(),
                is_dir: false,
                size: 5,
            }],
        );
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Enter));
        assert!(
            matches!(&ui.modal,Some(Modal::TransferProtocol{options,start:true,..})if options==&[crate::session::Protocol::Ftp])
        );
    }

    #[test]
    fn active_protocols_are_sftp_https_ftp_http_in_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let ui = test_ui(app);
        let ids = [
            ServiceId::Http,
            ServiceId::Ftp,
            ServiceId::Https,
            ServiceId::Ssh,
        ];
        for id in ids {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let mut cfg = ui.app.config.write().unwrap();
            let service = cfg.service_mut(id);
            service.bind = "127.0.0.1".into();
            service.port = port;
            service.enabled = true;
        }
        for id in ids {
            ui.app.services.start(id);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ids
            .iter()
            .all(|id| ui.app.services.status(*id).is_running())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "{:?}",
                ids.map(|id| ui.app.services.status(id))
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let options = super::super::protocol_options(&ui, false);
        assert_eq!(
            &options[..4],
            &[
                crate::session::Protocol::Sftp,
                crate::session::Protocol::Https,
                crate::session::Protocol::Ftp,
                crate::session::Protocol::Http
            ]
        );
        assert!(!options.contains(&crate::session::Protocol::Tftp));
        ui.app.services.stop_all();
    }

    #[test]
    fn upgrade_metadata_actions_and_y_confirmation_are_available_per_device() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Upgrade;
        ui.upgrade_menu = None;
        let sw = test_switch(SwitchState::Ready);
        let mut jobs = sw.test_job_receiver();
        ui.app.switches.add_for_test(sw.clone());
        sw.facts_for_test(|f| {
            f.flash = Some(crate::cisco::FlashUsage {
                free: 1,
                total: 100,
            })
        });
        ui.upgrade_assignments
            .insert(sw.id, ("cat9k_lite_iosxe.17.15.06.SPA.bin".into(), 10));
        let screen = render_ui(&mut ui);
        for text in [
            "C9200L-48P-4X",
            "flash free",
            "ETA",
            "upload %",
            "[i Remove inactive]",
        ] {
            assert!(screen.contains(text), "{text}: {screen}");
        }
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Enter));
        assert!(matches!(ui.modal, Some(Modal::UpgradeActions { .. })));
        assert!(render_ui(&mut ui).contains("Remove inactive"));
        assert!(jobs.try_recv().is_err());
        ui.modal = None;
        sw.set_upgrade(crate::upgrade::Progress::Verified {
            remote: "flash:cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
            md5: "78805a221a988e79ef3f42d7c5bfd418".into(),
            version: Some("17.15.6".into()),
        });
        assert!(render_ui(&mut ui).contains("[u Upgrade]"));
        // Mouse hit areas must overlay the rendered buttons, including their row.
        for width in [110, 120, 160] {
            let mut terminal =
                Terminal::new(ratatui::backend::TestBackend::new(width, 40)).unwrap();
            terminal.draw(|f| draw(f, &mut ui)).unwrap();
            let buffer = terminal.backend().buffer();
            for button in &ui.upgrade_buttons {
                let rendered: String = (button.area.x..button.area.right())
                    .map(|x| buffer[(x, button.area.y)].symbol())
                    .collect();
                assert_eq!(
                    rendered,
                    if button.action == 2 {
                        "[u Upgrade]"
                    } else {
                        "[i Remove inactive]"
                    },
                    "width {width}"
                );
            }
        }
        let button = ui.upgrade_buttons.iter().find(|b| b.action == 2).unwrap();
        let click = crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: button.area.x,
            row: button.area.y,
            modifiers: KeyModifiers::NONE,
        };
        super::super::handle_mouse(&mut ui, click);
        assert!(matches!(ui.modal, Some(Modal::ConfirmInstall { .. })));
        assert!(jobs.try_recv().is_err());
        ui.modal = None;

        for (key, mods, yes) in [
            (KeyCode::Char('y'), KeyModifiers::NONE, true),
            (KeyCode::Char('Y'), KeyModifiers::NONE, false),
            (KeyCode::Enter, KeyModifiers::NONE, false),
            (KeyCode::Char('c'), KeyModifiers::CONTROL, false),
        ] {
            super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('u')));
            assert!(matches!(ui.modal, Some(Modal::ConfirmInstall { .. })));
            assert!(render_ui(&mut ui).contains("ANY OTHER KEY: cancel"));
            super::super::handle_key(&mut ui, KeyEvent::new(key, mods));
            if yes {
                assert!(matches!(
                    jobs.try_recv().unwrap().untracked(),
                    crate::switch::Job::Install { yolo: false }
                ));
                sw.complete_job_for_test();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while ui.app.engine().busy(sw.id) && std::time::Instant::now() < deadline {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            } else {
                assert!(jobs.try_recv().is_err());
            }
            ui.modal = None;
        }
        sw.facts_for_test(|f| f.version.as_mut().unwrap().version = Some("17.15.06".into()));
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('u')));
        assert!(ui.modal.is_none());
        assert!(ui
            .status_msg
            .as_ref()
            .unwrap()
            .contains("already installed"));
    }

    #[test]
    fn upgrade_metrics_show_progress_speed_and_eta_for_the_assigned_image() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let ui = test_ui(app);
        let sw = test_switch(SwitchState::Busy {
            what: "copy".into(),
        });
        sw.begin_transfer(
            "image.bin".into(),
            10_000_000,
            crate::session::Protocol::Http,
        );
        let h = ui.app.sessions.open(
            crate::session::Protocol::Http,
            "10.20.30.40:50000".parse().unwrap(),
            8080,
        );
        ui.app.sessions.update(h.id, |s| {
            s.file = Some("image.bin".into());
            s.total = Some(10_000_000);
            s.direction = Some(crate::session::Direction::Download);
            s.state = SessionState::Transferring;
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        h.add_bytes(5_000_000);
        ui.app.sessions.sample();
        let (percent, speed, eta) = upgrade_metrics(&ui, &sw, Some("image.bin"));
        assert_eq!(percent, "50%");
        assert_ne!(speed, "—");
        assert_ne!(eta, "—");
        assert_eq!(upgrade_metrics(&ui, &sw, Some("other.bin")).0, "—");
    }

    #[test]
    fn reload_requires_plain_y_and_timer_is_visible_in_console() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        for (code, modifiers, expected) in [
            (KeyCode::Char('y'), KeyModifiers::NONE, true),
            (KeyCode::Char('Y'), KeyModifiers::NONE, false),
            (KeyCode::Char('y'), KeyModifiers::CONTROL, false),
            (KeyCode::Enter, KeyModifiers::NONE, false),
            (KeyCode::Esc, KeyModifiers::NONE, false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (_rt, app) = test_app(dir.path().to_path_buf());
            let mut ui = test_ui(app);
            let (sw, mut reply) = crate::switch::Switch::for_test_reload();
            ui.session_view = Some(super::super::SessionView {
                switch: sw,
                scroll: None,
                jobs_seen: 0,
            });
            ui.modal = Some(Modal::Session);
            let screen = render_ui(&mut ui);
            assert!(screen.contains("10.20.30.40") && screen.contains("CONFIRM RELOAD"));
            super::super::handle_key(&mut ui, KeyEvent::new(code, modifiers));
            assert_eq!(reply.try_recv().unwrap(), expected);
        }
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let sw = test_switch(crate::switch::SwitchState::Rebooting);
        sw.set_upgrade(crate::upgrade::Progress::Rebooting {
            since: std::time::Instant::now() - std::time::Duration::from_secs(345),
            attempts: 2,
            last_error: None,
        });
        ui.session_view = Some(super::super::SessionView {
            switch: sw,
            scroll: None,
            jobs_seen: 0,
        });
        ui.modal = Some(Modal::Session);
        assert!(render_ui(&mut ui).contains("rebooting 05:45"));
    }

    #[test]
    fn transfer_shows_ten_devices_and_scrolls_to_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        for n in 1..=12 {
            let sw = ui.app.switches.connect(crate::switch::Target {
                host: format!("127.0.0.{n}"),
                port: 1,
                username: "test".into(),
                password: "secret".into(),
                enable_password: String::new(),
                known_hosts: dir.path().join("known_hosts"),
                auto_trust: true,
            });
            sw.facts_for_test(|facts| facts.hostname = Some(format!("device-{n:02}")));
        }
        ui.tab = Tab::Files;
        let first = render_ui(&mut ui);
        for n in 1..=10 {
            assert!(first.contains(&format!("device-{n:02}")));
        }
        assert!(!first.contains("device-11"));
        assert!(first.contains("1–10 / 12") && first.contains("↓ more"));
        ui.switch_sel = 11;
        let last = render_ui(&mut ui);
        assert!(last.contains("device-12"));
        assert!(last.contains("3–12 / 12") && last.contains("↑ more"));
        for sw in ui.app.switches.list() {
            sw.cancel();
        }
    }

    #[test]
    fn ios_files_are_colored_and_reboot_thresholds_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "cat9k_lite-rpbase.17.15.03.SPA.pkg",
            "cat9k_lite_iosxe.17.09.04a.SPA.bin",
            "cat9k_lite_iosxe.17.15.03.SPA.conf",
            "packages.conf",
            "notes.txt",
        ] {
            std::fs::write(dir.path().join(name), "test").unwrap();
        }
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Files;
        ui.transfer_ui.focus = 0;
        ui.files.refresh(dir.path());
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
        let buf = terminal.backend().buffer();
        for (name, color) in [
            ("cat9k_lite-rpbase", theme::WARN),
            ("cat9k_lite_iosxe.17.09", theme::WARN),
            ("cat9k_lite_iosxe.17.15", theme::WARN),
            ("packages.conf", theme::WARN),
            ("notes.txt", theme::TEXT),
        ] {
            let (x, y) = (0..40)
                .find_map(|y| {
                    let line: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
                    line.find(name)
                        .map(|byte| (line[..byte].chars().count() as u16, y))
                })
                .unwrap();
            assert_eq!(buf[(x, y)].fg, color, "{name}");
        }
        for (seconds, color) in [
            (0, theme::OK),
            (299, theme::OK),
            (300, theme::WARN),
            (599, theme::WARN),
            (600, theme::ERR),
        ] {
            let progress = crate::upgrade::Progress::Rebooting {
                since: std::time::Instant::now() - std::time::Duration::from_secs(seconds),
                attempts: 0,
                last_error: None,
            };
            assert_eq!(reboot_color(&progress), color);
        }
    }

    #[test]
    fn protocol_selection_is_per_device_and_connections_have_no_picker() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        {
            let mut cfg = ui.app.config.write().unwrap();
            for id in ServiceId::ALL {
                cfg.service_mut(id).enabled = false;
            }
            cfg.ftp.enabled = true;
        }
        let sw = ui.app.switches.connect(crate::switch::Target {
            host: "127.0.0.1".into(),
            port: 1,
            username: "test".into(),
            password: "secret".into(),
            enable_password: String::new(),
            known_hosts: dir.path().join("known_hosts"),
            auto_trust: true,
        });
        ui.tab = Tab::Files;
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('a')));
        assert!(ui.modal.is_none());
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('p')));
        assert!(
            matches!(&ui.modal,Some(Modal::TransferProtocol{options,selected,..}) if options[*selected]==crate::session::Protocol::Ftp)
        );
        super::super::handle_modal_key(&mut ui, KeyEvent::from(KeyCode::Enter));
        assert!(sw.protocol_chosen());
        assert_eq!(sw.protocol(), crate::session::Protocol::Ftp);
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('p')));
        assert!(
            matches!(&ui.modal,Some(Modal::TransferProtocol{options,selected,..}) if options[*selected]==sw.protocol())
        );
        sw.cancel();
    }

    #[test]
    fn changing_an_idle_job_image_invalidates_its_verified_release() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let sw = test_switch(crate::switch::SwitchState::Ready);
        let mut other = test_switch(crate::switch::SwitchState::Ready);
        std::sync::Arc::get_mut(&mut other).unwrap().id = 2;
        for name in ["one.bin", "two.bin", "three.bin"] {
            std::fs::write(dir.path().join(name), b"test image").unwrap();
        }
        ui.app.switches.add_for_test(sw.clone());
        ui.app.switches.add_for_test(other.clone());
        super::super::assign_upgrade(&mut ui, &sw, ("one.bin".into(), 10));
        super::super::assign_upgrade(&mut ui, &other, ("two.bin".into(), 20));
        sw.set_upgrade(crate::upgrade::Progress::Verified {
            remote: "flash:one.bin".into(),
            md5: "x".into(),
            version: Some("17.15.3".into()),
        });
        super::super::assign_upgrade(&mut ui, &sw, ("one.bin".into(), 10));
        assert!(matches!(
            sw.upgrade(),
            crate::upgrade::Progress::Verified { .. }
        ));
        super::super::assign_upgrade(&mut ui, &sw, ("three.bin".into(), 30));
        assert!(matches!(sw.upgrade(), crate::upgrade::Progress::Idle));
        assert_eq!(ui.upgrade_assignments[&other.id].0, "two.bin");
    }

    #[test]
    fn connect_tab_opens_ssh_without_selecting_a_transfer_protocol() {
        use crossterm::event::{KeyCode, KeyEvent};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Connect;
        super::super::handle_key(&mut ui, KeyEvent::from(KeyCode::Char('a')));
        let form = &mut ui.deploy.as_mut().unwrap().form;
        assert!(!super::super::DeployField::ADD.contains(&super::super::DeployField::Protocol));
        form.host = "127.0.0.1".into();
        form.port = "1".into();
        form.username = "test".into();
        form.password = "secret".into();
        super::super::add_devices(&mut ui);
        assert_eq!(ui.tab, Tab::Connect);
        assert!(matches!(ui.modal, Some(Modal::Session)));
        assert!(ui
            .session_view
            .as_ref()
            .unwrap()
            .switch
            .transfer(&ui.app.sessions)
            .is_none());
        assert!(ui.deploy.as_ref().unwrap().form.password.is_empty());
        ui.session_view.as_ref().unwrap().switch.cancel();
    }

    #[test]
    fn transfer_navigation_returns_to_root_and_hides_symlink_escapes() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("one/two")).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Files;
        ui.files.refresh(dir.path());
        assert!(!ui.files.entries.iter().any(|e| e.name == "escape"));
        let press = |ui: &mut Ui, code| {
            super::super::handle_key(ui, KeyEvent::new(code, KeyModifiers::NONE))
        };
        press(&mut ui, KeyCode::Enter);
        assert_eq!(ui.files.cwd, "one");
        ui.files.filter = "two".into();
        ui.files.selected = 1;
        press(&mut ui, KeyCode::Enter);
        assert_eq!(ui.files.cwd, "one/two");
        ui.files.filter = "no matches".into();
        assert_eq!(ui.files.visible()[0].name, "..");
        press(&mut ui, KeyCode::Backspace);
        assert_eq!(ui.files.cwd, "one");
        assert!(ui.files.filter.is_empty());
        press(&mut ui, KeyCode::Enter);
        assert!(ui.files.cwd.is_empty());
        press(&mut ui, KeyCode::Backspace);
        assert!(ui.files.cwd.is_empty());
        press(&mut ui, KeyCode::Enter);
        press(&mut ui, KeyCode::Home);
        assert!(ui.files.cwd.is_empty());
        let screen = render_ui(&mut ui);
        for label in ["3 Transfer", "DEVICES", "LOCAL", "REMOTE", "LOCAL → REMOTE"] {
            assert!(screen.contains(label), "{label}");
        }
        press(&mut ui, KeyCode::Right);
        assert_eq!(ui.transfer_ui.focus, 2);
        assert!(render_ui(&mut ui).contains("REMOTE → LOCAL"));
        press(&mut ui, KeyCode::Char('d'));
        assert!(
            ui.modal.is_none(),
            "remote-pane keys must not act on local files"
        );
        press(&mut ui, KeyCode::Tab);
        assert_eq!(ui.transfer_ui.focus, 0);
        press(&mut ui, KeyCode::Tab);
        assert_eq!(ui.transfer_ui.focus, 1);
    }

    #[test]
    fn every_view_and_modal_renders() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("image.bin"), b"x").unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);

        for tab in Tab::ALL {
            ui.tab = tab;
            let screen = render_ui(&mut ui);
            // Tab bar, version marker and footer are always there.
            assert!(screen.contains("Dashboard"), "{tab:?}: no tab bar");
            assert!(
                screen.contains(&format!("v{}", crate::VERSION)),
                "{tab:?}: no version"
            );
        }

        ui.tab = Tab::Dashboard;
        let dash = render_ui(&mut ui);
        assert!(dash.contains("SERVICES"));
        assert!(dash.contains("cisco / cisco123"));
        assert!(dash.contains("address in URLs"));

        for modal in [
            Modal::Help,
            Modal::ConfirmQuit,
            Modal::ConfirmUploads,
            Modal::ServiceEdit {
                id: ServiceId::Http,
                field: 0,
                editing: None,
            },
        ] {
            ui.modal = Some(modal);
            let screen = render_ui(&mut ui);
            assert!(screen.contains('╔'), "modal without a frame");
        }
    }

    #[test]
    fn stopped_server_prompt_preserves_form_and_can_start_before_deploy() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        app.config.write().unwrap().http.port = listener.local_addr().unwrap().port();
        drop(listener);
        let ssh_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mut ui = test_ui(app);
        ui.deploy = Some(super::super::DeployView {
            rel_path: "image.bin".into(),
            size: 100,
            form: super::super::DeployForm {
                host: "127.0.0.1".into(),
                port: ssh_listener.local_addr().unwrap().port().to_string(),
                username: "test".into(),
                password: "test-password".into(),
                ..Default::default()
            },
            error: None,
        });
        ui.modal = Some(Modal::Deploy);
        super::super::start_deploy(&mut ui);
        assert!(matches!(ui.modal, Some(Modal::ConfirmStart { .. })));
        assert!(render_ui(&mut ui).contains("not started"));
        assert!(ui.app.switches.list().is_empty());
        let key = |c| {
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(c),
                crossterm::event::KeyModifiers::NONE,
            )
        };
        super::super::handle_modal_key(&mut ui, key('n'));
        assert!(matches!(ui.modal, Some(Modal::Deploy)));
        assert_eq!(ui.deploy.as_ref().unwrap().form.password, "test-password");
        assert!(!ui.app.services.status(ServiceId::Http).is_running());
        super::super::start_deploy(&mut ui);
        super::super::handle_modal_key(&mut ui, key('y'));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while ui.pending_deploy.is_some() {
            assert!(std::time::Instant::now() < deadline);
            super::super::pump_pending_deploy(&mut ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ui.app.services.status(ServiceId::Http).is_running());
        assert!(matches!(ui.modal, Some(Modal::Session)));
        assert_eq!(ui.app.switches.list().len(), 1);
        ui.app.switches.list()[0].cancel();
        ui.app.services.stop_all();
    }

    #[test]
    fn server_bind_failure_keeps_form_and_never_starts_ssh() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        app.config.write().unwrap().http.port = occupied.local_addr().unwrap().port();
        app.config.write().unwrap().http.bind = "127.0.0.1".into();
        let mut ui = test_ui(app);
        super::super::open_deploy_modal(&mut ui, "image.bin", 100);
        let form = &mut ui.deploy.as_mut().unwrap().form;
        form.host = "127.0.0.1".into();
        form.username = "test".into();
        form.password = "secret".into();
        super::super::start_deploy(&mut ui);
        super::super::handle_modal_key(
            &mut ui,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('y'),
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while ui.pending_deploy.is_some() {
            assert!(std::time::Instant::now() < deadline);
            super::super::pump_pending_deploy(&mut ui);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(ui.modal, Some(Modal::Deploy)));
        assert!(ui
            .deploy
            .as_ref()
            .unwrap()
            .error
            .as_deref()
            .unwrap()
            .contains("server could not start"));
        assert_eq!(ui.deploy.as_ref().unwrap().form.password, "secret");
        assert!(ui.app.switches.list().is_empty());
    }

    #[test]
    fn session_modal_shows_transfer_size_speed_and_eta() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let switch = test_switch(crate::switch::SwitchState::Busy {
            what: "copying image.bin".into(),
        });
        switch.begin_transfer(
            "image.bin".into(),
            10_000_000,
            crate::session::Protocol::Http,
        );
        let h = ui.app.sessions.open(
            crate::session::Protocol::Http,
            "10.20.30.40:50000".parse().unwrap(),
            8080,
        );
        ui.app.sessions.update(h.id, |s| {
            s.file = Some("image.bin".into());
            s.total = Some(10_000_000);
            s.direction = Some(crate::session::Direction::Download);
            s.state = SessionState::Transferring;
        });
        std::thread::sleep(std::time::Duration::from_millis(60));
        h.add_bytes(5_000_000);
        ui.app.sessions.sample();
        super::super::open_session_view(&mut ui, switch);
        let screen = render_ui(&mut ui);
        assert!(screen.contains("5.0 MB / 10.0 MB"), "missing byte progress");
        assert!(screen.contains("50%"));
        assert!(screen.contains("avg"));
        assert!(screen.contains("ETA"));
    }

    #[test]
    fn long_bulk_ip_list_keeps_credentials_fields_visible() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::activate_upgrade_action(&mut ui, 2);
        super::super::handle_paste(
            &mut ui,
            (1..200)
                .map(|n| format!("10.0.0.{n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let screen = render_ui(&mut ui);
        for field in [
            "username",
            "password",
            "enable password",
            "device IP / Subnet:",
        ] {
            assert!(screen.contains(field), "long IP list hides {field}");
        }
        assert_eq!(ui.deploy.as_ref().unwrap().form.host.lines().count(), 199);
    }

    #[test]
    fn bulk_accepts_ips_and_subnets_and_validates_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::activate_upgrade_action(&mut ui, 2);
        let form = &mut ui.deploy.as_mut().unwrap().form;
        form.host = "192.168.22.0/24, 192.168.11.11, invalid".into();
        form.username = "test".into();
        form.password = "secret".into();
        let screen = render_ui(&mut ui);
        assert!(screen.contains("device IP / Subnet:"));
        assert!(!screen.contains("scan subnet (exp.)"));
        super::super::add_devices(&mut ui);
        assert!(ui.app.switches.list().is_empty());
        assert!(ui.app.switches.scan().is_none());
        assert!(ui.deploy.as_ref().unwrap().error.is_some());
        let targets =
            super::super::bulk_targets("192.168.22.0/24, 192.168.11.11, 192.168.22.0/24").unwrap();
        assert_eq!(targets.subnets, ["192.168.22.0/24"]);
        assert_eq!(targets.direct, ["192.168.11.11"]);
    }

    #[test]
    fn cleanup_confirmation_requires_only_plain_lowercase_y() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        for (key, accepted) in [
            (KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), true),
            (
                KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
                false,
            ),
            (
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                false,
            ),
            (
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                false,
            ),
            (KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), false),
            (KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false),
            (KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), false),
        ] {
            let (switch, mut reply) = crate::switch::Switch::for_test_cleanup();
            ui.session_view = Some(super::super::SessionView {
                switch,
                scroll: None,
                jobs_seen: 0,
            });
            ui.modal = Some(Modal::Session);
            let screen = render_ui(&mut ui);
            assert!(screen.contains("DANGER"));
            assert!(screen.contains("17.12.06.SPA.bin"));
            assert!(screen.contains("ANY OTHER KEY: abort"));
            super::super::handle_key(&mut ui, key);
            assert_eq!(reply.try_recv().unwrap(), accepted, "{key:?}");
        }
        let (switch, mut reply) = crate::switch::Switch::for_test_cleanup();
        ui.session_view = Some(super::super::SessionView {
            switch,
            scroll: None,
            jobs_seen: 0,
        });
        ui.modal = Some(Modal::Session);
        super::super::handle_paste(&mut ui, "y".into());
        assert!(!reply.try_recv().unwrap());
    }

    #[test]
    fn connect_shows_mode_version_and_storage() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Connect;
        let sw = ui.app.switches.connect(crate::switch::Target {
            host: "127.0.0.1".into(),
            port: 1,
            username: "test".into(),
            password: "secret".into(),
            enable_password: String::new(),
            known_hosts: dir.path().join("known_hosts"),
            auto_trust: true,
        });
        sw.facts_for_test(|facts| {
            *facts = test_switch(crate::switch::SwitchState::Ready).facts();
            facts.updated = Some(std::time::Instant::now());
            facts.flash = Some(crate::cisco::FlashUsage {
                free: 1_305_000_000,
                total: 1_957_000_000,
            });
        });
        ui.upgrade_file = Some(("image.bin".into(), 1_305_000_001));
        let screen = render_ui(&mut ui);
        for label in [
            "model",
            "version",
            "mode",
            "C9200L-48P-4X",
            "17.15.03",
            "1305 MB",
            "1957 MB",
        ] {
            assert!(screen.contains(label), "missing {label}: {screen}");
        }
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| draw(f, &mut ui)).unwrap();
        sw.cancel();
    }

    #[test]
    fn add_and_bulk_forms_advance_with_enter_and_require_confirmation() {
        for action in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let (_rt, app) = test_app(dir.path().to_path_buf());
            let mut ui = test_ui(app);
            super::super::activate_upgrade_action(&mut ui, action);
            let form = &mut ui.deploy.as_mut().unwrap().form;
            form.host = "127.0.0.1".into();
            form.port = "1".into();
            form.username = "test".into();
            form.password = "test-password".into();
            let enter = crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            );
            for field in 0..5 {
                super::super::handle_modal_key(&mut ui, enter);
                assert_eq!(ui.deploy.as_ref().unwrap().form.field, field + 1);
                assert!(
                    ui.app.switches.list().is_empty(),
                    "Enter submitted a text field"
                );
            }
            assert_eq!(ui.deploy.as_ref().unwrap().form.field, 5);
            assert!(render_ui(&mut ui).contains("[ Enter ]"));
            assert!(ui.app.switches.list().is_empty());
            super::super::handle_modal_key(&mut ui, enter);
            assert_eq!(ui.app.switches.list().len(), 1);
            assert_eq!(ui.tab, Tab::Connect);
            assert_eq!(ui.modal.is_some(), action == 1);
            let switch = ui.app.switches.list()[0].clone();
            assert!(switch.transfer(&ui.app.sessions).is_none());
            switch.cancel();
        }
    }

    #[test]
    fn upgrade_file_browser_is_above_device_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Upgrade;
        ui.upgrade_file = Some(("images/selected-image.bin".into(), 50_000_000));
        let screen = render_ui(&mut ui);
        let rows: Vec<&str> = screen.lines().collect();
        let file_row = rows
            .iter()
            .position(|row| row.contains("images/selected-image.bin"))
            .unwrap();
        let device_row = rows
            .iter()
            .position(|row| row.contains(" DEVICE JOBS"))
            .unwrap();
        assert!(file_row < device_row);
        let browser_row = rows
            .iter()
            .position(|row| row.contains("FILE BROWSER"))
            .unwrap();
        assert!(browser_row < device_row);
    }

    #[test]
    fn protocol_picker_starts_server_without_leaving_or_deploying_and_keeps_status_colors() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        app.config.write().unwrap().http.port = reserved.local_addr().unwrap().port();
        drop(reserved);
        let mut ui = test_ui(app);
        super::super::open_deploy_modal(&mut ui, "image.bin", 100);
        ui.modal = Some(Modal::DeployProtocol { selected: 0 });
        let assert_color = |ui: &mut Ui, word: &str, expected: Color| {
            let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
            terminal.draw(|f| draw(f, ui)).unwrap();
            let buf = terminal.backend().buffer();
            let mut found = false;
            for y in 0..40 {
                let line: String = (0..120).map(|x| buf[(x, y)].symbol()).collect();
                if let Some(index) = line.find(word) {
                    let x = line[..index].chars().count() as u16;
                    assert_eq!(buf[(x, y)].fg, expected, "wrong color for {word}");
                    found = true;
                }
            }
            assert!(found, "missing {word}");
        };
        assert_color(&mut ui, "Stopped", theme::ERR);
        let start = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('s'),
            crossterm::event::KeyModifiers::NONE,
        );
        super::super::handle_modal_key(&mut ui, start);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while !ui.app.services.status(ServiceId::Http).is_running() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(matches!(
            ui.modal,
            Some(Modal::DeployProtocol { selected: 0 })
        ));
        assert_color(&mut ui, "Running", theme::OK);
        assert!(ui.app.switches.list().is_empty());
        assert!(ui.pending_deploy.is_none());
        super::super::handle_modal_key(&mut ui, start);
        assert!(ui.app.services.status(ServiceId::Http).is_running());
        ui.app.services.stop_all();
    }

    #[test]
    fn protocol_enter_opens_picker_and_returns_to_form() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::open_deploy_modal(&mut ui, "image.bin", 100);
        ui.deploy.as_mut().unwrap().form.field = 5;
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        super::super::handle_modal_key(&mut ui, key);
        assert!(matches!(ui.modal, Some(Modal::DeployProtocol { .. })));
        assert!(render_ui(&mut ui).contains("PROTOCOL"));
        super::super::handle_modal_key(&mut ui, key);
        assert!(matches!(ui.modal, Some(Modal::Deploy)));
        assert!(ui.app.switches.list().is_empty());
    }

    #[test]
    fn bulk_add_validates_entire_list_then_connects_without_a_deploy() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::activate_upgrade_action(&mut ui, 2);
        let view = ui.deploy.as_mut().unwrap();
        view.form.host = "127.0.0.1;invalid".into();
        view.form.port = "1".into();
        view.form.username = "test".into();
        view.form.password = "password".into();
        view.form.proto = crate::session::Protocol::Sftp;
        super::super::add_devices(&mut ui);
        assert!(ui.deploy.as_ref().unwrap().error.is_some());
        assert!(ui.app.switches.list().is_empty());
        ui.deploy.as_mut().unwrap().form.host = "127.0.0.1;127.0.0.2\n127.0.0.1".into();
        super::super::add_devices(&mut ui);
        let devices = ui.app.switches.list();
        assert_eq!(devices.len(), 2);
        for device in devices {
            assert!(!device.protocol_chosen());
            assert!(device.transfer(&ui.app.sessions).is_none());
            device.cancel();
        }
        assert!(!ui.app.services.status(ServiceId::Ssh).is_running());
        assert!(ui.modal.is_none());
    }

    #[test]
    fn upgrade_file_picker_selects_a_file_without_deploying() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("images")).unwrap();
        std::fs::write(dir.path().join("images/image.bin"), b"image").unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::activate_upgrade_action(&mut ui, 0);
        let key = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        super::super::handle_modal_key(&mut ui, key);
        assert_eq!(ui.files.cwd, "images");
        assert_eq!(ui.files.visible()[0].name, "..");
        ui.files.selected = 1;
        super::super::handle_modal_key(&mut ui, key);
        assert_eq!(ui.upgrade_file, Some(("images/image.bin".into(), 5)));
        assert!(ui.modal.is_none());
        assert!(ui.app.switches.list().is_empty());
    }

    #[test]
    fn logs_wrap_to_show_transfer_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Logs;
        ui.app.logger.log(
            crate::logging::Event::new(LogLevel::Info, "ftp", "download")
                .path("cat9k_lite_iosxe.17.15.06.SPA.bin")
                .session(42)
                .ip("10.40.40.56".parse().unwrap())
                .result("ok")
                .bytes(32_000_000)
                .duration_ms(2000),
        );
        let screen = render_ui(&mut ui);
        assert!(screen.contains("32.0 MB"));
        assert!(screen.contains("16.0 MB/s"));
    }

    #[test]
    fn deploy_form_renders_with_a_command_preview() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("image.bin"), b"x").unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Files;
        ui.modal = Some(Modal::Deploy);

        let form = super::super::DeployForm {
            host: "10.20.30.40".into(),
            username: "netadmin".into(),
            password: "letmein".into(),
            ..Default::default()
        };
        ui.deploy = Some(super::super::DeployView {
            rel_path: "image.bin".into(),
            size: 504_057_659,
            form,
            error: None,
        });

        let screen = render_ui(&mut ui);
        for label in DeployField::ALL.iter().map(|f| f.label()) {
            assert!(screen.contains(label), "form is missing {label}");
        }
        assert!(screen.contains("copy http://"), "no command preview");
        assert!(screen.contains("never a config"), "missing the safety note");
        assert!(!screen.contains("letmein"), "SSH password must be masked");
    }

    #[test]
    fn session_modal_shows_facts_and_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.modal = Some(Modal::Session);

        // Busy: the header names the job, the flash figure is in MB and the
        // stack members are listed.
        let switch = test_switch(crate::switch::SwitchState::Busy {
            what: "copying cat9k_lite_iosxe.17.15.06.SPA.bin".into(),
        });
        ui.session_view = Some(super::super::SessionView {
            switch: switch.clone(),
            scroll: None,
            jobs_seen: 0,
        });
        let screen = render_ui(&mut ui);
        assert!(screen.contains("SG-AS-OG5-01"), "no device name");
        assert!(screen.contains("copying cat9k"), "no job label");
        assert!(
            screen.contains("235 MB free of 1957 MB"),
            "flash not in MB: {screen}"
        );
        assert!(screen.contains("17.15.03"), "no IOS version");
        assert!(screen.contains("C9200L-48P-4X"), "no model");
        assert!(screen.contains("*2:17.15.03"), "no stack members");
        assert!(screen.contains("terminal length 0"), "no transcript");
        assert!(screen.contains("1.2 ms"), "no ping");

        // The host key question replaces the whole body.
        ui.session_view.as_mut().unwrap().switch =
            test_switch(crate::switch::SwitchState::HostKey {
                fingerprint: "SHA256:abc123".into(),
            });
        let screen = render_ui(&mut ui);
        assert!(screen.contains("SHA256:abc123"), "no fingerprint");
        assert!(screen.contains("unknown host key"), "no warning");
    }

    /// The popup must never be a dead end: y accepts, anything else rejects,
    /// and the rejection has to reach the session that is waiting for it.
    #[test]
    fn host_key_popup_always_answers() {
        use crossterm::event::{KeyCode, KeyEvent};

        for (key, expected) in [
            (KeyCode::Char('y'), Some(true)),
            (KeyCode::Char('n'), Some(false)),
            (KeyCode::Enter, Some(false)),
            (KeyCode::Esc, Some(false)),
            (KeyCode::Char('q'), Some(false)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (_rt, app) = test_app(dir.path().to_path_buf());
            let mut ui = test_ui(app);
            let (switch, mut rx) = crate::switch::Switch::for_test_host_key("SHA256:abc");
            ui.session_view = Some(super::super::SessionView {
                switch: switch.clone(),
                scroll: None,
                jobs_seen: 0,
            });
            ui.modal = Some(Modal::Session);

            super::super::handle_key(&mut ui, KeyEvent::from(key));
            assert_eq!(rx.try_recv().ok(), expected, "wrong answer for {key:?}");

            // Rejecting also aborts, so a session cannot be left parked.
            if expected == Some(false) {
                assert!(
                    switch.cancel_requested() || rx.try_recv().is_err(),
                    "{key:?} left the session waiting"
                );
            }
        }
    }

    #[test]
    fn switches_view_lists_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Connect;

        // Empty: the view explains how to get a session.
        let screen = render_ui(&mut ui);
        assert!(screen.contains("CONNECTIONS"), "no table");
        assert!(screen.contains("a: add"), "no hint for the empty view");

        // A half-upgraded stack must stand out in the detail pane.
        let switch = test_switch(crate::switch::SwitchState::Ready);
        switch.facts_for_test(|f| {
            if let Some(v) = f.version.as_mut() {
                v.members[2].version = "17.15.06".into();
            }
        });
        ui.session_view = Some(super::super::SessionView {
            switch,
            scroll: None,
            jobs_seen: 0,
        });
        ui.modal = Some(Modal::Session);
        let screen = render_ui(&mut ui);
        assert!(
            screen.contains("members differ"),
            "half-upgraded stack not flagged"
        );
    }

    #[test]
    fn help_columns_are_roughly_balanced() {
        let left = help_column(HELP_LEFT).len();
        let right = help_column(HELP_RIGHT).len();
        // Both tables should be within a few rows of each other, otherwise the
        // help popup looks lopsided.
        assert!(left.abs_diff(right) <= 4, "left {left}, right {right}");
    }

    /// Render the help popup into a test terminal and return the plain text
    /// of every row, so the layout can be asserted on.
    fn render_help(width: u16, height: u16, scroll: usize) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_help(f, scroll)).unwrap();
        let buf = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn help_shows_every_key_on_a_40_row_terminal() {
        let screen = render_help(120, 40, 0).join("\n");
        for (_, rows) in HELP_LEFT.iter().chain(HELP_RIGHT.iter()) {
            for (key, desc) in rows.iter() {
                assert!(
                    screen.contains(desc),
                    "missing help entry {key} — {desc}:\n{screen}"
                );
            }
        }
        // Closed box: the double border must be complete top and bottom.
        assert!(screen.contains('╔') && screen.contains('╚'));
    }

    #[test]
    fn help_key_columns_line_up() {
        let screen = render_help(120, 40, 0);
        // Description columns of both tables start at fixed offsets, and the
        // same offsets are used by every section.
        // Column, not byte offset — the key column contains arrows.
        let col_of = |needle: &str| -> Option<usize> {
            screen
                .iter()
                .find_map(|l| l.find(needle).map(|b| l[..b].chars().count()))
        };
        let left_a = col_of("select service").unwrap();
        let left_b = col_of("sort / refresh listing").unwrap();
        assert_eq!(left_a, left_b, "left table not aligned");
        let right_a = col_of("select tab").unwrap();
        let right_b = col_of("protocol filter").unwrap();
        assert_eq!(right_a, right_b, "right table not aligned");
    }

    #[test]
    fn help_scrolls_on_a_short_terminal() {
        let short = render_help(120, 20, 0).join("\n");
        // Not everything fits, so the last entries are off-screen ...
        assert!(!short.contains("protocol filter"));
        assert!(short.contains("scroll"));
        // ... and scrolling to the bottom brings them in. The scroll is
        // clamped, so this stays right as the tables grow.
        let scrolled = render_help(120, 20, usize::MAX).join("\n");
        assert!(scrolled.contains("protocol filter"));
    }

    #[test]
    fn every_help_key_has_a_description() {
        for (_, rows) in HELP_LEFT.iter().chain(HELP_RIGHT.iter()) {
            for (key, desc) in rows.iter() {
                assert!(!key.is_empty());
                assert!(!desc.is_empty());
                // Keys share a 7-column field, so they must fit.
                assert!(key.chars().count() <= 6, "key too wide: {key}");
            }
        }
    }
    #[test]
    fn workflow_keyboard_paste_edits_fields_but_cannot_approve_actions() {
        use super::super::workflow::{self, Field};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        workflow::open(&mut ui);
        ui.workflow.edit = Some(Field::Source);
        super::super::handle_paste(&mut ui, "abc.txt".into());
        assert_eq!(ui.workflow.source, "abc.txt");
        ui.workflow.edit = None;
        super::super::handle_paste(&mut ui, "y".into());
        assert_eq!(ui.workflow.source, "abc.txt");
        let text = render_ui(&mut ui);
        assert!(text.contains("WORKFLOW ASSISTANT"));
    }
    #[test]
    fn workflow_login_masks_password_and_supports_paste_and_clear() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        let form = super::super::DeployForm {
            field: 1,
            ..Default::default()
        };
        ui.modal = Some(super::super::Modal::WorkflowLogin { device: 1, form });
        super::super::handle_paste(&mut ui, "ssh-secret".into());
        let text = render_ui(&mut ui);
        assert!(!text.contains("ssh-secret"));
        assert!(text.contains("••"));
        super::super::handle_key(
            &mut ui,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        );
        assert!(
            matches!(ui.modal,Some(super::super::Modal::WorkflowLogin { ref form, .. }) if form.password.is_empty())
        );
    }
}
