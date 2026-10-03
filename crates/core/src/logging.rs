use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

pub const LOG_BUFFER: usize = 5000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    pub fn label(self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warning => "WARNING",
            LogLevel::Error => "ERROR",
        }
    }
    pub const ALL: [LogLevel; 4] = [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warning,
        LogLevel::Error,
    ];
}

impl std::str::FromStr for LogLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" | "warning" => Ok(LogLevel::Warning),
            "error" => Ok(LogLevel::Error),
            other => Err(format!(
                "unknown log level: {other} (use debug|info|warning|error)"
            )),
        }
    }
}

/// One structured log entry. Optional fields stay empty when not applicable.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub time: DateTime<Local>,
    pub level: LogLevel,
    /// Protocol or subsystem ("http", "ftp", "core", ...).
    pub proto: String,
    pub hostname: Option<String>,
    pub model: Option<String>,
    pub command: Option<String>,
    pub source_ip: Option<std::net::IpAddr>,
    pub session_id: Option<u64>,
    pub action: String,
    pub path: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub bytes: Option<u64>,
    pub duration_ms: Option<u64>,
}

impl LogEntry {
    pub fn render_line(&self) -> String {
        let mut s = format!(
            "{} {:<7} [{:<5}]",
            self.time.format("%Y-%m-%d %H:%M:%S%.3f"),
            self.level.label(),
            self.proto
        );
        if let Some(id) = self.session_id {
            s.push_str(&format!(" #{id}"));
        }
        if let Some(ip) = self.source_ip {
            s.push_str(&format!(" {ip}"));
        }
        s.push(' ');
        s.push_str(&self.action);
        if let Some(host) = &self.hostname {
            s.push_str(&format!(" hostname={host}"));
        }
        if let Some(model) = &self.model {
            s.push_str(&format!(" model={model}"));
        }
        if let Some(command) = &self.command {
            s.push_str(&format!(" command={command}"));
        }
        if let Some(p) = &self.path {
            s.push_str(&format!(" path={p}"));
        }
        if let Some(r) = &self.result {
            s.push_str(&format!(" result={r}"));
        }
        if let Some(b) = self.bytes {
            s.push_str(&format!(
                " size={} ({b} bytes)",
                crate::session::fmt_bytes(b)
            ));
        }
        if let Some(d) = self.duration_ms {
            if d < 1000 {
                s.push_str(&format!(" duration={d}ms"));
            } else {
                s.push_str(&format!(" duration={:.2}s", d as f64 / 1000.0));
            }
            if let Some(b) = self.bytes.filter(|_| d > 0) {
                s.push_str(&format!(
                    " avg={}",
                    crate::session::fmt_speed(b as f64 * 1000.0 / d as f64, false)
                ));
            }
        }
        if let Some(e) = &self.error {
            s.push_str(&format!(" error=\"{e}\""));
        }
        s
    }
}

/// Builder-ish helper so call sites stay readable.
pub struct Event {
    entry: LogEntry,
}

impl Event {
    pub fn new(level: LogLevel, proto: &str, action: impl Into<String>) -> Self {
        Self {
            entry: LogEntry {
                time: Local::now(),
                level,
                proto: proto.to_string(),
                hostname: None,
                model: None,
                command: None,
                source_ip: None,
                session_id: None,
                action: action.into(),
                path: None,
                result: None,
                error: None,
                bytes: None,
                duration_ms: None,
            },
        }
    }
    pub fn ip(mut self, ip: std::net::IpAddr) -> Self {
        self.entry.source_ip = Some(ip);
        self
    }
    pub fn device(mut self, hostname: String, model: Option<String>) -> Self {
        self.entry.hostname = Some(hostname);
        self.entry.model = model;
        self
    }
    pub fn command(mut self, command: String) -> Self {
        self.entry.command = Some(command);
        self
    }
    pub fn session(mut self, id: u64) -> Self {
        self.entry.session_id = Some(id);
        self
    }
    pub fn path(mut self, p: impl Into<String>) -> Self {
        self.entry.path = Some(p.into());
        self
    }
    pub fn result(mut self, r: impl Into<String>) -> Self {
        self.entry.result = Some(r.into());
        self
    }
    pub fn error(mut self, e: impl Into<String>) -> Self {
        self.entry.error = Some(e.into());
        self
    }
    pub fn bytes(mut self, b: u64) -> Self {
        self.entry.bytes = Some(b);
        self
    }
    pub fn duration_ms(mut self, d: u64) -> Self {
        self.entry.duration_ms = Some(d);
        self
    }
}

pub struct Logger {
    revision: std::sync::atomic::AtomicU64,
    min_level: Mutex<LogLevel>,
    buffer: Mutex<VecDeque<LogEntry>>,
    tx: broadcast::Sender<LogEntry>,
    file: Mutex<Option<std::fs::File>>,
    file_path: Option<PathBuf>,
    stdout: bool,
}

impl Logger {
    pub fn new(min_level: LogLevel, file_path: Option<PathBuf>, stdout: bool) -> Self {
        let file = file_path.as_ref().and_then(|p| {
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
        });
        let (tx, _) = broadcast::channel(1024);
        Self {
            revision: std::sync::atomic::AtomicU64::new(0),
            min_level: Mutex::new(min_level),
            buffer: Mutex::new(VecDeque::with_capacity(LOG_BUFFER)),
            tx,
            file: Mutex::new(file),
            file_path,
            stdout,
        }
    }

    pub fn file_path(&self) -> Option<&PathBuf> {
        self.file_path.as_ref()
    }

    pub fn set_level(&self, level: LogLevel) {
        *self.min_level.lock().unwrap() = level;
    }

    pub fn level(&self) -> LogLevel {
        *self.min_level.lock().unwrap()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn log(&self, ev: Event) {
        let entry = ev.entry;
        if entry.level < self.level() {
            return;
        }
        if let Some(f) = self.file.lock().unwrap().as_mut() {
            let _ = writeln!(f, "{}", entry.render_line());
        }
        // In --no-tui mode the headless loop prints entries from the broadcast
        // channel; only fall back to direct printing if nobody listens.
        if self.stdout && self.tx.receiver_count() == 0 {
            println!("{}", entry.render_line());
        }
        {
            let mut buf = self.buffer.lock().unwrap();
            if buf.len() >= LOG_BUFFER {
                buf.pop_front();
            }
            buf.push_back(entry.clone());
        }
        self.revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        let _ = self.tx.send(entry);
    }

    pub fn log_simple(&self, level: LogLevel, proto: &str, action: impl Into<String>) {
        self.log(Event::new(level, proto, action));
    }

    /// Snapshot of the ring buffer for the TUI log view.
    pub fn entries(&self) -> Vec<LogEntry> {
        self.buffer.lock().unwrap().iter().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_log_has_readable_size_duration_and_average_speed() {
        let entry = Event::new(LogLevel::Info, "ftp", "download")
            .bytes(32_000_000)
            .duration_ms(2000)
            .entry;
        let line = entry.render_line();
        assert!(line.contains("size=32.0 MB (32000000 bytes)"));
        assert!(line.contains("duration=2.00s"));
        assert!(line.contains("avg=16.0 MB/s"));
        let line = Event::new(LogLevel::Info, "ftp", "download")
            .bytes(0)
            .duration_ms(0)
            .entry
            .render_line();
        assert!(!line.contains("avg="));
        assert!(!line.contains("NaN"));
    }
}
