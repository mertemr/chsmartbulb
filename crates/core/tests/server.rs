//! The service on the network: the line protocol and the WebSocket, as the Python tools use them.

use std::net::SocketAddr;
use std::sync::Arc;

use chsmartbulb_core::server::{self, accept_key, Assets};
use chsmartbulb_core::sim::SimulatedBulb;
use chsmartbulb_core::{Bulb, Service};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

async fn started() -> (Service, SocketAddr, SocketAddr) {
    let service = Service::builder(Bulb::new(SimulatedBulb::new().connector())).build();
    service.attach().await.unwrap();
    let lines = server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let web = server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let (lines_at, web_at) = (lines.local_addr().unwrap(), web.local_addr().unwrap());
    let mut assets = Assets::default();
    assets.insert("index.html", b"<!doctype html><title>Bulb</title>".to_vec());
    assets.insert("assets/app.js", "console.log('bulb');\n".repeat(200).into_bytes());
    tokio::spawn(server::serve_lines(service.clone(), lines, Some("secret".into())));
    tokio::spawn(server::serve_web(service.clone(), web, Arc::new(assets), Some("secret".into())));
    (service, lines_at, web_at)
}

async fn ask(reader: &mut BufReader<TcpStream>, request: Value) -> Value {
    reader.get_mut().write_all(format!("{request}\n").as_bytes()).await.unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn line_clients_authenticate_then_control_and_follow_the_light() {
    let (service, lines, _) = started().await;
    let mut stranger = BufReader::new(TcpStream::connect(lines).await.unwrap());
    assert_eq!(ask(&mut stranger, json!({"cmd": "status"})).await["error"], "not authorised");

    let mut client = BufReader::new(TcpStream::connect(lines).await.unwrap());
    assert_eq!(ask(&mut client, json!({"cmd": "auth", "token": "secret"})).await["ok"], true);
    assert_eq!(ask(&mut client, json!({"cmd": "subscribe"})).await["watchers"], 1);
    service.handle(&json!({"cmd": "color", "color": "#00ff00"})).await;
    let mut line = String::new();
    client.read_line(&mut line).await.unwrap();
    let event: Value = serde_json::from_str(&line).unwrap();
    assert_eq!((event["event"].clone(), event["color"].clone()), (json!("state"), json!("#00ff00")));
}

fn masked(text: &str) -> Vec<u8> {
    let payload = text.as_bytes();
    let mask = [1u8, 2, 3, 4];
    let mut frame = vec![0x81];
    if payload.len() < 126 {
        frame.push(0x80 | payload.len() as u8);
    } else {
        frame.push(0x80 | 127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, byte)| byte ^ mask[i % 4]));
    frame
}

async fn read_text(stream: &mut TcpStream) -> Value {
    let mut head = [0u8; 2];
    stream.read_exact(&mut head).await.unwrap();
    let mut size = (head[1] & 0x7f) as usize;
    if size == 126 {
        let mut bytes = [0u8; 2];
        stream.read_exact(&mut bytes).await.unwrap();
        size = u16::from_be_bytes(bytes) as usize;
    }
    let mut payload = vec![0u8; size];
    stream.read_exact(&mut payload).await.unwrap();
    serde_json::from_slice(&payload).unwrap()
}

#[tokio::test]
async fn the_web_port_serves_the_page_and_the_socket_protocol() {
    let (_service, _, web) = started().await;
    let mut page = TcpStream::connect(web).await.unwrap();
    page.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut answer = String::new();
    page.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 200 OK"), "{answer}");
    assert!(answer.ends_with("<title>Bulb</title>"));

    let mut script = TcpStream::connect(web).await.unwrap();
    script.write_all(b"GET /assets/app.js HTTP/1.1\r\nHost: x\r\nAccept-Encoding: br, gzip\r\n\r\n").await.unwrap();
    let mut answer = Vec::new();
    script.read_to_end(&mut answer).await.unwrap();
    let text = String::from_utf8_lossy(&answer);
    assert!(text.contains("Content-Encoding: gzip"), "{text}");
    assert!(text.contains("immutable"));
    assert!(answer.len() < 1000, "compressed: {} bytes", answer.len());

    let mut wrong = BufReader::new(TcpStream::connect(web).await.unwrap());
    wrong.get_mut().write_all(b"{\"cmd\": \"status\"}\n").await.unwrap();
    let mut line = String::new();
    wrong.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("HTTP/"), "a line client is told it reached the web port");

    let mut socket = TcpStream::connect(web).await.unwrap();
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let handshake = format!(
        "GET /ws HTTP/1.1\r\nHost: {web}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\nOrigin: http://{web}\r\n\r\n"
    );
    socket.write_all(handshake.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(socket.read_u8().await.unwrap());
    }
    let head = String::from_utf8(head).unwrap();
    assert!(head.starts_with("HTTP/1.1 101"), "{head}");
    assert!(head.contains(&accept_key(key)));
    socket.write_all(&masked(r#"{"cmd": "auth", "token": "secret", "id": 1}"#)).await.unwrap();
    assert_eq!(read_text(&mut socket).await, json!({"ok": true, "id": 1}));
    socket.write_all(&masked(r#"{"cmd": "subscribe", "id": 2}"#)).await.unwrap();
    let subscribed = read_text(&mut socket).await;
    assert_eq!((subscribed["ok"].clone(), subscribed["connected"].clone()), (json!(true), json!(true)));

    // too big a message ends the socket with a reason the client gets to read
    let mut greedy = TcpStream::connect(web).await.unwrap();
    greedy.write_all(handshake.as_bytes()).await.unwrap();
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(greedy.read_u8().await.unwrap());
    }
    greedy.write_all(&masked(&"x".repeat(70000))).await.unwrap();
    let mut close = [0u8; 4];
    greedy.read_exact(&mut close).await.unwrap();
    assert_eq!(close, [0x88, 2, 0x03, 0xf1], "close frame with 1009");

    let mut foreign = TcpStream::connect(web).await.unwrap();
    let elsewhere = handshake.replace(&format!("Origin: http://{web}"), "Origin: http://evil.example");
    foreign.write_all(elsewhere.as_bytes()).await.unwrap();
    let mut answer = String::new();
    foreign.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
}
