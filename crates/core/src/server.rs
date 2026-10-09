//! The service on the network, as `chsmartbulb daemon --listen PORT --web PORT` offers it.
//!
//! The listeners all speak the socket protocol through a [`Session`]:
//!
//! - [`serve_lines`]: one JSON object per line over TCP, for the command line and the
//!   audio and screen agents of other machines (usually port 8377);
//! - [`serve_local`]: the same lines over a Unix socket, for this machine's command line;
//! - [`serve_web`]: the web interface's files and the same objects over a WebSocket at
//!   `/ws` (usually port 8378).
//!
//! A port of `chsmartbulb.web` and the TCP part of `chsmartbulb.service`, so the Python
//! tools cannot tell the app from the Python service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

use crate::service::{Service, Session};

pub const LINES_PORT: u16 = 8377;
pub const WEB_PORT: u16 = 8378;

/// Seconds between pings on a WebSocket.
const PING_INTERVAL: Duration = Duration::from_secs(20);
/// A client silent for this long, pongs included, is gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
/// Seconds to wait for a closing client to hang up.
const LINGER: Duration = Duration::from_secs(1);
const MAX_HEADER: usize = 8192;
const MAX_MESSAGE: usize = 65536;
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const TEXT: u8 = 0x1;
const CLOSE: u8 = 0x8;
const PING: u8 = 0x9;
const PONG: u8 = 0xA;
const PROTOCOL_ERROR: u16 = 1002;
const TOO_BIG: u16 = 1009;

const SECURITY: [(&str, &str); 3] = [
    ("X-Content-Type-Options", "nosniff"),
    ("Referrer-Policy", "no-referrer"),
    (
        "Content-Security-Policy",
        "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
         connect-src 'self' ws: wss:; frame-ancestors 'none'",
    ),
];

/// One file of the web interface.
pub struct Asset {
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Kept only where it is smaller.
    gzipped: Option<Vec<u8>>,
    etag: String,
    /// The name carries a hash of the content.
    immutable: bool,
}

/// The web interface's files, keyed by their path in a URL (without the leading slash).
#[derive(Default)]
pub struct Assets {
    files: HashMap<String, Asset>,
}

impl Assets {
    pub fn insert(&mut self, name: &str, body: Vec<u8>) {
        let extension = name.rsplit('.').next().unwrap_or_default();
        let content_type = match extension {
            "html" => "text/html; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "css" => "text/css; charset=utf-8",
            "json" => "application/json",
            "webmanifest" => "application/manifest+json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff2" => "font/woff2",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        let etag = format!("\"{}\"", &sha1_smol::Sha1::from(&body).digest().to_string()[..16]);
        let immutable = name.starts_with("assets/");
        let gzipped = gzip(&body).filter(|packed| packed.len() < body.len());
        self.files.insert(name.to_string(), Asset { content_type, body, gzipped, etag, immutable });
    }

    pub fn is_empty(&self) -> bool {
        !self.files.contains_key("index.html")
    }
}

fn gzip(body: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write as _;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(body).ok()?;
    encoder.finish().ok()
}

/// The proof a WebSocket server gives that it read the client's handshake key.
pub fn accept_key(key: &str) -> String {
    STANDARD.encode(sha1_smol::Sha1::from(format!("{key}{WEBSOCKET_GUID}")).digest().bytes())
}

/// Bind `address`, saying clearly when it is taken.
pub async fn bind(address: SocketAddr) -> std::io::Result<TcpListener> {
    TcpListener::bind(address).await
}

/// Accept clients of the line protocol until the task is dropped.
pub async fn serve_lines(service: Service, listener: TcpListener, token: Option<String>) {
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let session = Session::new(service.clone(), token.clone());
        tokio::spawn(async move {
            log::debug!("line client {peer} connected");
            lines(stream, session).await;
        });
    }
}

/// Accept this machine's clients of the line protocol, which need no token, until the task is dropped.
#[cfg(unix)]
pub async fn serve_local(service: Service, listener: tokio::net::UnixListener) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else { continue };
        let session = Session::new(service.clone(), None);
        tokio::spawn(lines(stream, session));
    }
}

async fn lines(stream: impl AsyncRead + AsyncWrite + Unpin, mut session: Session) {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let next = async {
            let (updates, told) = session.watch();
            match updates {
                Some(updates) => tokio::select! {
                    read = reader.read_line(&mut line) => LineIncoming::Read(read),
                    changed = updates.changed() => match changed {
                        Ok(()) => LineIncoming::State(updates.borrow_and_update().clone()),
                        Err(_) => LineIncoming::Read(Ok(0)),
                    },
                    Some(event) = told.recv() => LineIncoming::Told(event),
                },
                None => tokio::select! {
                    read = reader.read_line(&mut line) => LineIncoming::Read(read),
                    Some(event) = told.recv() => LineIncoming::Told(event),
                },
            }
        };
        match next.await {
            LineIncoming::Read(Ok(0) | Err(_)) => break,
            LineIncoming::Read(Ok(_)) => {
                let outcome = session.receive(line.trim_end()).await;
                if let Some(reply) = outcome.reply {
                    if send_line(&mut writer, &reply).await.is_err() {
                        break;
                    }
                }
                if outcome.close {
                    break;
                }
            }
            LineIncoming::State(state) => {
                if send_line(&mut writer, &event_message(state)).await.is_err() {
                    break;
                }
            }
            LineIncoming::Told(event) => {
                if send_line(&mut writer, &event).await.is_err() {
                    break;
                }
            }
        }
    }
    session.close().await;
}

enum LineIncoming {
    Read(std::io::Result<usize>),
    State(Value),
    Told(Value),
}

fn event_message(state: Value) -> Value {
    let mut message = json!({"event": "state"});
    if let (Value::Object(message), Value::Object(state)) = (&mut message, state) {
        message.extend(state);
    }
    message
}

async fn send_line(writer: &mut (impl AsyncWrite + Unpin), message: &Value) -> std::io::Result<()> {
    let mut text = message.to_string();
    text.push('\n');
    writer.write_all(text.as_bytes()).await
}

/// Serve the web interface's files and its WebSocket until the task is dropped.
pub async fn serve_web(service: Service, listener: TcpListener, assets: Arc<Assets>, token: Option<String>) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else { continue };
        let service = service.clone();
        let assets = assets.clone();
        let token = token.clone();
        tokio::spawn(async move {
            let _ = web(stream, service, assets, token).await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    headers: HashMap<String, String>,
}

async fn read_head(reader: &mut BufReader<TcpStream>) -> Result<Option<Request>, u16> {
    let mut head = Vec::new();
    loop {
        let before = head.len();
        let read = reader.read_until(b'\n', &mut head).await.map_err(|_| 400u16)?;
        if read == 0 {
            return if head.is_empty() { Ok(None) } else { Err(400) };
        }
        if before == 0 && head.first() == Some(&b'{') {
            // a client of the line protocol on the wrong port: its line has been read, so the answer arrives
            return Err(400);
        }
        if head.len() > MAX_HEADER {
            return Err(431);
        }
        if head.ends_with(b"\r\n\r\n") || head.ends_with(b"\n\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head);
    let mut lines = text.split("\r\n").filter(|line| !line.is_empty());
    let first: Vec<&str> = lines.next().unwrap_or_default().split(' ').collect();
    if first.len() != 3 || !first[1].starts_with('/') {
        return Err(400);
    }
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    let path = first[1].split('?').next().unwrap_or("/").to_string();
    Ok(Some(Request { method: first[0].to_string(), path, headers }))
}

fn reason(status: u16) -> &'static str {
    match status {
        101 => "Switching Protocols",
        200 => "OK",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Request Header Fields Too Large",
    }
}

async fn respond(
    stream: &mut (impl AsyncWrite + Unpin),
    status: u16,
    fields: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<()> {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    if status != 101 {
        head.push_str(&format!("Content-Length: {}\r\nConnection: close\r\n", body.len()));
    }
    for (name, value) in fields {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

async fn web(stream: TcpStream, service: Service, assets: Arc<Assets>, token: Option<String>) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let request = match tokio::time::timeout(HEADER_TIMEOUT, read_head(&mut reader)).await {
        Ok(Ok(Some(request))) => request,
        Ok(Ok(None)) | Err(_) => return Ok(()),
        Ok(Err(status)) => return respond(reader.get_mut(), status, &[], b"").await,
    };
    if request.path == "/ws" {
        return websocket(reader, request, service, token).await;
    }
    if request.method != "GET" && request.method != "HEAD" {
        return respond(reader.get_mut(), 405, &[("Allow", "GET, HEAD")], b"").await;
    }
    let name = request.path.trim_start_matches('/');
    let Some(asset) = assets.files.get(if name.is_empty() { "index.html" } else { name }) else {
        return respond(reader.get_mut(), 404, &[], b"").await;
    };
    let caching = if asset.immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
    let mut fields = vec![("ETag", asset.etag.as_str()), ("Cache-Control", caching), ("Vary", "Accept-Encoding")];
    fields.extend(SECURITY);
    if request.headers.get("if-none-match") == Some(&asset.etag) {
        return respond(reader.get_mut(), 304, &fields, b"").await;
    }
    fields.push(("Content-Type", asset.content_type));
    let accepts_gzip = request.headers.get("accept-encoding").is_some_and(|codings| {
        codings.split(',').any(|coding| coding.split(';').next().unwrap_or_default().trim() == "gzip")
    });
    let mut body: &[u8] = &asset.body;
    if let (true, Some(packed)) = (accepts_gzip, &asset.gzipped) {
        body = packed;
        fields.push(("Content-Encoding", "gzip"));
    }
    if request.method != "GET" {
        body = b"";
    }
    respond(reader.get_mut(), 200, &fields, body).await
}

async fn websocket(
    mut reader: BufReader<TcpStream>,
    request: Request,
    service: Service,
    token: Option<String>,
) -> std::io::Result<()> {
    let headers = &request.headers;
    let key = headers.get("sec-websocket-key").cloned().unwrap_or_default();
    let upgrade = headers.get("upgrade").is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if !upgrade || key.is_empty() || headers.get("sec-websocket-version").map(String::as_str) != Some("13") {
        return respond(reader.get_mut(), 400, &[], b"").await;
    }
    if let Some(origin) = headers.get("origin") {
        // a page from elsewhere, open in a browser on this network, must not reach the bulb
        let host = origin.split("://").nth(1).unwrap_or_default();
        if Some(&host.to_string()) != headers.get("host") {
            return respond(reader.get_mut(), 403, &[], b"").await;
        }
    }
    let accept = accept_key(&key);
    let fields = [("Upgrade", "websocket"), ("Connection", "Upgrade"), ("Sec-WebSocket-Accept", accept.as_str())];
    respond(reader.get_mut(), 101, &fields, b"").await?;

    let (read_half, mut write_half) = reader.into_inner().into_split();
    let mut frames = BufReader::new(read_half);
    let mut session = Session::new(service, token);
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.tick().await;
    let code = loop {
        let next = async {
            let (updates, told) = session.watch();
            match updates {
                Some(updates) => tokio::select! {
                    message = receive(&mut frames) => Incoming::Message(message),
                    changed = updates.changed() => match changed {
                        Ok(()) => Incoming::State(updates.borrow_and_update().clone()),
                        Err(_) => Incoming::Message(Ok(None)),
                    },
                    Some(event) = told.recv() => Incoming::Told(event),
                    _ = ping.tick() => Incoming::Ping,
                },
                None => tokio::select! {
                    message = receive(&mut frames) => Incoming::Message(message),
                    Some(event) = told.recv() => Incoming::Told(event),
                    _ = ping.tick() => Incoming::Ping,
                },
            }
        };
        match next.await {
            Incoming::Ping => {
                if frame(&mut write_half, PING, b"").await.is_err() {
                    break None;
                }
            }
            Incoming::State(state) => {
                if send_text(&mut write_half, &event_message(state)).await.is_err() {
                    break None;
                }
            }
            Incoming::Told(event) => {
                if send_text(&mut write_half, &event).await.is_err() {
                    break None;
                }
            }
            Incoming::Message(Ok(Some(Received::Text(text)))) => {
                let outcome = session.receive(&text).await;
                if let Some(reply) = outcome.reply {
                    if send_text(&mut write_half, &reply).await.is_err() {
                        break None;
                    }
                }
                if outcome.close {
                    break Some(1000);
                }
            }
            Incoming::Message(Ok(Some(Received::Ping(payload)))) => {
                if frame(&mut write_half, PONG, &payload).await.is_err() {
                    break None;
                }
            }
            Incoming::Message(Ok(None)) => break Some(1000),
            Incoming::Message(Err(Closing(code))) => break code,
        }
    };
    session.close().await;
    if let Some(code) = code {
        let _ = frame(&mut write_half, CLOSE, &code.to_be_bytes()).await;
        // Closing with the client's data unread resets the connection, and the reset can overtake
        // the close frame; so take what is still coming, briefly, until the client hangs up.
        let mut sink = [0u8; 4096];
        let _ = tokio::time::timeout(LINGER, async {
            while matches!(frames.read(&mut sink).await, Ok(size) if size > 0) {}
        })
        .await;
    }
    Ok(())
}

enum Incoming {
    Message(Result<Option<Received>, Closing>),
    State(Value),
    Told(Value),
    Ping,
}

enum Received {
    Text(String),
    Ping(Vec<u8>),
}

/// The WebSocket has to end; with a close code, or silently when the client is gone.
struct Closing(Option<u16>);

async fn frame(writer: &mut (impl AsyncWrite + Unpin), opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut data = vec![0x80 | opcode];
    match payload.len() {
        size if size < 126 => data.push(size as u8),
        size if size < 0x10000 => {
            data.push(126);
            data.extend_from_slice(&(size as u16).to_be_bytes());
        }
        size => {
            data.push(127);
            data.extend_from_slice(&(size as u64).to_be_bytes());
        }
    }
    data.extend_from_slice(payload);
    writer.write_all(&data).await
}

async fn send_text(writer: &mut (impl AsyncWrite + Unpin), message: &Value) -> std::io::Result<()> {
    frame(writer, TEXT, message.to_string().as_bytes()).await
}

/// The next text message or ping, or `None` once the client has closed.
async fn receive(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<Received>, Closing> {
    let mut message = Vec::new();
    loop {
        let mut head = [0u8; 2];
        match tokio::time::timeout(IDLE_TIMEOUT, reader.read_exact(&mut head)).await {
            Ok(Ok(_)) => {}
            _ => return Err(Closing(None)), // gone without a word
        }
        let [first, second] = head;
        if second & 0x80 == 0 {
            return Err(Closing(Some(PROTOCOL_ERROR))); // a client must mask what it sends
        }
        let mut size = u64::from(second & 0x7f);
        if size == 126 {
            let mut bytes = [0u8; 2];
            reader.read_exact(&mut bytes).await.map_err(|_| Closing(None))?;
            size = u64::from(u16::from_be_bytes(bytes));
        } else if size == 127 {
            let mut bytes = [0u8; 8];
            reader.read_exact(&mut bytes).await.map_err(|_| Closing(None))?;
            size = u64::from_be_bytes(bytes);
        }
        if message.len() as u64 + size > MAX_MESSAGE as u64 {
            return Err(Closing(Some(TOO_BIG)));
        }
        let mut mask = [0u8; 4];
        reader.read_exact(&mut mask).await.map_err(|_| Closing(None))?;
        let mut payload = vec![0u8; size as usize];
        reader.read_exact(&mut payload).await.map_err(|_| Closing(None))?;
        for (i, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[i % 4];
        }
        match first & 0x0f {
            CLOSE => return Ok(None),
            PING => return Ok(Some(Received::Ping(payload))),
            PONG => continue,
            _ => {
                message.extend_from_slice(&payload);
                if first & 0x80 != 0 {
                    // the last fragment
                    return Ok(Some(Received::Text(String::from_utf8_lossy(&message).into_owned())));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_matches_the_rfc_example() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }
}
