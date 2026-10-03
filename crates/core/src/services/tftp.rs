//! TFTP server (RFC 1350) with option negotiation (RFC 2347): `blksize`
//! (RFC 2348) and `tsize` (RFC 2349). Block numbers roll over so images
//! larger than 32 MB transfer fine even with the default 512-byte blocks.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UdpSocket;

use crate::logging::{Event, LogLevel};
use crate::services::ServiceCtx;
use crate::session::{Direction, Protocol, SessionState};

const OP_RRQ: u16 = 1;
const OP_WRQ: u16 = 2;
const OP_DATA: u16 = 3;
const OP_ACK: u16 = 4;
const OP_ERROR: u16 = 5;
const OP_OACK: u16 = 6;

const RETRIES: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(3);

pub async fn run(ctx: ServiceCtx) -> Result<(), String> {
    let addr = ctx.bind_addr();
    let socket = crate::platform::bind_udp(addr)
        .await
        .map_err(|e| crate::services::http::bind_error(addr, e))?;
    ctx.set_running();

    let mut shutdown = ctx.shutdown.clone();
    let mut buf = vec![0u8; 65536];
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            recv = socket.recv_from(&mut buf) => {
                let (n, peer) = match recv {
                    Ok(x) => x,
                    Err(e) => {
                        ctx.logger.log(Event::new(LogLevel::Warning, "tftp", "recv failed").error(e.to_string()));
                        continue;
                    }
                };
                if ctx.at_session_limit() {
                    ctx.logger.log(Event::new(LogLevel::Warning, "tftp", "session limit reached, rejecting").ip(peer.ip()));
                    continue;
                }
                let packet = buf[..n].to_vec();
                let ctx = ctx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_request(ctx.clone(), peer, packet).await {
                        ctx.logger.log(Event::new(LogLevel::Warning, "tftp", "request failed").ip(peer.ip()).error(e));
                    }
                });
            }
        }
    }
    Ok(())
}

struct Request {
    op: u16,
    filename: String,
    options: Vec<(String, String)>,
}

fn parse_request(packet: &[u8]) -> Option<Request> {
    if packet.len() < 4 {
        return None;
    }
    let op = u16::from_be_bytes([packet[0], packet[1]]);
    if op != OP_RRQ && op != OP_WRQ {
        return None;
    }
    let mut fields = packet[2..]
        .split(|b| *b == 0)
        .map(|f| String::from_utf8_lossy(f).to_string());
    let filename = fields.next()?;
    let _mode = fields.next()?; // octet/netascii — we always transfer binary
    let mut options = Vec::new();
    while let Some(k) = fields.next() {
        if k.is_empty() {
            break;
        }
        let Some(v) = fields.next() else { break };
        options.push((k.to_lowercase(), v));
    }
    Some(Request {
        op,
        filename,
        options,
    })
}

async fn send_error(socket: &UdpSocket, code: u16, msg: &str) {
    let mut pkt = Vec::with_capacity(msg.len() + 5);
    pkt.extend_from_slice(&OP_ERROR.to_be_bytes());
    pkt.extend_from_slice(&code.to_be_bytes());
    pkt.extend_from_slice(msg.as_bytes());
    pkt.push(0);
    let _ = socket.send(&pkt).await;
}

async fn handle_request(ctx: ServiceCtx, peer: SocketAddr, packet: Vec<u8>) -> Result<(), String> {
    let Some(req) = parse_request(&packet) else {
        return Ok(()); // not a request — ignore stray packet
    };

    // Per-transfer socket on an ephemeral port, connected to the client (TID).
    let local_ip = ctx.bind_addr().ip();
    let bind_any = SocketAddr::new(local_ip, 0);
    let socket = UdpSocket::bind(bind_any).await.map_err(|e| e.to_string())?;
    socket.connect(peer).await.map_err(|e| e.to_string())?;

    let handle = ctx
        .sessions
        .open(Protocol::Tftp, peer, ctx.bind_addr().port());
    let sid = handle.id;
    let start = Instant::now();

    let result = match req.op {
        OP_RRQ => handle_read(&ctx, &socket, &req, sid, &handle).await,
        OP_WRQ => handle_write(&ctx, &socket, &req, sid, &handle).await,
        _ => Ok(0),
    };
    let action = if req.op == OP_RRQ {
        "download"
    } else {
        "upload"
    };
    match result {
        Ok(bytes) => {
            ctx.sessions.close(sid, SessionState::Completed);
            ctx.logger.log(
                Event::new(LogLevel::Info, "tftp", action)
                    .ip(peer.ip())
                    .session(sid)
                    .path(req.filename.clone())
                    .result("ok")
                    .bytes(bytes)
                    .duration_ms(start.elapsed().as_millis() as u64),
            );
            Ok(())
        }
        Err(e) => {
            ctx.sessions.close(sid, SessionState::Failed(e.clone()));
            ctx.logger.log(
                Event::new(LogLevel::Warning, "tftp", action)
                    .ip(peer.ip())
                    .session(sid)
                    .path(req.filename.clone())
                    .result("failed")
                    .error(e.clone()),
            );
            Err(e)
        }
    }
}

fn negotiated_options(req: &Request, tsize: Option<u64>) -> (usize, Vec<(String, String)>) {
    let mut blksize = 512usize;
    let mut oack = Vec::new();
    for (k, v) in &req.options {
        match k.as_str() {
            "blksize" => {
                if let Ok(want) = v.parse::<usize>() {
                    blksize = want.clamp(8, 8192);
                    oack.push(("blksize".into(), blksize.to_string()));
                }
            }
            "tsize" => {
                if let Some(size) = tsize {
                    oack.push(("tsize".into(), size.to_string()));
                } else if req.op == OP_WRQ {
                    // client announced its upload size
                    oack.push(("tsize".into(), v.clone()));
                }
            }
            "timeout" => {
                if v.parse::<u8>().is_ok() {
                    oack.push(("timeout".into(), v.clone()));
                }
            }
            _ => {}
        }
    }
    (blksize, oack)
}

fn oack_packet(opts: &[(String, String)]) -> Vec<u8> {
    let mut pkt = Vec::new();
    pkt.extend_from_slice(&OP_OACK.to_be_bytes());
    for (k, v) in opts {
        pkt.extend_from_slice(k.as_bytes());
        pkt.push(0);
        pkt.extend_from_slice(v.as_bytes());
        pkt.push(0);
    }
    pkt
}

/// Send a packet and wait for the matching ACK (with retransmits).
async fn send_and_await_ack(
    socket: &UdpSocket,
    packet: &[u8],
    expect_block: u16,
) -> Result<(), String> {
    let mut buf = [0u8; 1024];
    for _ in 0..RETRIES {
        socket.send(packet).await.map_err(|e| e.to_string())?;
        match tokio::time::timeout(TIMEOUT, socket.recv(&mut buf)).await {
            Ok(Ok(n)) if n >= 4 => {
                let op = u16::from_be_bytes([buf[0], buf[1]]);
                let block = u16::from_be_bytes([buf[2], buf[3]]);
                if op == OP_ACK && block == expect_block {
                    return Ok(());
                }
                if op == OP_ERROR {
                    let msg = String::from_utf8_lossy(&buf[4..n.saturating_sub(1)]).to_string();
                    return Err(format!("client error: {msg}"));
                }
                // Unexpected/duplicate packet — retransmit.
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_) => {} // timeout — retransmit
        }
    }
    Err("timeout waiting for ACK (client gone?)".into())
}

async fn handle_read(
    ctx: &ServiceCtx,
    socket: &UdpSocket,
    req: &Request,
    sid: u64,
    handle: &Arc<crate::session::SessionHandle>,
) -> Result<u64, String> {
    let abs = match ctx.root.resolve(&req.filename) {
        Ok(p) if p.is_file() => p,
        Ok(_) => {
            send_error(socket, 1, "not a file").await;
            return Err("not a file".into());
        }
        Err(e) => {
            send_error(socket, 1, "file not found").await;
            return Err(e.to_string());
        }
    };
    let meta = tokio::fs::metadata(&abs).await.map_err(|e| e.to_string())?;
    let size = meta.len();
    ctx.sessions.update(sid, |s| {
        s.file = Some(req.filename.clone());
        s.direction = Some(Direction::Download);
        s.total = Some(size);
        s.state = SessionState::Transferring;
    });

    let (blksize, oack) = negotiated_options(req, Some(size));
    if !oack.is_empty() {
        send_and_await_ack(socket, &oack_packet(&oack), 0).await?;
    }

    let mut file = tokio::fs::File::open(&abs)
        .await
        .map_err(|e| e.to_string())?;
    let mut block: u16 = 1;
    let mut sent: u64 = 0;
    loop {
        let mut data = vec![0u8; blksize];
        let mut filled = 0;
        while filled < blksize {
            let n = file
                .read(&mut data[filled..])
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            filled += n;
        }
        data.truncate(filled);

        let mut pkt = Vec::with_capacity(filled + 4);
        pkt.extend_from_slice(&OP_DATA.to_be_bytes());
        pkt.extend_from_slice(&block.to_be_bytes());
        pkt.extend_from_slice(&data);
        tokio::select! { _ = handle.cancelled() => return Err("transfer cancelled".into()), result = send_and_await_ack(socket, &pkt, block) => result? };
        handle.add_bytes(filled as u64);
        sent += filled as u64;
        if filled < blksize {
            return Ok(sent);
        }
        block = block.wrapping_add(1);
    }
}

async fn handle_write(
    ctx: &ServiceCtx,
    socket: &UdpSocket,
    req: &Request,
    sid: u64,
    handle: &Arc<crate::session::SessionHandle>,
) -> Result<u64, String> {
    if !ctx.cfg.uploads.enabled {
        send_error(socket, 2, "uploads are disabled").await;
        return Err("uploads are disabled".into());
    }
    let announced: u64 = req
        .options
        .iter()
        .find(|(k, _)| k == "tsize")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let name = req.filename.rsplit('/').next().unwrap_or("").to_string();
    let mut guard = match crate::services::begin_upload(ctx, &name, announced).await {
        Ok(g) => g,
        Err(e) => {
            send_error(socket, 2, &e).await;
            return Err(e);
        }
    };
    ctx.sessions.update(sid, |s| {
        s.file = Some(req.filename.clone());
        s.direction = Some(Direction::Upload);
        s.total = if announced > 0 { Some(announced) } else { None };
        s.state = SessionState::Transferring;
    });

    let (blksize, oack) = negotiated_options(req, None);
    // Kick off: OACK or ACK 0.
    let first = if oack.is_empty() {
        let mut pkt = Vec::new();
        pkt.extend_from_slice(&OP_ACK.to_be_bytes());
        pkt.extend_from_slice(&0u16.to_be_bytes());
        pkt
    } else {
        oack_packet(&oack)
    };
    socket.send(&first).await.map_err(|e| e.to_string())?;

    let mut file = guard.file.take().expect("fresh upload guard has a file");
    let mut expected: u16 = 1;
    let mut received: u64 = 0;
    let limit = ctx.cfg.uploads.max_upload_mib * 1024 * 1024;
    let mut buf = vec![0u8; blksize + 4 + 64];
    loop {
        let mut got: Option<usize> = None;
        for _ in 0..RETRIES {
            match tokio::time::timeout(TIMEOUT, socket.recv(&mut buf)).await {
                Ok(Ok(n)) if n >= 4 => {
                    let op = u16::from_be_bytes([buf[0], buf[1]]);
                    let block = u16::from_be_bytes([buf[2], buf[3]]);
                    if op == OP_DATA && block == expected {
                        got = Some(n);
                        break;
                    }
                    if op == OP_ERROR {
                        return Err("client aborted upload".into());
                    }
                    // Duplicate DATA: re-ACK the previous block.
                    if op == OP_DATA && block == expected.wrapping_sub(1) {
                        let mut ack = Vec::new();
                        ack.extend_from_slice(&OP_ACK.to_be_bytes());
                        ack.extend_from_slice(&block.to_be_bytes());
                        let _ = socket.send(&ack).await;
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e.to_string()),
                Err(_) => {}
            }
        }
        let Some(n) = got else {
            return Err("timeout waiting for data".into());
        };
        let data = &buf[4..n];
        file.write_all(data).await.map_err(|e| e.to_string())?;
        handle.add_bytes(data.len() as u64);
        received += data.len() as u64;
        if limit > 0 && received > limit {
            send_error(socket, 3, "upload size limit exceeded").await;
            return Err("upload size limit exceeded".into());
        }
        let mut ack = Vec::new();
        ack.extend_from_slice(&OP_ACK.to_be_bytes());
        ack.extend_from_slice(&expected.to_be_bytes());
        socket.send(&ack).await.map_err(|e| e.to_string())?;
        if data.len() < blksize {
            break;
        }
        expected = expected.wrapping_add(1);
    }
    guard.file = Some(file);
    guard.finalize().await?;
    Ok(received)
}
