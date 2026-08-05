use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::fsroot::PathError;
use crate::logging::{Event, LogLevel};
use crate::services::ServiceCtx;
use crate::session::{Direction, Protocol, SessionState};

const CHUNK: usize = 64 * 1024;
const MAX_HEADER: usize = 16 * 1024;

/// Minimal HTTP/1.1 file server: GET/HEAD downloads, PUT/POST uploads (when
/// enabled), directory listings. One request per connection — exactly the
/// pattern `copy http://...` on IOS and curl/wget use.
pub async fn run(ctx: ServiceCtx, tls: bool) -> Result<(), String> {
    let addr = ctx.bind_addr();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| bind_error(addr, e))?;

    let tls_acceptor = if tls {
        let (certs, key) = crate::certs::load_or_generate(&ctx.cfg)
            .map_err(|e| format!("certificate error: {e:#}"))?;
        let server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("invalid certificate/key: {e}"))?;
        Some(tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg)))
    } else {
        None
    };

    ctx.set_running();
    let mut shutdown = ctx.shutdown.clone();
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(x) => x,
                    Err(e) => {
                        ctx.logger.log(Event::new(LogLevel::Warning, ctx.id.log_proto(), "accept failed").error(e.to_string()));
                        continue;
                    }
                };
                if ctx.at_session_limit() {
                    ctx.logger.log(Event::new(LogLevel::Warning, ctx.id.log_proto(), "session limit reached, rejecting").ip(peer.ip()));
                    continue;
                }
                let ctx = ctx.clone();
                let tls_acceptor = tls_acceptor.clone();
                tokio::spawn(async move {
                    let proto = if tls_acceptor.is_some() { Protocol::Https } else { Protocol::Http };
                    let handle = ctx.sessions.open(proto, peer, ctx.bind_addr().port());
                    let sid = handle.id;
                    let timeout = Duration::from_secs(ctx.cfg.idle_timeout_secs.max(10));
                    let result = match tls_acceptor {
                        Some(acceptor) => {
                            match tokio::time::timeout(timeout, acceptor.accept(stream)).await {
                                Ok(Ok(tls_stream)) => {
                                    handle_connection(&ctx, proto, sid, &handle, tls_stream, timeout).await
                                }
                                Ok(Err(e)) => Err(format!("TLS handshake failed: {e}")),
                                Err(_) => Err("TLS handshake timeout".into()),
                            }
                        }
                        None => handle_connection(&ctx, proto, sid, &handle, stream, timeout).await,
                    };
                    match result {
                        Ok(()) => ctx.sessions.close(sid, SessionState::Completed),
                        Err(e) => {
                            ctx.logger.log(
                                Event::new(LogLevel::Warning, proto.label(), "request failed")
                                    .ip(peer.ip()).session(sid).error(e.clone()),
                            );
                            ctx.sessions.close(sid, SessionState::Failed(e));
                        }
                    }
                });
            }
        }
    }
    Ok(())
}

pub fn bind_error(addr: std::net::SocketAddr, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => {
            format!("port {} is already in use — choose another port", addr.port())
        }
        std::io::ErrorKind::PermissionDenied => format!(
            "no permission for port {} — run with sudo or pick a port >= 1024",
            addr.port()
        ),
        std::io::ErrorKind::AddrNotAvailable => {
            format!("bind address {} is not available on this host", addr.ip())
        }
        _ => format!("could not bind {addr}: {e}"),
    }
}

async fn handle_connection<S>(
    ctx: &ServiceCtx,
    proto: Protocol,
    sid: u64,
    handle: &Arc<crate::session::SessionHandle>,
    stream: S,
    timeout: Duration,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let mut reader = BufReader::new(stream);

    // ---- read request head ----
    let mut head = Vec::new();
    let deadline = Instant::now() + timeout;
    loop {
        let mut byte = [0u8; 1];
        let n = tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), reader.read(&mut byte))
            .await
            .map_err(|_| "request timeout".to_string())?
            .map_err(|e| format!("read error: {e}"))?;
        if n == 0 {
            return Err("client closed connection".into());
        }
        head.push(byte[0]);
        if head.len() > MAX_HEADER {
            return Err("request header too large".into());
        }
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let head_text = String::from_utf8_lossy(&head).to_string();
    let mut lines = head_text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_uppercase();
    let raw_path = parts.next().unwrap_or("/");
    let path = percent_decode(raw_path.split('?').next().unwrap_or("/"));
    let mut content_length: Option<u64> = None;
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                content_length = v.trim().parse().ok();
            }
        }
    }

    let peer_ip = {
        let sessions = ctx.sessions.snapshot();
        sessions.iter().find(|s| s.id == sid).map(|s| s.peer.ip())
    };
    let log = |lvl, action: &str| {
        let mut ev = Event::new(lvl, proto.label(), action.to_string()).session(sid).path(path.clone());
        if let Some(ip) = peer_ip {
            ev = ev.ip(ip);
        }
        ev
    };

    match method.as_str() {
        "GET" | "HEAD" => {
            ctx.sessions.update(sid, |s| {
                s.file = Some(path.clone());
                s.direction = Some(Direction::Download);
            });
            match ctx.root.resolve(&path) {
                Ok(abs) if abs.is_dir() => {
                    let body = directory_listing(ctx, &path, &abs);
                    send_response(&mut reader, 200, "OK", "text/html; charset=utf-8", Some(body.as_bytes()), method == "HEAD").await
                }
                Ok(abs) => {
                    let start = Instant::now();
                    let meta = tokio::fs::metadata(&abs).await.map_err(|e| format!("stat failed: {e}"))?;
                    let size = meta.len();
                    ctx.sessions.update(sid, |s| {
                        s.total = Some(size);
                        s.state = SessionState::Transferring;
                    });
                    let mut file = tokio::fs::File::open(&abs)
                        .await
                        .map_err(|e| format!("file not readable: {e}"))?;
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {size}\r\nContent-Type: {}\r\nConnection: close\r\nServer: transferbuddy\r\n\r\n",
                        content_type(&path)
                    );
                    let stream = reader.get_mut();
                    stream.write_all(headers.as_bytes()).await.map_err(|e| format!("write failed: {e}"))?;
                    if method != "HEAD" {
                        let mut buf = vec![0u8; CHUNK];
                        loop {
                            let n = file.read(&mut buf).await.map_err(|e| format!("read failed: {e}"))?;
                            if n == 0 {
                                break;
                            }
                            stream
                                .write_all(&buf[..n])
                                .await
                                .map_err(|_| "client aborted transfer".to_string())?;
                            handle.add_bytes(n as u64);
                        }
                        stream.flush().await.map_err(|e| format!("flush failed: {e}"))?;
                        ctx.logger.log(
                            log(LogLevel::Info, "download")
                                .result("ok")
                                .bytes(size)
                                .duration_ms(start.elapsed().as_millis() as u64),
                        );
                    }
                    Ok(())
                }
                Err(PathError::Denied(_)) => {
                    ctx.logger.log(log(LogLevel::Warning, "download").result("denied"));
                    send_response(&mut reader, 403, "Forbidden", "text/plain", Some(b"403 forbidden\n"), false).await
                }
                Err(PathError::NotFound(_)) => {
                    ctx.logger.log(log(LogLevel::Info, "download").result("not found"));
                    send_response(&mut reader, 404, "Not Found", "text/plain", Some(b"404 not found\n"), false).await
                }
            }
        }
        "PUT" | "POST" => {
            ctx.sessions.update(sid, |s| {
                s.file = Some(path.clone());
                s.direction = Some(Direction::Upload);
            });
            if !ctx.cfg.uploads.enabled {
                ctx.logger.log(log(LogLevel::Warning, "upload").result("uploads disabled"));
                return send_response(&mut reader, 403, "Forbidden", "text/plain", Some(b"uploads are disabled\n"), false).await;
            }
            let size = content_length.ok_or("missing Content-Length")?;
            let start = Instant::now();
            ctx.sessions.update(sid, |s| {
                s.total = Some(size);
                s.state = SessionState::Transferring;
            });
            let name = path.rsplit('/').next().unwrap_or("").to_string();
            let result = async {
                let mut guard = crate::services::begin_upload(ctx, &name, size).await?;
                let mut file = guard.file.take().expect("fresh upload guard has a file");
                copy_body(&mut reader, &mut file, size, handle.clone()).await?;
                guard.file = Some(file);
                guard.finalize().await
            }
            .await;
            match result {
                Ok(final_path) => {
                    ctx.logger.log(
                        log(LogLevel::Info, "upload")
                            .result("ok")
                            .bytes(size)
                            .duration_ms(start.elapsed().as_millis() as u64),
                    );
                    let body = format!("stored as {}\n", final_path);
                    send_response(&mut reader, 201, "Created", "text/plain", Some(body.as_bytes()), false).await
                }
                Err(e) => {
                    ctx.logger.log(log(LogLevel::Warning, "upload").result("failed").error(e.clone()));
                    let body = format!("upload rejected: {e}\n");
                    send_response(&mut reader, 409, "Conflict", "text/plain", Some(body.as_bytes()), false).await?;
                    Err(e)
                }
            }
        }
        _ => {
            send_response(&mut reader, 405, "Method Not Allowed", "text/plain", Some(b"405 method not allowed\n"), false).await
        }
    }
}

async fn copy_body<R>(
    reader: &mut R,
    writer: &mut tokio::fs::File,
    size: u64,
    handle: Arc<crate::session::SessionHandle>,
) -> Result<(), String>
where
    R: AsyncRead + Unpin + Send,
{
    let mut remaining = size;
    let mut buf = vec![0u8; CHUNK];
    while remaining > 0 {
        let want = remaining.min(CHUNK as u64) as usize;
        let n = reader
            .read(&mut buf[..want])
            .await
            .map_err(|_| "client aborted upload".to_string())?;
        if n == 0 {
            return Err("client aborted upload".into());
        }
        writer.write_all(&buf[..n]).await.map_err(|e| format!("write failed: {e}"))?;
        handle.add_bytes(n as u64);
        remaining -= n as u64;
    }
    writer.flush().await.map_err(|e| format!("flush failed: {e}"))?;
    Ok(())
}

async fn send_response<S>(
    stream: &mut S,
    code: u16,
    reason: &str,
    ctype: &str,
    body: Option<&[u8]>,
    head_only: bool,
) -> Result<(), String>
where
    S: AsyncWrite + Unpin + Send,
{
    let body = body.unwrap_or_default();
    let headers = format!(
        "HTTP/1.1 {code} {reason}\r\nContent-Length: {}\r\nContent-Type: {ctype}\r\nConnection: close\r\nServer: transferbuddy\r\n\r\n",
        body.len()
    );
    stream.write_all(headers.as_bytes()).await.map_err(|e| format!("write failed: {e}"))?;
    if !head_only {
        stream.write_all(body).await.map_err(|e| format!("write failed: {e}"))?;
    }
    stream.flush().await.map_err(|e| format!("flush failed: {e}"))?;
    Ok(())
}

fn directory_listing(ctx: &ServiceCtx, rel: &str, abs: &std::path::Path) -> String {
    let mut rows = String::new();
    if let Ok(entries) = std::fs::read_dir(abs) {
        let mut items: Vec<_> = entries.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for e in items {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
            let size = e.metadata().map(|m| m.len()).unwrap_or(0);
            let href = format!(
                "{}/{}",
                rel.trim_end_matches('/'),
                percent_encode(&name)
            );
            rows.push_str(&format!(
                "<tr><td><a href=\"/{}\">{}{}</a></td><td>{}</td></tr>\n",
                href.trim_start_matches('/'),
                html_escape(&name),
                if is_dir { "/" } else { "" },
                if is_dir { "-".into() } else { crate::session::fmt_bytes(size) },
            ));
        }
    }
    format!(
        "<!DOCTYPE html><html><head><title>transferbuddy — /{rel}</title></head><body>\
         <h2>transferbuddy — /{rel}</h2><p>root: {}</p><table>{rows}</table></body></html>",
        html_escape(&ctx.root.root().display().to_string()),
        rel = html_escape(rel.trim_start_matches('/')),
    )
}

fn content_type(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".txt") || lower.ends_with(".cfg") || lower.ends_with(".conf") || lower.ends_with(".log") {
        "text/plain; charset=utf-8"
    } else if lower.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".xml") {
        "application/xml"
    } else {
        "application/octet-stream"
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
