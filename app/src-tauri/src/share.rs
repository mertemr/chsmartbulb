//! Offering the app's service to the network, as `chsmartbulb daemon --listen --web` does:
//! other machines' browsers, the command line and the audio and screen agents reach the
//! bulb through this device.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, OnceLock};

use chsmartbulb_core::server::{self, Assets, LINES_PORT, WEB_PORT};
use chsmartbulb_core::Service;
use serde::{Deserialize, Serialize};
use tauri::async_runtime::JoinHandle;

mod embedded {
    include!(concat!(env!("OUT_DIR"), "/webui.rs"));
}

/// What the settings remember about sharing.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sharing {
    pub enabled: bool,
    pub token: Option<String>,
}

/// The listeners while sharing is on.
pub struct Shared {
    tasks: Vec<JoinHandle<()>>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// What the interface shows about sharing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub enabled: bool,
    pub token: Option<String>,
    pub addresses: Vec<String>,
    pub lines_port: u16,
    pub web_port: u16,
    pub problem: Option<String>,
}

fn assets() -> Arc<Assets> {
    static ASSETS: OnceLock<Arc<Assets>> = OnceLock::new();
    ASSETS
        .get_or_init(|| {
            let mut assets = Assets::default();
            for (name, body) in embedded::FILES {
                assets.insert(name, body.to_vec());
            }
            Arc::new(assets)
        })
        .clone()
}

/// A fresh secret for other machines to present.
pub fn new_token() -> String {
    let mut bytes = [0u8; 12];
    getrandom::fill(&mut bytes).expect("the system has a random source");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The address this device has on the local network, as other machines would use it.
pub fn addresses() -> Vec<String> {
    // routing a datagram socket picks the outgoing interface; nothing is sent
    let local = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|socket| socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).map(|()| socket))
        .and_then(|socket| socket.local_addr());
    match local {
        Ok(address) if !address.ip().is_unspecified() => vec![address.ip().to_string()],
        _ => Vec::new(),
    }
}

/// Listen on every interface; fails when a port is taken, for example by a Python service.
pub async fn start(service: &Service, token: &str) -> Result<Shared, String> {
    let any = |port| SocketAddr::from((Ipv4Addr::UNSPECIFIED, port));
    let lines = server::bind(any(LINES_PORT)).await.map_err(|e| format!("cannot listen on port {LINES_PORT}: {e}"))?;
    let web = server::bind(any(WEB_PORT)).await.map_err(|e| format!("cannot listen on port {WEB_PORT}: {e}"))?;
    let token = Some(token.to_string());
    let tasks = vec![
        tauri::async_runtime::spawn(server::serve_lines(service.clone(), lines, token.clone())),
        tauri::async_runtime::spawn(server::serve_web(service.clone(), web, assets(), token)),
    ];
    log::info!("sharing the bulb on ports {LINES_PORT} and {WEB_PORT}");
    Ok(Shared { tasks })
}
