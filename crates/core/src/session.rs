use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Protocol {
    Ftp,
    Http,
    Https,
    Scp,
    Sftp,
    Tftp,
}

impl Protocol {
    pub fn label(self) -> &'static str {
        match self {
            Protocol::Ftp => "ftp",
            Protocol::Http => "http",
            Protocol::Https => "https",
            Protocol::Scp => "scp",
            Protocol::Sftp => "sftp",
            Protocol::Tftp => "tftp",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Download,
    Upload,
}

impl Direction {
    pub fn label(self) -> &'static str {
        match self {
            Direction::Download => "download",
            Direction::Upload => "upload",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Connected,
    Authenticating,
    Transferring,
    Completed,
    Failed(String),
    Aborted,
}

impl SessionState {
    pub fn label(&self) -> &'static str {
        match self {
            SessionState::Connected => "connected",
            SessionState::Authenticating => "auth",
            SessionState::Transferring => "transfer",
            SessionState::Completed => "completed",
            SessionState::Failed(_) => "failed",
            SessionState::Aborted => "aborted",
        }
    }
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            SessionState::Connected | SessionState::Authenticating | SessionState::Transferring
        )
    }
}

/// Shared, atomically updated part of a session; protocol adapters keep an
/// `Arc<SessionHandle>` and bump `bytes` from their transfer loops.
pub struct SessionHandle {
    pub id: u64,
    pub bytes: AtomicU64,
    last_byte: Mutex<Option<Instant>>,
    cancelled: std::sync::atomic::AtomicBool,
    cancellation: tokio::sync::Notify,
}

impl SessionHandle {
    pub fn check_active(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Acquire) {
            Err("transfer cancelled".into())
        } else {
            Ok(())
        }
    }
    pub async fn cancelled(&self) {
        loop {
            let notified = self.cancellation.notified();
            if self.cancelled.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
    pub fn add_bytes(&self, n: u64) {
        self.bytes.fetch_add(n, Ordering::Relaxed);
        *self.last_byte.lock().unwrap() = Some(Instant::now());
    }
}

#[derive(Clone)]
pub struct SessionInfo {
    pub id: u64,
    pub protocol: Protocol,
    pub peer: SocketAddr,
    pub local_port: u16,
    pub username: Option<String>,
    pub file: Option<String>,
    pub direction: Option<Direction>,
    pub state: SessionState,
    pub bytes: u64,
    pub total: Option<u64>,
    pub started_wall: SystemTime,
    pub started: Instant,
    pub ended: Option<Instant>,
    /// Transfer timing is independent of a persistent FTP/SSH connection.
    pub transfer_started: Option<Instant>,
    pub transfer_ended: Option<Instant>,
    /// Bytes/sec over the last sampling window.
    pub current_speed: f64,
}

impl SessionInfo {
    pub fn duration(&self) -> Duration {
        self.transfer_ended
            .or(self.ended)
            .unwrap_or_else(Instant::now)
            .saturating_duration_since(self.transfer_started.unwrap_or(self.started))
    }
    pub fn avg_speed(&self) -> f64 {
        let secs = self.duration().as_secs_f64();
        if secs > 0.0 {
            self.bytes as f64 / secs
        } else {
            0.0
        }
    }
    pub fn progress(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.bytes as f64 / t as f64).min(1.0))
    }
    pub fn eta(&self) -> Option<Duration> {
        let total = self.total?;
        if !self.state.is_active() || self.current_speed <= 1.0 || self.bytes >= total {
            return None;
        }
        Some(Duration::from_secs_f64(
            (total - self.bytes) as f64 / self.current_speed,
        ))
    }
}

struct SessionRecord {
    info: SessionInfo,
    handle: Arc<SessionHandle>,
    last_sample: (Instant, u64),
}

#[derive(Default)]
pub struct Totals {
    pub completed_transfers: u64,
    pub total_bytes: u64,
}

pub struct SessionManager {
    next_id: AtomicU64,
    records: Mutex<HashMap<u64, SessionRecord>>,
    totals: Mutex<Totals>,
    history_secs: AtomicU64,
    aborted_copies: Mutex<HashMap<(Protocol, std::net::IpAddr, String), Instant>>,
}

impl SessionManager {
    /// Interrupt only the data stream belonging to this copy, never other devices.
    pub fn abort_transfer(
        &self,
        protocol: Protocol,
        peer: Option<std::net::IpAddr>,
        path: &str,
        started: Instant,
    ) {
        if let Some(peer) = peer {
            self.aborted_copies.lock().unwrap().insert(
                (protocol, peer, path.trim_start_matches('/').into()),
                Instant::now(),
            );
        }
        for record in self.records.lock().unwrap().values_mut() {
            if record.info.protocol == protocol
                && Some(record.info.peer.ip()) == peer
                && record
                    .info
                    .file
                    .as_deref()
                    .map(|s| s.trim_start_matches('/'))
                    == Some(path.trim_start_matches('/'))
                && record.info.transfer_started.unwrap_or(record.info.started) >= started
            {
                record.handle.cancelled.store(true, Ordering::Release);
                record.handle.cancellation.notify_waiters();
                record.info.state = SessionState::Aborted;
            }
        }
    }
    pub fn allow_transfer(&self, protocol: Protocol, peer: Option<std::net::IpAddr>, path: &str) {
        if let Some(peer) = peer {
            self.aborted_copies.lock().unwrap().remove(&(
                protocol,
                peer,
                path.trim_start_matches('/').into(),
            ));
        }
    }
    pub fn new(history_secs: u64) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            records: Mutex::new(HashMap::new()),
            totals: Mutex::new(Totals::default()),
            history_secs: AtomicU64::new(history_secs.max(5)),
            aborted_copies: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_history_secs(&self, secs: u64) {
        self.history_secs.store(secs.max(5), Ordering::Relaxed);
    }

    /// Periodically sample per-session byte counters (for current speed) and
    /// drop finished sessions that fell out of the history window.
    pub fn start_metrics_task(self: &Arc<Self>, handle: &tokio::runtime::Handle) {
        let mgr = self.clone();
        handle.spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_millis(500));
            loop {
                tick.tick().await;
                mgr.sample();
            }
        });
    }

    pub fn sample(&self) {
        let now = Instant::now();
        let history = Duration::from_secs(self.history_secs.load(Ordering::Relaxed));
        let mut records = self.records.lock().unwrap();
        records.retain(|_, r| match r.info.ended {
            Some(t) => now.duration_since(t) < history,
            None => true,
        });
        for r in records.values_mut() {
            let bytes = r.handle.bytes.load(Ordering::Relaxed);
            if r.info.transfer_ended.is_none() && r.info.total.is_some_and(|t| bytes >= t) {
                r.info.transfer_ended = *r.handle.last_byte.lock().unwrap();
            }
            let (t0, b0) = r.last_sample;
            let dt = now.duration_since(t0).as_secs_f64();
            if dt > 0.05 {
                r.info.current_speed = if r.info.transfer_ended.is_some() || r.info.ended.is_some()
                {
                    0.0
                } else {
                    (bytes.saturating_sub(b0)) as f64 / dt
                };
                r.last_sample = (now, bytes);
            }
            r.info.bytes = bytes;
            if r.info.transfer_ended.is_some() || r.info.ended.is_some() {
                r.info.current_speed = 0.0;
            }
        }
    }

    pub fn open(
        &self,
        protocol: Protocol,
        peer: SocketAddr,
        local_port: u16,
    ) -> Arc<SessionHandle> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let handle = Arc::new(SessionHandle {
            id,
            bytes: AtomicU64::new(0),
            last_byte: Mutex::new(None),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            cancellation: tokio::sync::Notify::new(),
        });
        let info = SessionInfo {
            id,
            protocol,
            peer,
            local_port,
            username: None,
            file: None,
            direction: None,
            state: SessionState::Connected,
            bytes: 0,
            total: None,
            started_wall: SystemTime::now(),
            started: Instant::now(),
            ended: None,
            transfer_started: None,
            transfer_ended: None,
            current_speed: 0.0,
        };
        self.records.lock().unwrap().insert(
            id,
            SessionRecord {
                info,
                handle: handle.clone(),
                last_sample: (Instant::now(), 0),
            },
        );
        handle
    }

    pub fn update<F: FnOnce(&mut SessionInfo)>(&self, id: u64, f: F) {
        if let Some(r) = self.records.lock().unwrap().get_mut(&id) {
            let was_transfer = r.info.state == SessionState::Transferring;
            f(&mut r.info);
            if !was_transfer && r.info.state == SessionState::Transferring {
                let blocked = r.info.file.as_ref().is_some_and(|file| {
                    self.aborted_copies
                        .lock()
                        .unwrap()
                        .get(&(
                            r.info.protocol,
                            r.info.peer.ip(),
                            file.trim_start_matches('/').into(),
                        ))
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(60))
                });
                r.handle.cancelled.store(blocked, Ordering::Release);
                if blocked {
                    r.handle.cancellation.notify_waiters();
                }
                let now = Instant::now();
                r.handle.bytes.store(0, Ordering::Relaxed);
                *r.handle.last_byte.lock().unwrap() = None;
                r.info.bytes = 0;
                r.info.current_speed = 0.0;
                r.info.ended = None;
                r.info.transfer_started = Some(now);
                r.info.transfer_ended = None;
                r.last_sample = (now, 0);
            } else if was_transfer && r.info.state != SessionState::Transferring {
                // Adapters used to return to Connected after a copy, leaving
                // the transfer's duration growing while the connection idled.
                if r.info.state == SessionState::Connected {
                    r.info.state = SessionState::Completed;
                }
                self.finish_record(r);
            }
        }
    }

    fn finish_record(&self, r: &mut SessionRecord) {
        r.info.bytes = r.handle.bytes.load(Ordering::Relaxed);
        r.info.transfer_ended = r
            .info
            .transfer_ended
            .or(*r.handle.last_byte.lock().unwrap())
            .or(Some(Instant::now()));
        r.info.current_speed = 0.0;
        let mut totals = self.totals.lock().unwrap();
        totals.total_bytes += r.info.bytes;
        if r.info.state == SessionState::Completed && r.info.file.is_some() {
            totals.completed_transfers += 1;
        }
    }

    pub fn finish(&self, id: u64, state: SessionState) {
        self.update(id, |s| s.state = state);
    }

    /// Close the connection without changing an already completed transfer.
    pub fn close(&self, id: u64, state: SessionState) {
        let mut records = self.records.lock().unwrap();
        if let Some(r) = records.get_mut(&id) {
            if r.info.ended.is_none() {
                if r.info.state == SessionState::Transferring || r.info.transfer_started.is_none() {
                    r.info.state = state;
                    self.finish_record(r);
                }
                r.info.ended = Some(Instant::now());
                r.info.current_speed = 0.0;
            }
        }
    }

    pub fn active_count(&self) -> usize {
        self.records
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.info.state.is_active())
            .count()
    }

    pub fn active_count_for(&self, protocol: Protocol) -> usize {
        self.records
            .lock()
            .unwrap()
            .values()
            .filter(|r| r.info.state.is_active() && r.info.protocol == protocol)
            .count()
    }

    pub fn totals(&self) -> (u64, u64) {
        let t = self.totals.lock().unwrap();
        (t.completed_transfers, t.total_bytes)
    }

    /// All sessions, active first, then most recent.
    pub fn snapshot(&self) -> Vec<SessionInfo> {
        let records = self.records.lock().unwrap();
        let mut list: Vec<SessionInfo> = records.values().map(|r| r.info.clone()).collect();
        list.sort_by(|a, b| {
            b.state
                .is_active()
                .cmp(&a.state.is_active())
                .then_with(|| b.started_wall.cmp(&a.started_wall))
        });
        list
    }
}

/// Format a byte count ("14.2 MB").
pub fn fmt_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

/// Format a speed, either in bit/s (network convention) or bytes/s.
pub fn fmt_speed(bytes_per_sec: f64, bits: bool) -> String {
    if bits {
        let mut v = bytes_per_sec * 8.0;
        for unit in ["bit/s", "kbit/s", "Mbit/s", "Gbit/s"] {
            if v < 1000.0 {
                return format!("{v:.1} {unit}");
            }
            v /= 1000.0;
        }
        format!("{v:.1} Tbit/s")
    } else {
        let mut v = bytes_per_sec;
        for unit in ["B/s", "KB/s", "MB/s", "GB/s"] {
            if v < 1000.0 {
                return format!("{v:.1} {unit}");
            }
            v /= 1000.0;
        }
        format!("{v:.1} TB/s")
    }
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_formatting() {
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(1500), "1.5 KB");
        assert_eq!(fmt_bytes(5_242_880), "5.2 MB");
    }

    #[test]
    fn speed_formatting() {
        assert_eq!(fmt_speed(125.0, true), "1.0 kbit/s");
        assert_eq!(fmt_speed(125_000_000.0, true), "1.0 Gbit/s");
        assert_eq!(fmt_speed(2048.0, false), "2.0 KB/s");
        assert_eq!(fmt_speed(10.0, true), "80.0 bit/s");
    }

    #[test]
    fn duration_formatting() {
        assert_eq!(fmt_duration(Duration::from_secs(59)), "0:59");
        assert_eq!(fmt_duration(Duration::from_secs(61)), "1:01");
        assert_eq!(fmt_duration(Duration::from_secs(3723)), "1:02:03");
    }

    fn peer() -> SocketAddr {
        "192.0.2.1:50000".parse().unwrap()
    }

    #[test]
    fn session_lifecycle_and_totals() {
        let mgr = SessionManager::new(600);
        let h = mgr.open(Protocol::Http, peer(), 8080);
        mgr.update(h.id, |s| {
            s.file = Some("image.bin".into());
            s.direction = Some(Direction::Download);
            s.total = Some(1000);
            s.state = SessionState::Transferring;
        });
        h.add_bytes(400);
        mgr.sample();
        let snap = mgr.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].bytes, 400);
        assert!((snap[0].progress().unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(mgr.active_count(), 1);

        h.add_bytes(600);
        mgr.close(h.id, SessionState::Completed);
        assert_eq!(mgr.active_count(), 0);
        let (completed, bytes) = mgr.totals();
        assert_eq!(completed, 1);
        assert_eq!(bytes, 1000);
    }

    #[test]
    fn completed_transfer_stops_timing_even_with_connection_open() {
        let mgr = SessionManager::new(600);
        let h = mgr.open(Protocol::Ftp, peer(), 2121);
        // A connection can be idle for a long time before its first RETR.
        mgr.update(h.id, |s| {
            s.started = Instant::now() - Duration::from_secs(60)
        });
        mgr.update(h.id, |s| {
            s.file = Some("one.bin".into());
            s.total = Some(1000);
            s.state = SessionState::Transferring;
        });
        std::thread::sleep(Duration::from_millis(10));
        h.add_bytes(1000);
        mgr.sample();
        let completed = mgr.snapshot()[0].clone();
        assert_eq!(completed.progress(), Some(1.0));
        assert!(completed.duration() < Duration::from_secs(1));
        std::thread::sleep(Duration::from_millis(60));
        mgr.sample();
        let idle = mgr.snapshot()[0].clone();
        assert_eq!(idle.duration(), completed.duration());
        assert_eq!(idle.avg_speed(), completed.avg_speed());
        assert_eq!(idle.current_speed, 0.0);
        assert_eq!(idle.eta(), None);
        // Finishing the data transfer must not wait for FTP QUIT / SSH close.
        mgr.finish(h.id, SessionState::Completed);
        assert_eq!(mgr.active_count(), 0);
        assert_eq!(mgr.totals(), (1, 1000));
        mgr.close(h.id, SessionState::Completed);
        assert_eq!(mgr.totals(), (1, 1000));
    }

    #[test]
    fn persistent_connection_resets_metrics_for_each_transfer() {
        let mgr = SessionManager::new(600);
        let h = mgr.open(Protocol::Sftp, peer(), 2222);
        for (file, bytes) in [("one.bin", 1000), ("two.bin", 2000)] {
            mgr.update(h.id, |s| {
                s.file = Some(file.into());
                s.total = Some(bytes);
                s.state = SessionState::Transferring;
            });
            assert_eq!(mgr.snapshot()[0].bytes, 0);
            assert_eq!(h.bytes.load(Ordering::Relaxed), 0);
            assert_eq!(mgr.snapshot()[0].transfer_ended, None);
            h.add_bytes(bytes);
            mgr.finish(h.id, SessionState::Completed);
            assert_eq!(mgr.snapshot()[0].bytes, bytes);
            assert_eq!(mgr.snapshot()[0].progress(), Some(1.0));
        }
        mgr.close(h.id, SessionState::Completed);
        assert_eq!(mgr.totals(), (2, 3000));
    }

    #[test]
    fn failed_sessions_do_not_count_as_completed() {
        let mgr = SessionManager::new(600);
        let h = mgr.open(Protocol::Ftp, peer(), 2121);
        mgr.update(h.id, |s| s.file = Some("x".into()));
        h.add_bytes(10);
        mgr.close(h.id, SessionState::Failed("client aborted".into()));
        let (completed, bytes) = mgr.totals();
        assert_eq!(completed, 0);
        assert_eq!(bytes, 10);
    }

    #[test]
    fn abort_is_scoped_to_the_copy_and_blocks_late_streams_until_explicit_retry() {
        let mgr = SessionManager::new(60);
        let started = Instant::now();
        let first = mgr.open(Protocol::Http, "192.0.2.1:50001".parse().unwrap(), 8080);
        let other = mgr.open(Protocol::Http, "192.0.2.2:50002".parse().unwrap(), 8080);
        for handle in [&first, &other] {
            mgr.update(handle.id, |s| {
                s.file = Some("image.bin".into());
                s.state = SessionState::Transferring;
            });
        }
        mgr.abort_transfer(
            Protocol::Http,
            Some("192.0.2.1".parse().unwrap()),
            "image.bin",
            started,
        );
        assert!(first.check_active().is_err());
        assert!(other.check_active().is_ok());
        let late = mgr.open(Protocol::Http, "192.0.2.1:50003".parse().unwrap(), 8080);
        mgr.update(late.id, |s| {
            s.file = Some("image.bin".into());
            s.state = SessionState::Transferring;
        });
        assert!(late.check_active().is_err());
        mgr.allow_transfer(
            Protocol::Http,
            Some("192.0.2.1".parse().unwrap()),
            "image.bin",
        );
        let retry = mgr.open(Protocol::Http, "192.0.2.1:50004".parse().unwrap(), 8080);
        mgr.update(retry.id, |s| {
            s.file = Some("image.bin".into());
            s.state = SessionState::Transferring;
        });
        assert!(retry.check_active().is_ok());
        assert!(other.check_active().is_ok());
    }

    #[test]
    fn parallel_sessions() {
        let mgr = SessionManager::new(600);
        let handles: Vec<_> = (0..8)
            .map(|_| mgr.open(Protocol::Tftp, peer(), 6969))
            .collect();
        assert_eq!(mgr.active_count(), 8);
        assert_eq!(mgr.active_count_for(Protocol::Tftp), 8);
        assert_eq!(mgr.active_count_for(Protocol::Http), 0);
        for h in &handles {
            mgr.close(h.id, SessionState::Completed);
        }
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn speed_calculation_from_samples() {
        let mgr = SessionManager::new(600);
        let h = mgr.open(Protocol::Http, peer(), 8080);
        mgr.sample();
        std::thread::sleep(Duration::from_millis(120));
        h.add_bytes(100_000);
        mgr.sample();
        let snap = mgr.snapshot();
        // ~100 KB in ~0.12 s => several hundred KB/s; just require a sane range.
        assert!(
            snap[0].current_speed > 100_000.0,
            "speed was {}",
            snap[0].current_speed
        );
    }
}
