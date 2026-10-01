use chrono::{DateTime, Local};
use ratatui::prelude::*;
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs, Wrap,
};

use crate::logging::LogLevel;
use crate::services::{ServiceId, ServiceStatus};
use crate::session::{fmt_bytes, fmt_duration, fmt_speed, SessionState};
use crate::switch::{LineKind, SwitchState};

use super::{theme, DeployField, EditField, Modal, Tab, Ui};

pub fn draw(f: &mut Frame, ui: &mut Ui) {
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
        Tab::Services => draw_services(f, ui, chunks[1]),
        Tab::Files => draw_files(f, ui, chunks[1]),
        Tab::Sessions => draw_sessions(f, ui, chunks[1]),
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
            ("q", "quit"),
            ("h", "help"),
            ("m", "sound"),
            ("i", "address"),
            ("←/→", "view"),
        ],
        Tab::Services => vec![
            ("Enter", "edit"),
            ("Space", "on/off"),
            ("s", "start/stop"),
            ("r", "restart"),
            ("S", "all"),
            ("X", "stop all"),
            ("p", "port"),
            ("b", "bind"),
            ("n", "user"),
            ("w", "pass"),
            ("u", "uploads"),
            ("d", "dir"),
            ("L", "logs"),
        ],
        Tab::Files => vec![
            ("Enter", "open"),
            ("Bksp", "up"),
            ("d", "deploy"),
            ("H", "hashes"),
            ("s", "sort"),
            ("/", "filter"),
            ("R", "refresh"),
            ("y", "copy"),
        ],
        Tab::Sessions => vec![("↑↓", "select"), ("B", "bit/byte"), ("←/→", "view")],
        Tab::Upgrade => vec![
            ("Tab", "menu/devices"),
            ("a/b", "add/import"),
            ("f", "file"),
            ("d", "deploy"),
            ("Enter", "select"),
            ("r", "refresh"),
            ("x", "disconnect"),
            ("X", "clear closed"),
        ],
        Tab::Logs => vec![
            ("↑↓", "scroll"),
            ("G", "follow"),
            ("/", "filter"),
            ("L", "level"),
            ("P", "proto"),
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

fn draw_dashboard(f: &mut Frame, ui: &Ui, area: Rect) {
    // The log pane gets its own full-width block at the bottom and all the
    // space the other panes do not need.
    let chunks = Layout::vertical([
        Constraint::Length(7),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Min(5),
    ])
    .split(area);

    let cfg = ui.app.config.read().unwrap();
    let (completed, bytes) = ui.app.sessions.totals();
    let active = ui.app.sessions.active_count();
    let (advertised, advertised_source) = super::advertised_now(&cfg);
    let advertised = advertised
        .map(|i| i.to_string())
        .unwrap_or_else(|| "-".into());
    let summary = vec![
        Line::from(vec![
            theme::label("root:              "),
            theme::value(cfg.root.display().to_string()),
        ]),
        Line::from(vec![
            theme::label("address in URLs:   "),
            Span::styled(advertised, Style::default().fg(theme::HILITE).bold()),
            theme::label(format!("  ({advertised_source}, ")),
            theme::key("i"),
            theme::label(" changes it)   privileged: "),
            theme::value(if ui.app.privileged {
                "yes (sudo)"
            } else {
                "no"
            }),
        ]),
        Line::from(vec![
            theme::label("credentials:       "),
            Span::styled(
                format!("{} / {}", cfg.auth.username, cfg.auth.password),
                Style::default().fg(theme::ACCENT),
            ),
        ]),
        Line::from(vec![
            theme::label("sessions:          "),
            theme::value(format!("{active} active")),
            theme::label("   transfers: "),
            theme::value(format!(
                "{completed} completed, {} served",
                fmt_bytes(bytes)
            )),
        ]),
        Line::from(vec![
            theme::label("log file:          "),
            theme::value(
                ui.app
                    .logger
                    .file_path()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(disabled)".into()),
            ),
        ]),
    ];
    f.render_widget(
        Paragraph::new(summary).block(theme::panel_double(&format!(
            " TRANSFERBUDDY v{} ",
            crate::VERSION
        ))),
        chunks[0],
    );

    let rows: Vec<Row> = ServiceId::ALL
        .iter()
        .map(|id| {
            let sc = cfg.service(*id);
            let status = ui.app.services.status(*id);
            let uptime = ui
                .app
                .services
                .uptime(*id)
                .map(fmt_duration)
                .unwrap_or_else(|| "-".into());
            let sessions = match id {
                ServiceId::Ssh => {
                    ui.app
                        .sessions
                        .active_count_for(crate::session::Protocol::Sftp)
                        + ui.app
                            .sessions
                            .active_count_for(crate::session::Protocol::Scp)
                }
                ServiceId::Http => ui
                    .app
                    .sessions
                    .active_count_for(crate::session::Protocol::Http),
                ServiceId::Https => ui
                    .app
                    .sessions
                    .active_count_for(crate::session::Protocol::Https),
                ServiceId::Ftp => ui
                    .app
                    .sessions
                    .active_count_for(crate::session::Protocol::Ftp),
                ServiceId::Tftp => ui
                    .app
                    .sessions
                    .active_count_for(crate::session::Protocol::Tftp),
            };
            let crypto = if id.encrypted() {
                Span::styled("encrypted", Style::default().fg(theme::OK))
            } else {
                Span::styled("CLEARTEXT", Style::default().fg(theme::WARN).bold())
            };
            let err = match &status {
                ServiceStatus::Failed(e) => e.clone(),
                _ => String::new(),
            };
            Row::new(vec![
                Cell::from(Span::styled(
                    id.display_name(),
                    Style::default().fg(theme::CYAN).bold(),
                )),
                Cell::from(Span::styled(
                    status.label().to_string(),
                    status_style(&status),
                )),
                Cell::from(if sc.enabled {
                    Span::styled("on", Style::default().fg(theme::OK))
                } else {
                    Span::styled("off", Style::default().fg(theme::DIM))
                }),
                Cell::from(sc.port.to_string()),
                Cell::from(sc.bind.clone()),
                Cell::from(sessions.to_string()),
                Cell::from(uptime),
                Cell::from(crypto),
                Cell::from(Span::styled(err, Style::default().fg(theme::ERR))),
            ])
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Length(9),
            Constraint::Length(9),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(16),
            Constraint::Length(5),
            Constraint::Length(9),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .style(Style::default().fg(theme::TEXT))
    .header(
        Row::new(vec![
            "service", "status", "on", "port", "bind", "sess", "uptime", "crypto", "error",
        ])
        .style(theme::header()),
    )
    .block(theme::panel(" SERVICES (2) "));
    f.render_widget(table, chunks[1]);

    // Quick views: files, sessions, interfaces.
    let cols = Layout::horizontal([
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
        Constraint::Ratio(1, 3),
    ])
    .split(chunks[2]);

    let (n_files, n_dirs, total, recent) = scan_root(&cfg.root);
    let mut file_lines = vec![
        Line::from(vec![
            theme::label("items: "),
            theme::value(format!("{n_files} files, {n_dirs} dirs")),
        ]),
        Line::from(vec![
            theme::label("size:  "),
            theme::value(fmt_bytes(total)),
        ]),
    ];
    if !recent.is_empty() {
        file_lines.push(Line::from(theme::label("recent:")));
        for name in &recent {
            file_lines.push(Line::from(Span::styled(
                format!("  {name}"),
                Style::default().fg(theme::CYAN),
            )));
        }
    }
    f.render_widget(
        Paragraph::new(file_lines).block(theme::panel(" FILES (3) ")),
        cols[0],
    );

    let snap = ui.app.sessions.snapshot();
    let mut sess_lines = vec![
        Line::from(vec![
            theme::label("now:   "),
            theme::value(format!("{active} active")),
        ]),
        Line::from(vec![
            theme::label("total: "),
            theme::value(format!("{completed} done, {}", fmt_bytes(bytes))),
        ]),
    ];
    if snap.is_empty() {
        sess_lines.push(Line::from(theme::label("  no sessions yet")));
    } else {
        for s in snap.iter().rev().take(4) {
            let color = if s.state.is_active() {
                theme::HILITE
            } else {
                theme::DIM
            };
            sess_lines.push(Line::from(Span::styled(
                format!(
                    "  {} {} {}",
                    s.protocol.label(),
                    s.state.label(),
                    s.file.clone().unwrap_or_default()
                ),
                Style::default().fg(color),
            )));
        }
    }
    f.render_widget(
        Paragraph::new(sess_lines).block(theme::panel(" SESSIONS (4) ")),
        cols[1],
    );

    let (in_use, pinned) = {
        let cfg = ui.app.config.read().unwrap();
        (super::advertised_now(&cfg).0, cfg.advertise.is_some())
    };
    let mut if_lines = Vec::new();
    for ifa in crate::netif::interfaces().into_iter().take(6) {
        let active = Some(ifa.ip) == in_use;
        if_lines.push(Line::from(vec![
            Span::styled(
                if active { "→ " } else { "  " },
                Style::default().fg(theme::HILITE).bold(),
            ),
            Span::styled(
                format!("{:<7} {}", ifa.name, ifa.ip),
                if active {
                    Style::default().fg(theme::HILITE).bold()
                } else {
                    Style::default().fg(theme::TEXT)
                },
            ),
            theme::label(format!("  {}", ifa.kind.label())),
        ]));
    }
    if_lines.push(Line::from(vec![
        Span::raw("  "),
        theme::key("i"),
        theme::label(if pinned {
            " pinned — cycle"
        } else {
            " pin an interface"
        }),
    ]));
    f.render_widget(
        Paragraph::new(if_lines).block(theme::panel(" INTERFACES ")),
        cols[2],
    );

    // Logs — full width, own block, everything that is left over.
    let log_height = chunks[3].height.saturating_sub(2) as usize;
    let entries = ui.app.logger.entries();
    let log_lines: Vec<Line> = entries
        .iter()
        .rev()
        .take(log_height.max(1))
        .rev()
        .map(|e| {
            Line::from(Span::styled(
                e.render_line(),
                Style::default().fg(level_color(e.level)),
            ))
        })
        .collect();
    f.render_widget(
        Paragraph::new(if log_lines.is_empty() {
            vec![Line::from(theme::label("  no log entries"))]
        } else {
            log_lines
        })
        .block(theme::panel(" LOGS (5) ")),
        chunks[3],
    );
}

/// Lightweight top-level scan of the shared root for the dashboard summary.
fn scan_root(root: &std::path::Path) -> (usize, usize, u64, Vec<String>) {
    let mut files = 0usize;
    let mut dirs = 0usize;
    let mut total = 0u64;
    let mut recent: Vec<(std::time::SystemTime, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                dirs += 1;
            } else {
                files += 1;
                total += meta.len();
                if let Ok(m) = meta.modified() {
                    recent.push((m, name));
                }
            }
        }
    }
    recent.sort_by(|a, b| b.0.cmp(&a.0));
    let recent = recent.into_iter().take(4).map(|(_, n)| n).collect();
    (files, dirs, total, recent)
}

fn draw_services(f: &mut Frame, ui: &Ui, area: Rect) {
    let chunks = Layout::vertical([Constraint::Min(7), Constraint::Length(9)]).split(area);
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
        " SERVICES — Space on/off, s start/stop, Enter edit ",
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

fn draw_files(f: &mut Frame, ui: &mut Ui, area: Rect) {
    let cfg = ui.app.config.read().unwrap();
    let title = format!(
        " /{}  (sort: {}{}) ",
        ui.files.cwd,
        ui.files.sort.label(),
        if ui.files.filter.is_empty() {
            String::new()
        } else {
            format!(", filter: \"{}\"", ui.files.filter)
        }
    );
    let visible: Vec<super::FileEntry> = ui.files.visible().into_iter().cloned().collect();
    let height = area.height.saturating_sub(3) as usize;
    let offset = ui.files.selected.saturating_sub(height.saturating_sub(1));
    let rows: Vec<Row> = visible
        .iter()
        .enumerate()
        .skip(offset)
        .take(height.max(1))
        .map(|(i, e)| {
            let style = if i == ui.files.selected {
                theme::selected()
            } else if e.is_dir {
                Style::default().fg(theme::CYAN)
            } else {
                Style::default().fg(theme::TEXT)
            };
            let modified = e
                .modified
                .map(|t| {
                    DateTime::<Local>::from(t)
                        .format("%Y-%m-%d %H:%M")
                        .to_string()
                })
                .unwrap_or_default();
            let rel = if ui.files.cwd.is_empty() {
                format!("/{}", e.name)
            } else {
                format!("/{}/{}", ui.files.cwd, e.name)
            };
            Row::new(vec![
                Cell::from(format!("{}{}", e.name, if e.is_dir { "/" } else { "" })),
                Cell::from(if e.is_dir {
                    "-".into()
                } else {
                    fmt_bytes(e.size)
                }),
                Cell::from(modified),
                Cell::from(if e.is_dir {
                    "dir".into()
                } else {
                    e.ext.clone()
                }),
                Cell::from(rel),
            ])
            .style(style)
        })
        .collect();
    let table = Table::new(
        rows,
        [
            Constraint::Min(24),
            Constraint::Length(10),
            Constraint::Length(17),
            Constraint::Length(6),
            Constraint::Min(20),
        ],
    )
    .header(
        Row::new(vec!["name", "size", "modified", "type", "download path"]).style(theme::header()),
    )
    .block(theme::panel(&title));
    f.render_widget(table, area);
    if let Some(err) = &ui.files.error {
        let p = Paragraph::new(err.clone()).style(Style::default().fg(theme::ERR));
        let mut inner = area;
        inner.y = area.y + 1;
        inner.height = 1;
        f.render_widget(p, inner);
    }
    drop(cfg);
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

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
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
        "SERVICES (2)",
        &[
            ("↑ ↓", "select service"),
            ("Enter", "edit popup"),
            ("Space", "enable / disable"),
            ("s", "start / stop"),
            ("r", "restart"),
            ("S", "start all enabled"),
            ("X", "stop all"),
            ("p", "port"),
            ("b", "bind address"),
            ("n", "username"),
            ("w", "password"),
            ("u", "uploads on / off"),
            ("d", "upload directory"),
            ("L", "logs of this service"),
        ],
    ),
    (
        "FILES (3)",
        &[
            ("↑ ↓", "select entry"),
            ("Enter", "open dir / cisco cmds"),
            ("Bksp", "parent directory"),
            ("d", "deploy to a switch"),
            ("y", "copy cisco command"),
            ("H", "hashes + compare"),
            ("s", "cycle sort order"),
            ("/", "filter by name"),
            ("R", "refresh listing"),
        ],
    ),
    (
        "UPGRADE (6)",
        &[
            ("a / b", "add device / bulk import"),
            ("f / d", "choose file / deploy"),
            ("i / S", "cleanup / cancel scan"),
            ("Tab", "menu / devices"),
        ],
    ),
    (
        "SESSIONS (4)",
        &[("↑ ↓", "select session"), ("B", "bit/s ⇄ byte/s")],
    ),
];

/// Right help table.
const HELP_RIGHT: &[(&str, KeyRows)] = &[
    (
        "GLOBAL",
        &[
            ("1", "dashboard"),
            ("2", "services"),
            ("3", "files"),
            ("4", "sessions"),
            ("5", "logs"),
            ("6", "upgrade"),
            ("← →", "previous / next view"),
            ("Tab", "next view"),
            ("h", "this help"),
            ("i", "address used in URLs"),
            ("m", "sound on / off"),
            ("q", "quit"),
            ("^C", "quit"),
        ],
    ),
    (
        "DEPLOY (d) + UPGRADE (6)",
        &[
            ("↑ ↓", "form field / session"),
            ("^S", "start selected server"),
            ("^Enter", "submit from any field"),
            ("← →", "protocol / toggle"),
            ("Enter", "protocol / submit / open"),
            ("r", "re-read dir + version"),
            ("x", "disconnect"),
            ("X", "clear closed rows"),
            ("c / x", "abort / disconnect"),
            ("y", "trust the host key"),
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
        .min(super::deploy_fields(ui.deploy_mode).len() as u16 + 12);
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
            DeployField::ScanSubnet => {
                if view.form.scan_subnet {
                    "yes — experimental IPv4 ping sweep".into()
                } else {
                    "no".into()
                }
            }
            DeployField::Overwrite => {
                if view.form.overwrite {
                    "yes".into()
                } else {
                    "no".into()
                }
            }
            DeployField::Submit => match ui.deploy_mode {
                super::DeployMode::Bulk if view.form.scan_subnet => "[ Enter ]  Scan subnet".into(),
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
                        if view.form.scan_subnet {
                            "IPv4 subnet / CIDR"
                        } else {
                            "device IPs"
                        }
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
            lines.push(Line::from(theme::label(if view.form.scan_subnet {
                " Experimental: 192.168.10.0/24; 32 parallel pings, SSH only to responding IPs."
            } else {
                " IPs: spaces, commas, semicolons or pasted newlines. Credentials apply to all devices."
            })));
            lines.push(Line::from(Span::styled(
                " Bulk/scan automatically trusts unknown AND changed SSH host keys.",
                Style::default().fg(theme::WARN),
            )));
        }
        lines.push(Line::from(theme::label(
            " Checks SSH and reads device facts. Start the file deploy separately with d.",
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
        theme::label(" protocol, overwrite  "),
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

    let chunks = Layout::vertical([
        Constraint::Length(7),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(inner);

    let mut summary = switch_summary(switch);
    summary.extend(transfer_summary(ui, switch));
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
    let panes = Layout::horizontal([
        Constraint::Length(if area.width < 105 { 0 } else { 23 }),
        Constraint::Min(30),
    ])
    .split(area);
    let mut menu = Vec::new();
    for (i, label) in super::UPGRADE_ACTIONS.iter().enumerate() {
        menu.push(Line::from(Span::styled(
            format!(
                " {} {label}",
                if ui.upgrade_menu == Some(i) { ">" } else { " " }
            ),
            if ui.upgrade_menu == Some(i) {
                theme::selected()
            } else {
                Style::default().fg(theme::TEXT)
            },
        )));
        menu.push(Line::from(""));
    }
    menu.push(Line::from(theme::label(" Tab: menu / devices")));
    menu.push(Line::from(theme::label(" Enter: select")));
    menu.push(Line::from(theme::label(" a: Add   b: Bulk")));
    menu.push(Line::from(theme::label(" d: Deploy selected")));
    menu.push(Line::from(theme::label(" i: Remove inactive")));
    menu.push(Line::from(theme::label(" S: Cancel scan")));
    f.render_widget(
        Paragraph::new(menu)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" UPGRADE ")),
        panes[0],
    );
    let right = Layout::vertical([
        Constraint::Length(if ui.app.switches.scan().is_some() {
            7
        } else {
            4
        }),
        Constraint::Min(6),
    ])
    .split(panes[1]);
    let mut file = match &ui.upgrade_file {
        Some((path, size)) => vec![
            Line::from(Span::styled(
                format!(" /{path}"),
                Style::default().fg(theme::CYAN).bold(),
            )),
            Line::from(theme::value(format!(" {}", fmt_bytes(*size)))),
        ],
        None => vec![Line::from(theme::label(
            " No file selected — press f to choose one.",
        ))],
    };
    if let Some(scan) = ui.app.switches.scan() {
        let progress = scan.progress();
        let state = if progress.cancelled {
            "cancelled"
        } else if progress.finished {
            "ping sweep complete"
        } else {
            "scanning"
        };
        file.push(Line::from(Span::styled(
            format!(
                " SCAN {}: {state} — {}/{} pinged, {} reachable",
                progress.subnet, progress.checked, progress.total, progress.reachable
            ),
            Style::default().fg(theme::CYAN),
        )));
        file.push(Line::from(theme::label(format!(
            " Last: {} | reachable devices connect via SSH below | S: cancel scan",
            progress.last_host
        ))));
        if let Some(error) = progress.error {
            file.push(Line::from(Span::styled(
                error,
                Style::default().fg(theme::ERR),
            )));
        }
    }
    f.render_widget(
        Paragraph::new(file).block(theme::panel(" SELECTED FILE / DISCOVERY ")),
        right[0],
    );
    let area = right[1];

    let switches = ui.app.switches.list();
    if ui.switch_sel >= switches.len() {
        ui.switch_sel = switches.len().saturating_sub(1);
    }

    let wide = area.width >= 155;
    let compact = area.width < 90;
    let labels = if wide {
        vec![
            "name / host",
            "model",
            "version",
            "stack",
            "flash usage",
            "file fits",
            "state",
            "protocol",
            "progress",
            "speed",
            "ETA",
        ]
    } else {
        vec![
            "name / host",
            "model",
            "version",
            "stack",
            "flash usage",
            "fits",
            "state",
        ]
    };
    let header = Row::new(labels).style(theme::header());

    let rows: Vec<Row> = switches
        .iter()
        .enumerate()
        .map(|(i, sw)| {
            let state = sw.state();
            let state_style = match &state {
                SwitchState::Ready => Style::default().fg(theme::OK),
                SwitchState::Busy { .. } => Style::default().fg(theme::HILITE).bold(),
                SwitchState::Failed { .. } | SwitchState::Offline { .. } => {
                    Style::default().fg(theme::ERR)
                }
                _ => Style::default().fg(theme::WARN),
            };
            let transfer = sw.transfer(&ui.app.sessions);
            let stats = transfer.as_ref().and_then(|t| t.session.as_ref());
            let finished = transfer.as_ref().is_some_and(|t| t.ended.is_some());
            let facts = sw.facts();
            let version = facts.version.unwrap_or_default();
            let stack_count = if !version.members.is_empty() {
                version.members.len().to_string()
            } else if version.version.is_some() || version.model.is_some() {
                "1".into()
            } else {
                "?".into()
            };
            let flash = facts
                .flash
                .map(|usage| {
                    if wide {
                        format!(
                            "{} free of {} ({:.0}% used)",
                            crate::switch::fmt_mb(usage.free),
                            crate::switch::fmt_mb(usage.total),
                            usage.used_fraction() * 100.0
                        )
                    } else if compact {
                        format!(
                            "{} free\nof {}\n({:.0}% used)",
                            crate::switch::fmt_mb(usage.free),
                            crate::switch::fmt_mb(usage.total),
                            usage.used_fraction() * 100.0
                        )
                    } else {
                        format!(
                            "{} free of {}\n({:.0}% used)",
                            crate::switch::fmt_mb(usage.free),
                            crate::switch::fmt_mb(usage.total),
                            usage.used_fraction() * 100.0
                        )
                    }
                })
                .unwrap_or_else(|| "unknown".into());
            let fits = ui
                .upgrade_file
                .as_ref()
                .and_then(|(_, size)| facts.flash.map(|usage| usage.fits(*size)));
            let fit_label = match fits {
                Some(true) => "yes",
                Some(false) => "NO SPACE",
                None => {
                    if ui.upgrade_file.is_some() {
                        "unknown"
                    } else {
                        "no file"
                    }
                }
            };
            let mut cells = vec![
                Cell::from(format!("{}\n{}:{}", sw.display_name(), sw.host, sw.port)),
                Cell::from(version.model.unwrap_or_else(|| "?".into())),
                Cell::from(version.version.unwrap_or_else(|| "?".into())),
                Cell::from(stack_count),
                Cell::from(flash),
                Cell::from(fit_label).style(
                    Style::default()
                        .fg(match fits {
                            Some(true) => theme::OK,
                            Some(false) => theme::ERR,
                            None => theme::WARN,
                        })
                        .bold(),
                ),
                Cell::from(state.label()).style(state_style),
            ];
            if wide {
                cells.extend([
                    Cell::from(sw.protocol().label()),
                    Cell::from(
                        stats
                            .and_then(|s| s.progress())
                            .map(|p| format!("{:.0}%", p * 100.0))
                            .unwrap_or_else(|| "—".into()),
                    ),
                    Cell::from(
                        stats
                            .map(|s| {
                                fmt_speed(
                                    if finished { 0.0 } else { s.current_speed },
                                    ui.app.config.read().unwrap().speed_in_bits,
                                )
                            })
                            .unwrap_or_else(|| "—".into()),
                    ),
                    Cell::from(
                        stats
                            .and_then(|s| if finished { None } else { s.eta() })
                            .map(fmt_duration)
                            .unwrap_or_else(|| "—".into()),
                    ),
                ]);
            }
            let row = Row::new(cells).height(if compact { 3 } else { 2 });
            if i == ui.switch_sel && ui.upgrade_menu.is_none() {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(11)]).split(area);
    let mut widths = if compact {
        vec![
            Constraint::Min(10),
            Constraint::Length(12),
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Length(14),
            Constraint::Length(6),
            Constraint::Length(7),
        ]
    } else {
        vec![
            Constraint::Min(12),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(5),
            Constraint::Length(if wide { 40 } else { 27 }),
            Constraint::Length(8),
            Constraint::Length(10),
        ]
    };
    if wide {
        widths.extend([
            Constraint::Length(8),
            Constraint::Length(9),
            Constraint::Length(13),
            Constraint::Length(6),
        ]);
    }
    let mut table_state = TableState::default().with_selected(Some(ui.switch_sel));
    f.render_stateful_widget(
        Table::new(rows, widths)
            .header(header)
            .block(theme::panel(" DEVICES ")),
        chunks[0],
        &mut table_state,
    );

    let detail = match switches.get(ui.switch_sel) {
        Some(sw) => {
            let mut lines = switch_summary(sw);
            lines.extend(transfer_summary(ui, sw));
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw(" "),
                theme::key("Enter"),
                theme::label(" open the session   "),
                theme::key("r"),
                theme::label(" refresh facts   "),
                theme::key("i"),
                theme::label(" remove inactive   "),
                theme::key("x"),
                theme::label(" disconnect   "),
                theme::key("X"),
                theme::label(" clear closed"),
            ]));
            lines
        }
        None => vec![
            Line::from(""),
            Line::from(theme::label(
                "  No device yet. Add device (a) or Bulk import (b) checks SSH without starting a deploy.",
            )),
            Line::from(theme::label(
                "  Choose file (f), select a device with Tab / ↑↓, then deploy separately with d.",
            )),
        ],
    };
    f.render_widget(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .block(theme::panel(" DEVICE ")),
        chunks[1],
    );
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
        let mut cfg = crate::config::Config::default();
        cfg.config_dir = root.join(".test-config");
        cfg.sound = false;
        cfg.root = root;
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
        });
        app.services.attach_app(&app);
        (rt, app)
    }

    fn test_ui(app: crate::SharedApp) -> Ui {
        Ui {
            app,
            tab: Tab::Dashboard,
            service_sel: 0,
            files: super::super::FileBrowser::new(),
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
            session_view: None,
            switch_sel: 0,
            upgrade_menu: Some(0),
            upgrade_file: None,
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

        // The dashboard gives the log pane its own full-width block.
        ui.tab = Tab::Dashboard;
        let dash = render_ui(&mut ui);
        assert!(dash.contains("LOGS (5)"), "dashboard has no log block");
        let log_row = dash
            .lines()
            .find(|l| l.contains("LOGS (5)"))
            .expect("log block");
        // Full width means the block's border reaches the right edge.
        assert!(
            log_row.trim_end().chars().count() >= 118,
            "log block is not full width"
        );
        // Credentials are the fixed defaults.
        assert!(dash.contains("cisco / cisco123"), "unexpected credentials");
        // The address generated URLs use is named, with where it came from,
        // and the interface it belongs to is marked in the list.
        assert!(dash.contains("address in URLs"), "no advertised address");
        if let (Some(ip), _) = super::super::advertised_now(&ui.app.config.read().unwrap()) {
            assert!(dash.contains(&ip.to_string()), "address not shown");
            let marked = dash
                .lines()
                .find(|l| l.contains('→') && l.contains(&ip.to_string()));
            assert!(marked.is_some(), "advertised interface not marked");
        }

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
        for field in ["username", "password", "enable password", "protocol"] {
            assert!(screen.contains(field), "long IP list hides {field}");
        }
        assert_eq!(ui.deploy.as_ref().unwrap().form.host.lines().count(), 199);
    }

    #[test]
    fn bulk_scan_toggle_exposes_cidr_validation_before_starting_any_connection() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        super::super::activate_upgrade_action(&mut ui, 2);
        let form = &mut ui.deploy.as_mut().unwrap().form;
        form.field = 5;
        form.username = "netadmin".into();
        form.password = "secret".into();
        form.host = "192.168.10.0/99".into();
        super::super::handle_modal_key(
            &mut ui,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(' '),
                crossterm::event::KeyModifiers::NONE,
            ),
        );
        assert!(ui.deploy.as_ref().unwrap().form.scan_subnet);
        let screen = render_ui(&mut ui);
        assert!(screen.contains("IPv4 subnet / CIDR"));
        assert!(screen.contains("Scan subnet"));
        assert!(screen.contains("changed SSH host keys"));
        super::super::add_devices(&mut ui);
        assert!(ui.deploy.as_ref().unwrap().error.is_some());
        assert!(ui.app.switches.scan().is_none());
        assert!(ui.app.switches.list().is_empty());
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
    fn overview_shows_device_facts_and_recalculates_file_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Upgrade;
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
            "stack",
            "flash usage",
            "C9200L-48P-4X",
            "17.15.03",
            "1305 MB free of 1957 MB",
            "33% used",
            "NO SPACE",
        ] {
            assert!(screen.contains(label), "missing {label}: {screen}");
        }
        ui.upgrade_file = Some(("image.bin".into(), 1_304_999_999));
        let screen = render_ui(&mut ui);
        assert!(!screen.contains("NO SPACE"));
        assert!(screen.contains("yes"));
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
            if action == 2 {
                super::super::handle_modal_key(&mut ui, enter);
                assert!(!ui.deploy.as_ref().unwrap().form.scan_subnet);
            }
            super::super::handle_modal_key(&mut ui, enter);
            assert!(matches!(ui.modal, Some(Modal::DeployProtocol { .. })));
            super::super::handle_modal_key(&mut ui, enter);
            assert_eq!(
                ui.deploy.as_ref().unwrap().form.field,
                if action == 2 { 7 } else { 6 }
            );
            assert!(render_ui(&mut ui).contains("[ Enter ]"));
            assert!(ui.app.switches.list().is_empty());
            super::super::handle_modal_key(&mut ui, enter);
            assert_eq!(ui.app.switches.list().len(), 1);
            assert!(ui.modal.is_none());
            let switch = ui.app.switches.list()[0].clone();
            assert!(switch.transfer(&ui.app.sessions).is_none());
            switch.cancel();
        }
    }

    #[test]
    fn upgrade_selected_file_is_above_devices_on_the_right() {
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
            .position(|row| row.contains(" DEVICES "))
            .unwrap();
        assert!(file_row < device_row);
        let column = rows[file_row].find("images/selected-image.bin").unwrap();
        assert!(rows[file_row][..column].chars().count() >= 23);
        assert!(rows[file_row + 1].contains("50.0 MB"));
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
            assert_eq!(device.protocol(), crate::session::Protocol::Sftp);
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

        let mut form = super::super::DeployForm::default();
        form.host = "10.20.30.40".into();
        form.username = "netadmin".into();
        form.password = "letmein".into();
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
        assert!(!screen.contains("letmein"), "password shown in clear");
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
        ui.tab = Tab::Upgrade;

        // Empty: the view explains how to get a session.
        let screen = render_ui(&mut ui);
        assert!(screen.contains("DEVICES"), "no table");
        assert!(screen.contains("Add device"), "no hint for the empty view");

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
                assert!(screen.contains(desc), "missing help entry {key} — {desc}");
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
        let left_b = col_of("cycle sort order").unwrap();
        assert_eq!(left_a, left_b, "left table not aligned");
        let right_a = col_of("dashboard").unwrap();
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
}
