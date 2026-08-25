use chrono::{DateTime, Local};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, Tabs, Wrap};

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
        Tab::Switches => draw_switches(f, ui, chunks[1]),
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
                Span::styled(format!(" {}", name.trim()), Style::default().fg(theme::TEXT)),
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
        Tab::Switches => vec![
            ("Enter", "session"),
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
        ServiceStatus::Stopped => Style::default().fg(theme::DIM),
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
    let suggested = crate::netif::suggest_ip()
        .map(|i| i.to_string())
        .unwrap_or_else(|| "-".into());
    let summary = vec![
        Line::from(vec![
            theme::label("root:              "),
            theme::value(cfg.root.display().to_string()),
        ]),
        Line::from(vec![
            theme::label("suggested address: "),
            Span::styled(suggested, Style::default().fg(theme::HILITE).bold()),
            theme::label("   privileged: "),
            theme::value(if ui.app.privileged { "yes (sudo)" } else { "no" }),
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
            theme::value(format!("{completed} completed, {} served", fmt_bytes(bytes))),
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
        Paragraph::new(summary)
            .block(theme::panel_double(&format!(" TRANSFERBUDDY v{} ", crate::VERSION))),
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
                    ui.app.sessions.active_count_for(crate::session::Protocol::Sftp)
                        + ui.app.sessions.active_count_for(crate::session::Protocol::Scp)
                }
                ServiceId::Http => ui.app.sessions.active_count_for(crate::session::Protocol::Http),
                ServiceId::Https => ui.app.sessions.active_count_for(crate::session::Protocol::Https),
                ServiceId::Ftp => ui.app.sessions.active_count_for(crate::session::Protocol::Ftp),
                ServiceId::Tftp => ui.app.sessions.active_count_for(crate::session::Protocol::Tftp),
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
                Cell::from(Span::styled(status.label().to_string(), status_style(&status))),
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
        Row::new(vec!["service", "status", "on", "port", "bind", "sess", "uptime", "crypto", "error"])
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
        Line::from(vec![theme::label("size:  "), theme::value(fmt_bytes(total))]),
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
        Line::from(vec![theme::label("now:   "), theme::value(format!("{active} active"))]),
        Line::from(vec![
            theme::label("total: "),
            theme::value(format!("{completed} done, {}", fmt_bytes(bytes))),
        ]),
    ];
    if snap.is_empty() {
        sess_lines.push(Line::from(theme::label("  no sessions yet")));
    } else {
        for s in snap.iter().rev().take(4) {
            let color = if s.state.is_active() { theme::HILITE } else { theme::DIM };
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

    let mut if_lines = Vec::new();
    for ifa in crate::netif::interfaces().into_iter().take(6) {
        if_lines.push(Line::from(vec![
            Span::styled(
                format!("{:<7} {}", ifa.name, ifa.ip),
                Style::default().fg(theme::TEXT),
            ),
            theme::label(format!("  {}", ifa.kind.label())),
        ]));
    }
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
    .header(
        Row::new(vec!["on", "service", "status", "listen", "note"]).style(theme::header()),
    )
    .block(theme::panel(" SERVICES — Space on/off, s start/stop, Enter edit "));
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
                    if cfg.uploads.dir.is_empty() { "<root>".into() } else { cfg.uploads.dir.clone() },
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
        lines.push(Line::from(vec![theme::label("certificate: "), theme::value(info)]));
    }
    if id == ServiceId::Ssh {
        let fp = ssh_info(&cfg);
        lines.push(Line::from(vec![theme::label("host key: "), theme::value(fp)]));
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
            .block(theme::panel(&format!(" {} ", id.display_name().to_uppercase()))),
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
        Some(fp) => format!("{} (SHA256:{fp})", crate::sshkeys::host_key_path(cfg).display()),
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
                .map(|t| DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            let rel = if ui.files.cwd.is_empty() {
                format!("/{}", e.name)
            } else {
                format!("/{}/{}", ui.files.cwd, e.name)
            };
            Row::new(vec![
                Cell::from(format!("{}{}", e.name, if e.is_dir { "/" } else { "" })),
                Cell::from(if e.is_dir { "-".into() } else { fmt_bytes(e.size) }),
                Cell::from(modified),
                Cell::from(if e.is_dir { "dir".into() } else { e.ext.clone() }),
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
                Cell::from(Span::styled(s.protocol.label(), Style::default().fg(theme::CYAN))),
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
        Row::new(vec!["id", "proto", "source", "user", "file", "dir", "state", "prog", "speed"])
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
        let started = DateTime::<Local>::from(s.started_wall).format("%H:%M:%S").to_string();
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
    let max_scroll = filtered.len().saturating_sub(height);
    if ui.log_follow || ui.log_scroll > max_scroll {
        ui.log_scroll = max_scroll;
    }
    let lines: Vec<Line> = filtered
        .iter()
        .skip(ui.log_scroll)
        .take(height.max(1))
        .map(|e| {
            Line::from(Span::styled(
                e.render_line(),
                Style::default().fg(level_color(e.level)),
            ))
        })
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
    f.render_widget(
        Paragraph::new(lines).block(theme::panel(&title)),
        area,
    );
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
            ("6", "switches"),
            ("← →", "previous / next view"),
            ("Tab", "next view"),
            ("h", "this help"),
            ("m", "sound on / off"),
            ("q", "quit"),
            ("^C", "quit"),
        ],
    ),
    (
        "DEPLOY (d) + SWITCHES (6)",
        &[
            ("↑ ↓", "form field / session"),
            ("← →", "protocol / toggle"),
            ("Enter", "start the copy / open"),
            ("r", "re-read dir + version"),
            ("x", "disconnect"),
            ("X", "clear closed rows"),
            ("c", "cancel a running job"),
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

    let area = centered_rect(80, rows as u16 + 3, f.area());
    f.render_widget(Clear, area);
    let block = theme::panel_double(" HELP — one key per line ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Scroll when the terminal is too short for the full tables.
    let visible = inner.height.saturating_sub(1) as usize;
    let max_scroll = rows.saturating_sub(visible);
    let scroll = help_scroll.min(max_scroll);
    let take = |lines: Vec<Line<'static>>| -> Vec<Line<'static>> {
        lines.into_iter().skip(scroll).take(visible.max(1)).collect()
    };

    let cols = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1)));
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
        .min(DeployField::ALL.len() as u16 + 10);
    let area = centered_rect(width, height, f.area());
    f.render_widget(Clear, area);
    let block = theme::panel_double(&format!(" DEPLOY  /{} ", view.rel_path));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cfg = ui.app.config.read().unwrap();
    let open_session = ui.app.switches.find_live(
        view.form.host.trim(),
        view.form.port.trim().parse().unwrap_or(0),
        view.form.username.trim(),
    );
    let mut lines = Vec::new();
    for (i, field) in DeployField::ALL.iter().enumerate() {
        let value = match field {
            DeployField::Host => view.form.host.clone(),
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
                if view.form.overwrite { "yes".into() } else { "no".into() }
            }
            DeployField::Protocol => {
                let id = crate::cisco::service_of(view.form.proto);
                let status = ui.app.services.status(id);
                format!(
                    "{}  ({})",
                    view.form.proto.label(),
                    if status.is_running() {
                        "running".to_string()
                    } else if cfg.service(id).enabled {
                        "enabled, not started".to_string()
                    } else {
                        "off".to_string()
                    }
                )
            }
        };
        let selected = i == view.form.field;
        let shown = if selected && field.is_text() {
            format!("{value}█")
        } else {
            value
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<18}", field.label()),
                Style::default().fg(theme::ACCENT),
            ),
            Span::styled(
                shown,
                if selected { theme::selected_strong() } else { Style::default().fg(theme::TEXT) },
            ),
        ]));
    }
    drop(cfg);

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
        theme::label(" deploy  "),
        theme::key("Esc"),
        theme::label(" close"),
    ]));
    lines.push(Line::from(theme::label(
        " transferbuddy logs in, runs exactly this one copy and leaves — never a config command.",
    )));
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

    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(inner);

    f.render_widget(Paragraph::new(switch_summary(switch)).wrap(Wrap { trim: false }), chunks[0]);

    // Transcript, tail-following unless the user scrolled up.
    let rows = chunks[1].height.saturating_sub(2) as usize;
    let transcript = switch.transcript();
    let live = switch.live();
    let mut body: Vec<(LineKind, &str)> =
        transcript.iter().map(|l| (l.kind, l.text.as_str())).collect();
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
    for (key, desc) in [("r", " refresh   "), ("x", " disconnect   "), ("↑↓", " scroll   ")] {
        hint.push(theme::key(key));
        hint.push(theme::label(desc));
    }
    hint.push(theme::key("Esc"));
    hint.push(theme::label(" close"));
    f.render_widget(Paragraph::new(Line::from(hint)), chunks[2]);
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
                Style::default().fg(if usage.used_fraction() > 0.9 {
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
                    .fg(if odd.is_empty() { theme::TEXT } else { theme::WARN })
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
            Span::styled(fingerprint.to_string(), Style::default().fg(theme::CYAN).bold()),
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
    let switches = ui.app.switches.list();
    if ui.switch_sel >= switches.len() {
        ui.switch_sel = switches.len().saturating_sub(1);
    }

    let header = Row::new(vec![
        Cell::from("name"),
        Cell::from("host"),
        Cell::from("user"),
        Cell::from("state"),
        Cell::from("IOS-XE"),
        Cell::from("model"),
        Cell::from("flash free"),
        Cell::from("ping"),
        Cell::from("open"),
    ])
    .style(theme::header());

    let rows: Vec<Row> = switches
        .iter()
        .enumerate()
        .map(|(i, sw)| {
            let state = sw.state();
            let facts = sw.facts();
            let version = facts.version.clone().unwrap_or_default();
            let state_style = match &state {
                SwitchState::Ready => Style::default().fg(theme::OK),
                SwitchState::Busy { .. } => Style::default().fg(theme::HILITE).bold(),
                SwitchState::Failed { .. } | SwitchState::Offline { .. } => {
                    Style::default().fg(theme::ERR)
                }
                _ => Style::default().fg(theme::WARN),
            };
            let flash = match facts.flash {
                Some(u) => format!(
                    "{} ({:.0}%)",
                    crate::switch::fmt_mb(u.free),
                    u.used_fraction() * 100.0
                ),
                None => "—".into(),
            };
            let row = Row::new(vec![
                Cell::from(sw.display_name()),
                Cell::from(format!("{}:{}", sw.host, sw.port)),
                Cell::from(sw.username.clone()),
                Cell::from(state.label()).style(state_style),
                Cell::from(version.version.clone().unwrap_or_else(|| "—".into())),
                Cell::from(version.model.clone().unwrap_or_else(|| "—".into())),
                Cell::from(flash),
                Cell::from(Line::from(ping_span(&sw.reach()))),
                Cell::from(fmt_duration(sw.opened.elapsed())),
            ]);
            if i == ui.switch_sel {
                row.style(theme::selected())
            } else {
                row
            }
        })
        .collect();

    let chunks = Layout::vertical([Constraint::Min(3), Constraint::Length(7)]).split(area);
    let widths = [
        Constraint::Length(20),
        Constraint::Length(22),
        Constraint::Length(12),
        Constraint::Length(10),
        Constraint::Length(10),
        Constraint::Length(16),
        Constraint::Length(18),
        Constraint::Length(10),
        Constraint::Min(6),
    ];
    f.render_widget(
        Table::new(rows, widths)
            .header(header)
            .block(theme::panel(" SWITCH SESSIONS ")),
        chunks[0],
    );

    let detail = match switches.get(ui.switch_sel) {
        Some(sw) => {
            let mut lines = switch_summary(sw);
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw(" "),
                theme::key("Enter"),
                theme::label(" open the session   "),
                theme::key("r"),
                theme::label(" refresh facts   "),
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
                "  No session yet. Open one from the files view: select a file and press d.",
            )),
            Line::from(theme::label(
                "  A session stays open after the copy — it is what tracks the install later.",
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
        Modal::Cisco { rel_path, commands, selected, copied } => {
            let mut lines = vec![Line::from(vec![
                theme::label("copy commands for "),
                Span::styled(format!("/{rel_path}"), Style::default().fg(theme::CYAN).bold()),
                theme::label("   (↑↓ select, y/Enter copy, Esc close)"),
            ])];
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
            let width = (f.area().width).min(100);
            let area = centered_rect(width, commands.len() as u16 + 4, f.area());
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
            let running = ui.app.services.status(*id).is_running();
            let sc = cfg.service(*id);
            let mut lines = vec![Line::from(theme::label(
                " ↑↓ select   Enter edit / toggle   Esc close",
            ))];
            for (i, fld) in EditField::ALL.iter().enumerate() {
                let value: String = match fld {
                    EditField::StartStop => if running { "running".into() } else { "stopped".into() },
                    EditField::Enabled => if sc.enabled { "on".into() } else { "off".into() },
                    EditField::Port => sc.port.to_string(),
                    EditField::Bind => sc.bind.clone(),
                    EditField::Username => cfg.auth.username.clone(),
                    EditField::Password => cfg.auth.password.clone(),
                    EditField::Uploads => if cfg.uploads.enabled { "on".into() } else { "off".into() },
                    EditField::UploadDir => {
                        if cfg.uploads.dir.is_empty() { "<root>".into() } else { cfg.uploads.dir.clone() }
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
                let row_style = if selected {
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
                for (label, value) in
                    [("MD5    ", &info.md5), ("SHA-256", &info.sha256), ("SHA-512", &info.sha512)]
                {
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
                    lines.push(Line::from(Span::styled(txt, Style::default().fg(color).bold())));
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
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        let mut cfg = crate::config::Config::default();
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
            crate::switch::Line { kind: LineKind::Info, text: "connecting to 10.20.30.40:22".into() },
            crate::switch::Line { kind: LineKind::Sent, text: "terminal length 0".into() },
            crate::switch::Line { kind: LineKind::Output, text: "SG-AS-OG5-01#".into() },
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
            assert!(screen.contains(&format!("v{}", crate::VERSION)), "{tab:?}: no version");
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
        assert!(log_row.trim_end().chars().count() >= 118, "log block is not full width");
        // Credentials are the fixed defaults.
        assert!(dash.contains("cisco / cisco123"), "unexpected credentials");

        for modal in [
            Modal::Help,
            Modal::ConfirmQuit,
            Modal::ConfirmUploads,
            Modal::ServiceEdit { id: ServiceId::Http, field: 0, editing: None },
        ] {
            ui.modal = Some(modal);
            let screen = render_ui(&mut ui);
            assert!(screen.contains('╔'), "modal without a frame");
        }
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
        assert!(screen.contains("235 MB free of 1957 MB"), "flash not in MB: {screen}");
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

    #[test]
    fn switches_view_lists_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let (_rt, app) = test_app(dir.path().to_path_buf());
        let mut ui = test_ui(app);
        ui.tab = Tab::Switches;

        // Empty: the view explains how to get a session.
        let screen = render_ui(&mut ui);
        assert!(screen.contains("SWITCH SESSIONS"), "no table");
        assert!(screen.contains("press d"), "no hint for the empty view");

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
        assert!(screen.contains("members differ"), "half-upgraded stack not flagged");
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
