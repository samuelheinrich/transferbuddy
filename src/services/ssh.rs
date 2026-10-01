//! SSH service providing both SFTP (subsystem) and SCP (exec) with shared
//! password authentication — one listener serves `copy scp://` and
//! `copy sftp://` from Cisco devices as well as OpenSSH clients.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::Instant;

use russh::server::{Auth, Handler, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, CryptoVec, MethodSet};
use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle as SftpHandle, Name, OpenFlags, Status, StatusCode,
    Version,
};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;

use crate::auth::AuthResult;
use crate::logging::{Event, LogLevel};
use crate::services::{ServiceCtx, UploadGuard};
use crate::session::{Direction, Protocol, SessionState};

const CHUNK: usize = 64 * 1024;

pub async fn run(ctx: ServiceCtx) -> Result<(), String> {
    let host_key = crate::sshkeys::load_or_generate(&ctx.cfg)
        .map_err(|e| format!("SSH host key error: {e:#}"))?;

    let config = Arc::new(russh::server::Config {
        methods: MethodSet::PASSWORD,
        auth_rejection_time: std::time::Duration::from_secs(1),
        auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
        keys: vec![host_key],
        inactivity_timeout: Some(std::time::Duration::from_secs(ctx.cfg.idle_timeout_secs.max(10))),
        ..Default::default()
    });

    let addr = ctx.bind_addr();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| crate::services::http::bind_error(addr, e))?;
    ctx.set_running();

    let mut shutdown = ctx.shutdown.clone();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        ctx.logger.log(Event::new(LogLevel::Warning, "ssh", "accept failed").error(e.to_string()));
                        continue;
                    }
                };
                if ctx.at_session_limit() {
                    ctx.logger.log(Event::new(LogLevel::Warning, "ssh", "session limit reached, rejecting").ip(peer.ip()));
                    continue;
                }
                let ctx = ctx.clone();
                let config = config.clone();
                tokio::spawn(async move {
                    let handle = ctx.sessions.open(Protocol::Sftp, peer, ctx.bind_addr().port());
                    let sid = handle.id;
                    let handler = SshConnection {
                        ctx: ctx.clone(),
                        peer,
                        sid,
                        handle,
                        channels: HashMap::new(),
                    };
                    match russh::server::run_stream(config, stream, handler).await {
                        Ok(running) => {
                            let _ = running.await;
                        }
                        Err(e) => {
                            ctx.logger.log(
                                Event::new(LogLevel::Debug, "ssh", "handshake failed")
                                    .ip(peer.ip())
                                    .session(sid)
                                    .error(e.to_string()),
                            );
                        }
                    }
                    // Mark still-open sessions as completed on disconnect.
                    ctx.sessions.close(sid, SessionState::Completed);
                });
            }
        }
    }
    Ok(())
}

struct SshConnection {
    ctx: ServiceCtx,
    peer: SocketAddr,
    sid: u64,
    handle: Arc<crate::session::SessionHandle>,
    channels: HashMap<ChannelId, Channel<Msg>>,
}

#[async_trait::async_trait]
impl Handler for SshConnection {
    type Error = russh::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        match self.ctx.auth.check(self.peer.ip(), user, password) {
            AuthResult::Ok => {
                let username = user.to_string();
                self.ctx.sessions.update(self.sid, |s| s.username = Some(username));
                self.ctx.logger.log(
                    Event::new(LogLevel::Info, "ssh", "login ok").ip(self.peer.ip()).session(self.sid),
                );
                Ok(Auth::Accept)
            }
            AuthResult::LockedOut => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ssh", "login locked out")
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
                Ok(Auth::Reject { proceed_with_methods: None })
            }
            AuthResult::BadCredentials => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ssh", "login failed")
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
                Ok(Auth::Reject { proceed_with_methods: None })
            }
        }
    }

    async fn channel_eof(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // The SFTP subsystem stream keeps the channel open on our side;
        // answer the client's EOF with a close so `sftp` exits cleanly.
        session.eof(channel);
        session.close(channel);
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        self.channels.insert(channel.id(), channel);
        Ok(true)
    }

    async fn subsystem_request(
        &mut self,
        channel_id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "sftp" {
            if let Some(channel) = self.channels.remove(&channel_id) {
                session.channel_success(channel_id);
                let sftp = SftpSession {
                    ctx: self.ctx.clone(),
                    peer: self.peer,
                    sid: self.sid,
                    handle: self.handle.clone(),
                    next_handle: 0,
                    files: HashMap::new(),
                    dirs: HashMap::new(),
                };
                tokio::spawn(russh_sftp::server::run(channel.into_stream(), sftp));
                return Ok(());
            }
        }
        session.channel_failure(channel_id);
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let cmd = String::from_utf8_lossy(data).to_string();
        let Some(scp) = parse_scp_command(&cmd) else {
            self.ctx.logger.log(
                Event::new(LogLevel::Warning, "ssh", "unsupported exec command")
                    .ip(self.peer.ip())
                    .session(self.sid)
                    .error(cmd),
            );
            session.channel_failure(channel_id);
            return Ok(());
        };
        let Some(channel) = self.channels.remove(&channel_id) else {
            session.channel_failure(channel_id);
            return Ok(());
        };
        session.channel_success(channel_id);
        self.ctx.sessions.update(self.sid, |s| s.protocol = Protocol::Scp);
        let ssh_handle = session.handle();
        let ctx = self.ctx.clone();
        let peer = self.peer;
        let sid = self.sid;
        let counter = self.handle.clone();
        tokio::spawn(async move {
            let mut io = ScpIo { channel, handle: ssh_handle, id: channel_id };
            let result = match scp.mode {
                ScpMode::Source => scp_source(&ctx, peer, sid, &counter, &mut io, &scp.path).await,
                ScpMode::Sink => scp_sink(&ctx, peer, sid, &counter, &mut io, &scp.path).await,
            };
            let status = match &result {
                Ok(()) => 0u32,
                Err(_) => 1u32,
            };
            if let Err(e) = result {
                let _ = io.send_error(&e).await;
                ctx.logger.log(
                    Event::new(LogLevel::Warning, "scp", "transfer failed")
                        .ip(peer.ip())
                        .session(sid)
                        .error(e.clone()),
                );
                ctx.sessions.update(sid, |s| s.state = SessionState::Failed(e));
            }
            let _ = io.handle.exit_status_request(io.id, status).await;
            let _ = io.handle.eof(io.id).await;
            let _ = io.handle.close(io.id).await;
        });
        Ok(())
    }
}

// ---------------------------------------------------------------- SCP -----

#[derive(Debug, PartialEq)]
enum ScpMode {
    /// `scp -f`: server sends a file to the client (download).
    Source,
    /// `scp -t`: server receives a file from the client (upload).
    Sink,
}

struct ScpRequest {
    mode: ScpMode,
    path: String,
}

fn parse_scp_command(cmd: &str) -> Option<ScpRequest> {
    let mut parts = cmd.split_whitespace();
    if parts.next()? != "scp" {
        return None;
    }
    let mut mode = None;
    let mut path = None;
    for p in parts {
        match p {
            "-f" => mode = Some(ScpMode::Source),
            "-t" => mode = Some(ScpMode::Sink),
            "-v" | "-p" | "-d" | "-r" => {}
            other if !other.starts_with('-') => path = Some(other.to_string()),
            _ => {}
        }
    }
    Some(ScpRequest { mode: mode?, path: path? })
}

/// Byte-level reader/writer on top of a russh channel.
struct ScpIo {
    channel: Channel<Msg>,
    handle: russh::server::Handle,
    id: ChannelId,
}

impl ScpIo {
    async fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.handle
            .data(self.id, CryptoVec::from_slice(data))
            .await
            .map_err(|_| "client closed channel".to_string())
    }

    async fn send_error(&mut self, msg: &str) -> Result<(), String> {
        self.send(format!("\x02scp: {msg}\n").as_bytes()).await
    }

    /// Receive the next chunk of raw channel data (None on EOF/close).
    async fn recv(&mut self) -> Option<Vec<u8>> {
        loop {
            match self.channel.wait().await? {
                ChannelMsg::Data { data } => return Some(data.to_vec()),
                ChannelMsg::Eof | ChannelMsg::Close => return None,
                _ => {}
            }
        }
    }
}

struct ScpReader {
    pending: Vec<u8>,
    pos: usize,
}

impl ScpReader {
    fn new() -> Self {
        Self { pending: Vec::new(), pos: 0 }
    }
    async fn next_byte(&mut self, io: &mut ScpIo) -> Option<u8> {
        loop {
            if self.pos < self.pending.len() {
                let b = self.pending[self.pos];
                self.pos += 1;
                return Some(b);
            }
            self.pending = io.recv().await?;
            self.pos = 0;
        }
    }
    async fn read_line(&mut self, io: &mut ScpIo) -> Option<String> {
        let mut line = Vec::new();
        loop {
            let b = self.next_byte(io).await?;
            if b == b'\n' {
                return Some(String::from_utf8_lossy(&line).to_string());
            }
            line.push(b);
            if line.len() > 4096 {
                return None;
            }
        }
    }
    async fn expect_ack(&mut self, io: &mut ScpIo) -> Result<(), String> {
        match self.next_byte(io).await {
            Some(0) => Ok(()),
            Some(1) | Some(2) => {
                let msg = self.read_line(io).await.unwrap_or_default();
                Err(format!("client error: {msg}"))
            }
            Some(b) => Err(format!("unexpected scp response: {b}")),
            None => Err("client closed connection".into()),
        }
    }
}

async fn scp_source(
    ctx: &ServiceCtx,
    peer: SocketAddr,
    sid: u64,
    counter: &Arc<crate::session::SessionHandle>,
    io: &mut ScpIo,
    path: &str,
) -> Result<(), String> {
    let mut rx = ScpReader::new();
    // The sink (client) announces readiness with a zero byte.
    rx.expect_ack(io).await?;

    let abs = match ctx.root.resolve(path) {
        Ok(p) if p.is_file() => p,
        Ok(_) => return Err(format!("{path}: not a regular file")),
        Err(e) => {
            ctx.logger.log(
                Event::new(LogLevel::Warning, "scp", "download")
                    .ip(peer.ip())
                    .session(sid)
                    .path(path.to_string())
                    .result("denied"),
            );
            return Err(e.to_string());
        }
    };
    let size = std::fs::metadata(&abs).map(|m| m.len()).map_err(|e| e.to_string())?;
    let name = abs.file_name().unwrap_or_default().to_string_lossy().to_string();
    ctx.sessions.update(sid, |s| {
        s.file = Some(path.to_string());
        s.direction = Some(Direction::Download);
        s.total = Some(size);
        s.state = SessionState::Transferring;
    });

    let start = Instant::now();
    io.send(format!("C0644 {size} {name}\n").as_bytes()).await?;
    rx.expect_ack(io).await?;

    let mut file = std::fs::File::open(&abs).map_err(|e| e.to_string())?;
    use std::io::Read;
    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        io.send(&buf[..n]).await?;
        counter.add_bytes(n as u64);
    }
    io.send(&[0]).await?;
    rx.expect_ack(io).await?;

    ctx.sessions.update(sid, |s| s.state = SessionState::Connected);
    ctx.logger.log(
        Event::new(LogLevel::Info, "scp", "download")
            .ip(peer.ip())
            .session(sid)
            .path(path.to_string())
            .result("ok")
            .bytes(size)
            .duration_ms(start.elapsed().as_millis() as u64),
    );
    ctx.sessions.finish(sid, SessionState::Completed);
    Ok(())
}

async fn scp_sink(
    ctx: &ServiceCtx,
    peer: SocketAddr,
    sid: u64,
    counter: &Arc<crate::session::SessionHandle>,
    io: &mut ScpIo,
    target: &str,
) -> Result<(), String> {
    if !ctx.cfg.uploads.enabled {
        return Err("uploads are disabled".into());
    }
    let mut rx = ScpReader::new();
    io.send(&[0]).await?; // ready

    // If the target names a file (not an existing directory), it wins over
    // the name announced in the C message.
    let target_is_dir = target.ends_with('/')
        || target == "."
        || matches!(ctx.root.resolve(target), Ok(p) if p.is_dir());
    let target_name = target.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();

    let limit = ctx.cfg.uploads.max_upload_mib * 1024 * 1024;
    loop {
        let Some(line) = rx.read_line(io).await else {
            break; // client done
        };
        let msg = line.as_str();
        if msg.is_empty() {
            continue;
        }
        match msg.as_bytes()[0] {
            b'C' => {
                // "C0644 <size> <name>"
                let mut parts = msg[1..].split_whitespace();
                let _perm = parts.next().ok_or("malformed scp header")?;
                let size: u64 = parts
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("malformed scp size")?;
                let announced = parts.next().unwrap_or("upload.bin").to_string();
                let name = if target_is_dir || target_name.is_empty() {
                    announced
                } else {
                    target_name.clone()
                };
                let mut guard = crate::services::begin_upload(ctx, &name, size).await?;
                if limit > 0 && size > limit {
                    return Err(format!("file exceeds upload limit of {} MiB", ctx.cfg.uploads.max_upload_mib));
                }
                ctx.sessions.update(sid, |s| {
                    s.file = Some(name.clone());
                    s.direction = Some(Direction::Upload);
                    s.total = Some(size);
                    s.state = SessionState::Transferring;
                });
                io.send(&[0]).await?;

                let start = Instant::now();
                let mut file = guard.file.take().expect("fresh upload guard has a file");
                let mut remaining = size;
                while remaining > 0 {
                    // Drain buffered bytes first, then raw channel chunks.
                    let chunk: Vec<u8> = if rx.pos < rx.pending.len() {
                        let take = ((rx.pending.len() - rx.pos) as u64).min(remaining) as usize;
                        let c = rx.pending[rx.pos..rx.pos + take].to_vec();
                        rx.pos += take;
                        c
                    } else {
                        let data = io.recv().await.ok_or("client aborted upload")?;
                        let take = (data.len() as u64).min(remaining) as usize;
                        if take < data.len() {
                            rx.pending = data[take..].to_vec();
                            rx.pos = 0;
                        }
                        data[..take].to_vec()
                    };
                    file.write_all(&chunk).await.map_err(|e| format!("write failed: {e}"))?;
                    counter.add_bytes(chunk.len() as u64);
                    remaining -= chunk.len() as u64;
                }
                rx.expect_ack(io).await?; // client's trailing \0
                guard.file = Some(file);
                let final_rel = guard.finalize().await?;
                io.send(&[0]).await?;
                ctx.sessions.update(sid, |s| s.state = SessionState::Connected);
                ctx.logger.log(
                    Event::new(LogLevel::Info, "scp", "upload")
                        .ip(peer.ip())
                        .session(sid)
                        .path(final_rel)
                        .result("ok")
                        .bytes(size)
                        .duration_ms(start.elapsed().as_millis() as u64),
                );
                ctx.sessions.finish(sid, SessionState::Completed);
            }
            b'T' => {
                io.send(&[0]).await?; // timestamps — accepted and ignored
            }
            b'D' => return Err("recursive upload is not supported".into()),
            b'E' => {
                io.send(&[0]).await?;
            }
            1 | 2 => return Err(format!("client error: {}", &msg[1..])),
            _ => return Err("unexpected scp message".into()),
        }
    }
    Ok(())
}

// --------------------------------------------------------------- SFTP -----

enum OpenFile {
    Read { file: std::fs::File, rel: String, size: u64, started: Instant, done: bool },
    Write { guard: Option<UploadGuard>, file: std::fs::File, rel: String, started: Instant, written: u64 },
}

struct SftpSession {
    ctx: ServiceCtx,
    peer: SocketAddr,
    sid: u64,
    handle: Arc<crate::session::SessionHandle>,
    next_handle: u64,
    files: HashMap<String, OpenFile>,
    dirs: HashMap<String, Option<Vec<File>>>,
}

impl SftpSession {
    fn new_handle(&mut self) -> String {
        self.next_handle += 1;
        format!("h{}", self.next_handle)
    }

    fn ok_status(id: u32) -> Status {
        Status {
            id,
            status_code: StatusCode::Ok,
            error_message: "Ok".into(),
            language_tag: "en-US".into(),
        }
    }

    /// SFTP paths arrive absolute ("/sub/file"); resolve inside the root.
    fn norm(path: &str) -> String {
        let mut clean: Vec<&str> = Vec::new();
        for comp in path.split('/') {
            match comp {
                "" | "." => {}
                ".." => {
                    clean.pop();
                }
                c => clean.push(c),
            }
        }
        clean.join("/")
    }
}

impl russh_sftp::server::Handler for SftpSession {
    type Error = StatusCode;

    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        Ok(Version::new())
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let norm = Self::norm(&path);
        Ok(Name { id, files: vec![File::dummy(format!("/{norm}"))] })
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        _attrs: FileAttributes,
    ) -> Result<SftpHandle, Self::Error> {
        let rel = Self::norm(&filename);
        if pflags.contains(OpenFlags::WRITE) {
            if !self.ctx.cfg.uploads.enabled {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "sftp", "upload")
                        .ip(self.peer.ip())
                        .session(self.sid)
                        .path(rel)
                        .result("uploads disabled"),
                );
                return Err(StatusCode::PermissionDenied);
            }
            let name = rel.rsplit('/').next().unwrap_or("upload.bin").to_string();
            let mut guard = crate::services::begin_upload(&self.ctx, &name, 0)
                .await
                .map_err(|e| {
                    self.ctx.logger.log(
                        Event::new(LogLevel::Warning, "sftp", "upload")
                            .ip(self.peer.ip())
                            .session(self.sid)
                            .path(name.clone())
                            .error(e),
                    );
                    StatusCode::Failure
                })?;
            let file = guard
                .file
                .take()
                .expect("fresh upload guard has a file")
                .try_into_std()
                .map_err(|_| StatusCode::Failure)?;
            self.ctx.sessions.update(self.sid, |s| {
                s.file = Some(rel.clone());
                s.direction = Some(Direction::Upload);
                s.total = None;
                s.state = SessionState::Transferring;
            });
            let h = self.new_handle();
            self.files.insert(
                h.clone(),
                OpenFile::Write {
                    guard: Some(guard),
                    file,
                    rel,
                    started: Instant::now(),
                    written: 0,
                },
            );
            Ok(SftpHandle { id, handle: h })
        } else {
            let abs = self.ctx.root.resolve(&rel).map_err(|e| {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "sftp", "open")
                        .ip(self.peer.ip())
                        .session(self.sid)
                        .path(rel.clone())
                        .error(e.to_string()),
                );
                StatusCode::NoSuchFile
            })?;
            if !abs.is_file() {
                return Err(StatusCode::NoSuchFile);
            }
            let file = std::fs::File::open(&abs).map_err(|_| StatusCode::PermissionDenied)?;
            let size = file.metadata().map(|m| m.len()).unwrap_or(0);
            self.ctx.sessions.update(self.sid, |s| {
                s.file = Some(rel.clone());
                s.direction = Some(Direction::Download);
                s.total = Some(size);
                s.state = SessionState::Transferring;
            });
            let h = self.new_handle();
            self.files.insert(
                h.clone(),
                OpenFile::Read { file, rel, size, started: Instant::now(), done: false },
            );
            Ok(SftpHandle { id, handle: h })
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let Some(OpenFile::Read { file, size, done, .. }) = self.files.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        let mut buf = vec![0u8; len.min(256 * 1024) as usize];
        let n = file.read_at(&mut buf, offset).map_err(|_| StatusCode::Failure)?;
        if n == 0 {
            *done = true;
            return Err(StatusCode::Eof);
        }
        buf.truncate(n);
        self.handle.add_bytes(n as u64);
        if offset + n as u64 >= *size {
            *done = true;
        }
        Ok(Data { id, data: buf })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let limit = self.ctx.cfg.uploads.max_upload_mib * 1024 * 1024;
        let Some(OpenFile::Write { file, written, .. }) = self.files.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.write_all_at(&data, offset).map_err(|_| StatusCode::Failure)?;
        *written += data.len() as u64;
        if limit > 0 && *written > limit {
            return Err(StatusCode::Failure);
        }
        self.handle.add_bytes(data.len() as u64);
        Ok(Self::ok_status(id))
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        if let Some(open) = self.files.remove(&handle) {
            match open {
                OpenFile::Read {
                    rel,
                    size,
                    started,
                    done,
                    ..
                } => {
                    self.ctx.sessions.finish(
                        self.sid,
                        if done {
                            SessionState::Completed
                        } else {
                            SessionState::Aborted
                        },
                    );
                    if done {
                        self.ctx.logger.log(
                            Event::new(LogLevel::Info, "sftp", "download")
                                .ip(self.peer.ip())
                                .session(self.sid)
                                .path(rel)
                                .result("ok")
                                .bytes(size)
                                .duration_ms(started.elapsed().as_millis() as u64),
                        );
                    }
                }
                OpenFile::Write { mut guard, file, rel, started, written } => {
                    file.sync_all().ok();
                    drop(file);
                    if let Some(g) = guard.take() {
                        match g.finalize().await {
                            Ok(final_rel) => {
                                self.ctx.sessions.update(self.sid, |s| {
                                    s.state = SessionState::Connected;
                                });
                                self.ctx.logger.log(
                                    Event::new(LogLevel::Info, "sftp", "upload")
                                        .ip(self.peer.ip())
                                        .session(self.sid)
                                        .path(final_rel)
                                        .result("ok")
                                        .bytes(written)
                                        .duration_ms(started.elapsed().as_millis() as u64),
                                );
                            }
                            Err(e) => {
                                self.ctx
                                    .sessions
                                    .finish(self.sid, SessionState::Failed(e.clone()));
                                self.ctx.logger.log(
                                    Event::new(LogLevel::Warning, "sftp", "upload")
                                        .ip(self.peer.ip())
                                        .session(self.sid)
                                        .path(rel)
                                        .error(e),
                                );
                                return Err(StatusCode::Failure);
                            }
                        }
                    }
                }
            }
        } else {
            self.dirs.remove(&handle);
        }
        Ok(Self::ok_status(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<SftpHandle, Self::Error> {
        let rel = Self::norm(&path);
        let abs = self.ctx.root.resolve(&rel).map_err(|_| StatusCode::NoSuchFile)?;
        if !abs.is_dir() {
            return Err(StatusCode::NoSuchFile);
        }
        let mut files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&abs) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') {
                    continue;
                }
                if let Ok(meta) = e.metadata() {
                    let attrs = FileAttributes::from(&meta);
                    let mut f = File::new(name.clone(), attrs);
                    let kind = if meta.is_dir() { 'd' } else { '-' };
                    f.longname = format!("{kind}rw-r--r-- 1 tb tb {:>12} {name}", meta.len());
                    files.push(f);
                }
            }
        }
        let h = self.new_handle();
        self.dirs.insert(h.clone(), Some(files));
        Ok(SftpHandle { id, handle: h })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.dirs.get_mut(&handle) {
            Some(entries @ Some(_)) => {
                let files = entries.take().unwrap();
                Ok(Name { id, files })
            }
            Some(None) => Err(StatusCode::Eof),
            None => Err(StatusCode::Failure),
        }
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        let rel = Self::norm(&path);
        let abs = self.ctx.root.resolve(&rel).map_err(|_| StatusCode::NoSuchFile)?;
        let meta = std::fs::metadata(&abs).map_err(|_| StatusCode::NoSuchFile)?;
        Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.stat(id, path).await
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        match self.files.get(&handle) {
            Some(OpenFile::Read { file, .. }) => {
                let meta = file.metadata().map_err(|_| StatusCode::Failure)?;
                Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
            }
            Some(OpenFile::Write { file, .. }) => {
                let meta = file.metadata().map_err(|_| StatusCode::Failure)?;
                Ok(Attrs { id, attrs: FileAttributes::from(&meta) })
            }
            None => Err(StatusCode::Failure),
        }
    }
}
