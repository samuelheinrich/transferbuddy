//! Minimal FTP server (RFC 959) tailored to what Cisco IOS/IOS-XE `copy ftp:`
//! and standard clients (curl, ftp, lftp) need: USER/PASS login, passive
//! (PASV/EPSV) and active (PORT/EPRT) data connections, RETR/STOR/LIST/SIZE.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};

use crate::auth::AuthResult;
use crate::logging::{Event, LogLevel};
use crate::services::ServiceCtx;
use crate::session::{Direction, Protocol, SessionState};

const CHUNK: usize = 64 * 1024;

pub async fn run(ctx: ServiceCtx) -> Result<(), String> {
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
                        ctx.logger.log(Event::new(LogLevel::Warning, "ftp", "accept failed").error(e.to_string()));
                        continue;
                    }
                };
                if ctx.at_session_limit() {
                    ctx.logger.log(Event::new(LogLevel::Warning, "ftp", "session limit reached, rejecting").ip(peer.ip()));
                    continue;
                }
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    let handle = ctx.sessions.open(Protocol::Ftp, peer, ctx.bind_addr().port());
                    let sid = handle.id;
                    let state = match Conn::serve(&ctx, stream, peer, sid, handle).await {
                        Ok(()) => SessionState::Completed,
                        Err(e) => {
                            ctx.logger.log(Event::new(LogLevel::Warning, "ftp", "session ended").ip(peer.ip()).session(sid).error(e.clone()));
                            SessionState::Failed(e)
                        }
                    };
                    ctx.sessions.close(sid, state);
                });
            }
        }
    }
    Ok(())
}

enum DataMode {
    None,
    Passive(TcpListener),
    Active(SocketAddr),
}

struct Conn<'a> {
    ctx: &'a ServiceCtx,
    reader: BufReader<ReadHalf<TcpStream>>,
    writer: WriteHalf<TcpStream>,
    peer: SocketAddr,
    local: SocketAddr,
    sid: u64,
    handle: Arc<crate::session::SessionHandle>,
    authed: bool,
    username: String,
    cwd: String,
    data: DataMode,
    timeout: Duration,
}

impl<'a> Conn<'a> {
    async fn serve(
        ctx: &'a ServiceCtx,
        stream: TcpStream,
        peer: SocketAddr,
        sid: u64,
        handle: Arc<crate::session::SessionHandle>,
    ) -> Result<(), String> {
        let local = stream.local_addr().map_err(|e| e.to_string())?;
        let (r, w) = tokio::io::split(stream);
        let mut conn = Conn {
            ctx,
            reader: BufReader::new(r),
            writer: w,
            peer,
            local,
            sid,
            handle,
            authed: false,
            username: String::new(),
            cwd: String::new(),
            data: DataMode::None,
            timeout: Duration::from_secs(ctx.cfg.idle_timeout_secs.max(10)),
        };
        conn.reply(220, "transferbuddy FTP ready").await?;
        conn.command_loop().await
    }

    async fn reply(&mut self, code: u16, text: &str) -> Result<(), String> {
        let line = format!("{code} {text}\r\n");
        self.writer.write_all(line.as_bytes()).await.map_err(|e| e.to_string())?;
        self.writer.flush().await.map_err(|e| e.to_string())
    }

    async fn command_loop(&mut self) -> Result<(), String> {
        let mut shutdown = self.ctx.shutdown.clone();
        loop {
            let mut line = String::new();
            let read = tokio::select! {
                _ = shutdown.changed() => {
                    let _ = self.reply(421, "server shutting down").await;
                    return Ok(());
                }
                r = tokio::time::timeout(self.timeout, self.reader.read_line(&mut line)) => r,
            };
            let n = match read {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => {
                    let _ = self.reply(421, "idle timeout").await;
                    return Ok(());
                }
            };
            if n == 0 {
                return Ok(()); // client closed
            }
            let line = line.trim_end().to_string();
            let (cmd, arg) = match line.split_once(' ') {
                Some((c, a)) => (c.to_uppercase(), a.to_string()),
                None => (line.to_uppercase(), String::new()),
            };
            if !matches!(cmd.as_str(), "PASS") {
                self.ctx.logger.log(
                    Event::new(LogLevel::Debug, "ftp", format!("<- {cmd} {arg}"))
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
            }
            match cmd.as_str() {
                "USER" => {
                    self.username = arg;
                    self.reply(331, "password required").await?;
                }
                "PASS" => self.handle_pass(&arg).await?,
                "QUIT" => {
                    self.reply(221, "bye").await?;
                    return Ok(());
                }
                "SYST" => self.reply(215, "UNIX Type: L8").await?,
                "FEAT" => {
                    self.writer
                        .write_all(b"211-Features:\r\n SIZE\r\n EPSV\r\n UTF8\r\n211 End\r\n")
                        .await
                        .map_err(|e| e.to_string())?;
                }
                "NOOP" => self.reply(200, "ok").await?,
                "TYPE" => self.reply(200, "type set").await?,
                "MODE" | "STRU" => self.reply(200, "ok").await?,
                "ABOR" => self.reply(226, "nothing to abort").await?,
                _ if !self.authed => self.reply(530, "please login with USER and PASS").await?,
                "PWD" => {
                    let cwd = format!("/{}", self.cwd);
                    self.reply(257, &format!("\"{cwd}\"")).await?;
                }
                "CWD" => self.handle_cwd(&arg).await?,
                "CDUP" => {
                    self.cwd = self.cwd.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
                    self.reply(250, "directory changed").await?;
                }
                "PASV" => self.handle_pasv().await?,
                "EPSV" => self.handle_epsv().await?,
                "PORT" => self.handle_port(&arg).await?,
                "EPRT" => self.handle_eprt(&arg).await?,
                "SIZE" => self.handle_size(&arg).await?,
                "LIST" | "NLST" => self.handle_list(cmd == "NLST", &arg).await?,
                "RETR" => self.handle_retr(&arg).await?,
                "STOR" => self.handle_stor(&arg).await?,
                "DELE" | "RMD" | "MKD" | "RNFR" | "RNTO" => {
                    self.reply(550, "operation not permitted").await?;
                }
                _ => self.reply(502, "command not implemented").await?,
            }
        }
    }

    async fn handle_pass(&mut self, pass: &str) -> Result<(), String> {
        if self.username.is_empty() {
            return self.reply(503, "send USER first").await;
        }
        match self.ctx.auth.check(self.peer.ip(), &self.username, pass) {
            AuthResult::Ok => {
                self.authed = true;
                let user = self.username.clone();
                self.ctx.sessions.update(self.sid, |s| s.username = Some(user));
                self.ctx.logger.log(
                    Event::new(LogLevel::Info, "ftp", "login ok")
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
                self.reply(230, "login successful").await
            }
            AuthResult::LockedOut => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ftp", "login locked out")
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
                self.reply(530, "too many failed logins, try again later").await
            }
            AuthResult::BadCredentials => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ftp", "login failed")
                        .ip(self.peer.ip())
                        .session(self.sid),
                );
                self.reply(530, "login incorrect").await
            }
        }
    }

    fn rel(&self, arg: &str) -> String {
        let arg = arg.trim();
        if arg.starts_with('/') {
            arg.trim_start_matches('/').to_string()
        } else if self.cwd.is_empty() {
            arg.to_string()
        } else {
            format!("{}/{}", self.cwd, arg)
        }
    }

    async fn handle_cwd(&mut self, arg: &str) -> Result<(), String> {
        let rel = self.rel(arg);
        match self.ctx.root.resolve(&rel) {
            Ok(p) if p.is_dir() => {
                self.cwd = self.ctx.root.relative(&p).unwrap_or_default();
                self.reply(250, "directory changed").await
            }
            _ => self.reply(550, "no such directory").await,
        }
    }

    async fn handle_pasv(&mut self) -> Result<(), String> {
        let listener = TcpListener::bind(SocketAddr::new(self.local.ip(), 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let ip = match self.local.ip() {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => return self.reply(425, "use EPSV with IPv6").await,
        };
        let o = ip.octets();
        let reply = format!(
            "Entering Passive Mode ({},{},{},{},{},{})",
            o[0], o[1], o[2], o[3], port >> 8, port & 0xff
        );
        self.data = DataMode::Passive(listener);
        self.reply(227, &reply).await
    }

    async fn handle_epsv(&mut self) -> Result<(), String> {
        let listener = TcpListener::bind(SocketAddr::new(self.local.ip(), 0))
            .await
            .map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        self.data = DataMode::Passive(listener);
        self.reply(229, &format!("Entering Extended Passive Mode (|||{port}|)")).await
    }

    async fn handle_port(&mut self, arg: &str) -> Result<(), String> {
        let parts: Vec<u8> = arg.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if parts.len() != 6 {
            return self.reply(501, "bad PORT argument").await;
        }
        let ip = IpAddr::from([parts[0], parts[1], parts[2], parts[3]]);
        let port = ((parts[4] as u16) << 8) | parts[5] as u16;
        // Only connect back to the control connection's address (anti-bounce).
        if ip != self.peer.ip() {
            return self.reply(501, "PORT address must match control connection").await;
        }
        self.data = DataMode::Active(SocketAddr::new(ip, port));
        self.reply(200, "PORT ok").await
    }

    async fn handle_eprt(&mut self, arg: &str) -> Result<(), String> {
        // |1|ip|port| or |2|ipv6|port|
        let parts: Vec<&str> = arg.trim_matches('|').split('|').collect();
        if parts.len() != 3 {
            return self.reply(501, "bad EPRT argument").await;
        }
        let ip: IpAddr = match parts[1].parse() {
            Ok(ip) => ip,
            Err(_) => return self.reply(501, "bad EPRT address").await,
        };
        let port: u16 = match parts[2].parse() {
            Ok(p) => p,
            Err(_) => return self.reply(501, "bad EPRT port").await,
        };
        if ip != self.peer.ip() {
            return self.reply(501, "EPRT address must match control connection").await;
        }
        self.data = DataMode::Active(SocketAddr::new(ip, port));
        self.reply(200, "EPRT ok").await
    }

    async fn open_data(&mut self) -> Result<TcpStream, String> {
        let mode = std::mem::replace(&mut self.data, DataMode::None);
        match mode {
            DataMode::Passive(listener) => {
                match tokio::time::timeout(Duration::from_secs(30), listener.accept()).await {
                    Ok(Ok((stream, _))) => Ok(stream),
                    Ok(Err(e)) => Err(format!("data connection failed: {e}")),
                    Err(_) => Err("timeout waiting for data connection".into()),
                }
            }
            DataMode::Active(addr) => {
                match tokio::time::timeout(Duration::from_secs(30), TcpStream::connect(addr)).await {
                    Ok(Ok(stream)) => Ok(stream),
                    Ok(Err(e)) => Err(format!("data connection failed: {e}")),
                    Err(_) => Err("timeout opening data connection".into()),
                }
            }
            DataMode::None => Err("no data connection — send PASV or PORT first".into()),
        }
    }

    async fn handle_size(&mut self, arg: &str) -> Result<(), String> {
        let rel = self.rel(arg);
        match self.ctx.root.resolve(&rel) {
            Ok(p) if p.is_file() => {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                self.reply(213, &size.to_string()).await
            }
            _ => self.reply(550, "no such file").await,
        }
    }

    async fn handle_list(&mut self, names_only: bool, arg: &str) -> Result<(), String> {
        let arg = arg.trim_start_matches("-l").trim().to_string();
        let rel = self.rel(&arg);
        let dir = match self.ctx.root.resolve(&rel) {
            Ok(p) if p.is_dir() => p,
            Ok(p) if p.is_file() => p,
            _ => return self.reply(550, "no such directory").await,
        };
        let mut data = match self.open_data().await {
            Ok(s) => {
                self.reply(150, "opening data connection").await?;
                s
            }
            Err(e) => return self.reply(425, &e).await,
        };
        let mut listing = String::new();
        let entries: Vec<std::path::PathBuf> = if dir.is_file() {
            vec![dir]
        } else {
            let mut v: Vec<_> = std::fs::read_dir(&dir)
                .map_err(|e| e.to_string())?
                .flatten()
                .map(|e| e.path())
                .collect();
            v.sort();
            v
        };
        for path in entries {
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            if names_only {
                listing.push_str(&format!("{name}\r\n"));
            } else if let Ok(meta) = path.metadata() {
                let kind = if meta.is_dir() { 'd' } else { '-' };
                let mtime: chrono::DateTime<chrono::Local> =
                    meta.modified().map(Into::into).unwrap_or_else(|_| chrono::Local::now());
                listing.push_str(&format!(
                    "{kind}rw-r--r-- 1 ftp ftp {:>12} {} {name}\r\n",
                    meta.len(),
                    mtime.format("%b %e %H:%M"),
                ));
            }
        }
        data.write_all(listing.as_bytes()).await.map_err(|e| e.to_string())?;
        data.shutdown().await.ok();
        self.reply(226, "transfer complete").await
    }

    async fn handle_retr(&mut self, arg: &str) -> Result<(), String> {
        let rel = self.rel(arg);
        let abs = match self.ctx.root.resolve(&rel) {
            Ok(p) if p.is_file() => p,
            Ok(_) => return self.reply(550, "not a file").await,
            Err(crate::fsroot::PathError::Denied(_)) => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ftp", "download")
                        .ip(self.peer.ip())
                        .session(self.sid)
                        .path(rel.clone())
                        .result("denied"),
                );
                return self.reply(550, "access denied").await;
            }
            Err(_) => return self.reply(550, "no such file").await,
        };
        let size = std::fs::metadata(&abs).map(|m| m.len()).unwrap_or(0);
        self.ctx.sessions.update(self.sid, |s| {
            s.file = Some(rel.clone());
            s.direction = Some(Direction::Download);
            s.total = Some(size);
            s.state = SessionState::Transferring;
        });
        let mut data = match self.open_data().await {
            Ok(s) => {
                self.reply(150, &format!("opening data connection ({size} bytes)")).await?;
                s
            }
            Err(e) => return self.reply(425, &e).await,
        };
        let start = Instant::now();
        let mut file = tokio::fs::File::open(&abs).await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; CHUNK];
        let mut ok = true;
        loop {
            let n = file.read(&mut buf).await.map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            if data.write_all(&buf[..n]).await.is_err() {
                ok = false;
                break;
            }
            self.handle.add_bytes(n as u64);
        }
        data.shutdown().await.ok();
        self.ctx.sessions.update(self.sid, |s| s.state = SessionState::Connected);
        if ok {
            self.ctx.logger.log(
                Event::new(LogLevel::Info, "ftp", "download")
                    .ip(self.peer.ip())
                    .session(self.sid)
                    .path(rel)
                    .result("ok")
                    .bytes(size)
                    .duration_ms(start.elapsed().as_millis() as u64),
            );
            self.reply(226, "transfer complete").await
        } else {
            self.ctx.logger.log(
                Event::new(LogLevel::Warning, "ftp", "download")
                    .ip(self.peer.ip())
                    .session(self.sid)
                    .path(rel)
                    .result("aborted"),
            );
            self.reply(426, "transfer aborted").await
        }
    }

    async fn handle_stor(&mut self, arg: &str) -> Result<(), String> {
        if !self.ctx.cfg.uploads.enabled {
            return self.reply(550, "uploads are disabled").await;
        }
        let rel = self.rel(arg);
        let name = rel.rsplit('/').next().unwrap_or("").to_string();
        let mut guard = match crate::services::begin_upload(self.ctx, &name, 0).await {
            Ok(g) => g,
            Err(e) => return self.reply(550, &e).await,
        };
        self.ctx.sessions.update(self.sid, |s| {
            s.file = Some(rel.clone());
            s.direction = Some(Direction::Upload);
            s.total = None;
            s.state = SessionState::Transferring;
        });
        let mut data = match self.open_data().await {
            Ok(s) => {
                self.reply(150, "ready to receive").await?;
                s
            }
            Err(e) => return self.reply(425, &e).await,
        };
        let start = Instant::now();
        let mut file = guard.file.take().expect("fresh upload guard has a file");
        let limit = self.ctx.cfg.uploads.max_upload_mib * 1024 * 1024;
        let mut buf = vec![0u8; CHUNK];
        let mut received: u64 = 0;
        let result: Result<(), String> = loop {
            let n = match data.read(&mut buf).await {
                Ok(0) => break Ok(()),
                Ok(n) => n,
                Err(_) => break Err("client aborted upload".into()),
            };
            if let Err(e) = file.write_all(&buf[..n]).await {
                break Err(format!("write failed: {e}"));
            }
            self.handle.add_bytes(n as u64);
            received += n as u64;
            if limit > 0 && received > limit {
                break Err("upload size limit exceeded".into());
            }
        };
        self.ctx.sessions.update(self.sid, |s| s.state = SessionState::Connected);
        match result {
            Ok(()) => {
                guard.file = Some(file);
                match guard.finalize().await {
                    Ok(final_rel) => {
                        self.ctx.logger.log(
                            Event::new(LogLevel::Info, "ftp", "upload")
                                .ip(self.peer.ip())
                                .session(self.sid)
                                .path(final_rel)
                                .result("ok")
                                .bytes(received)
                                .duration_ms(start.elapsed().as_millis() as u64),
                        );
                        self.reply(226, "transfer complete").await
                    }
                    Err(e) => self.reply(550, &e).await,
                }
            }
            Err(e) => {
                self.ctx.logger.log(
                    Event::new(LogLevel::Warning, "ftp", "upload")
                        .ip(self.peer.ip())
                        .session(self.sid)
                        .path(rel)
                        .result("failed")
                        .error(e.clone()),
                );
                self.reply(426, &e).await
            }
        }
    }
}
