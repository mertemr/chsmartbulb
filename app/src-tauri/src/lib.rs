//! The app: the web interface in a native window, with the bulb's service running inside.
//!
//! The interface is the same bundle the Python service serves. In the app it talks to
//! the Rust service over Tauri's IPC instead of a WebSocket: `request` carries the
//! socket protocol's requests and `bulb-state` events carry its state events. It can
//! also connect to a Python service on the network, which needs nothing from here.

use std::path::PathBuf;
use std::sync::Arc;

use chsmartbulb_core::service::Options;
use chsmartbulb_core::sim::SimulatedBulb;
use chsmartbulb_core::{Bearer, Bulb, Connector, Service};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::async_runtime::JoinHandle;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_chsmartbulb::{AudioInput, AudioRoute, BluetoothExt, Found, Readiness};
use tokio::sync::Mutex;

mod share;

/// The address that stands for the simulated bulb, for trying the app without one.
const SIMULATED: &str = "SIMULATED";

/// The bulb this app drives.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    address: String,
    name: Option<String>,
    bearer: Bearer,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    device: Option<Device>,
    #[serde(default)]
    audio_input: AudioInput,
    #[serde(default)]
    sharing: share::Sharing,
}

struct Running {
    service: Service,
    forward: JoinHandle<()>,
    shared: Option<share::Shared>,
    share_problem: Option<String>,
}

struct App {
    settings: Mutex<Settings>,
    running: Mutex<Option<Running>>,
}

/// What the interface needs to know before it shows the light.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Setup {
    device: Option<Device>,
    audio_input: AudioInput,
    platform: &'static str,
}

type Reply<T> = Result<T, String>;

fn failed(error: impl ToString) -> String {
    error.to_string()
}

fn settings_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_config_dir().ok().map(|dir| dir.join("settings.json"))
}

fn load_settings(app: &AppHandle) -> Settings {
    settings_path(app)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_settings(app: &AppHandle, settings: &Settings) {
    let Some(path) = settings_path(app) else { return };
    let text = serde_json::to_string_pretty(settings).expect("plain JSON");
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, text));
    if let Err(error) = written {
        log::warn!("cannot save settings to {}: {error}", path.display());
    }
}

/// Build the service for `device` and keep the interface told of its state.
async fn start(
    app: &AppHandle,
    device: &Device,
    input: AudioInput,
    sharing: &share::Sharing,
) -> Result<Running, String> {
    let bluetooth = app.bluetooth();
    let connector: Arc<dyn Connector> = if device.address == SIMULATED {
        let simulated = SimulatedBulb::new();
        simulated.with(|bulb| bulb.channels = [0, 0, 0, 255, 0]); // lit white, as a bulb usually comes on
        simulated.connector()
    } else {
        bluetooth.connector(&device.address, device.bearer).map_err(failed)?
    };
    let state_path = app.path().app_data_dir().ok().map(|dir| dir.join("state.json"));
    let options = Options { state_path, ..Options::default() };
    let mut builder = Service::builder(Bulb::new(connector)).options(options);
    if let Some(capture) = bluetooth.audio_capture(input) {
        builder = builder.audio_capture(capture);
    }
    let service = builder.build();
    service.start();
    let mut subscription = service.subscribe();
    let events = app.clone();
    let forward = tauri::async_runtime::spawn(async move {
        loop {
            let state = subscription.updates.borrow_and_update().clone();
            let _ = events.emit("bulb-state", state);
            if subscription.updates.changed().await.is_err() {
                return;
            }
        }
    });
    let title = device.name.clone().unwrap_or_else(|| "Bulb".into());
    if let Err(error) = bluetooth.keep_running(true, &title).await {
        log::warn!("cannot keep running in the background: {error}");
    }
    let (shared, share_problem) = match (&sharing.token, sharing.enabled) {
        (Some(token), true) => match share::start(&service, token).await {
            Ok(shared) => (Some(shared), None),
            Err(problem) => (None, Some(problem)),
        },
        _ => (None, None),
    };
    Ok(Running { service, forward, shared, share_problem })
}

async fn stop(app: &AppHandle, state: &App) {
    if let Some(running) = state.running.lock().await.take() {
        running.forward.abort();
        drop(running.shared);
        running.service.close().await;
        let _ = app.bluetooth().keep_running(false, "").await;
    }
}

async fn restart(app: &AppHandle, state: &App) -> Reply<()> {
    stop(app, state).await;
    let settings = state.settings.lock().await.clone();
    if let Some(device) = &settings.device {
        let running = start(app, device, settings.audio_input, &settings.sharing).await?;
        *state.running.lock().await = Some(running);
    }
    Ok(())
}

#[tauri::command]
async fn setup(state: State<'_, App>) -> Reply<Setup> {
    let settings = state.settings.lock().await.clone();
    let platform = if cfg!(target_os = "android") {
        "android"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    Ok(Setup { device: settings.device, audio_input: settings.audio_input, platform })
}

#[tauri::command]
async fn prepare(app: AppHandle) -> Reply<Readiness> {
    app.bluetooth().prepare().await.map_err(failed)
}

#[tauri::command]
async fn scan(app: AppHandle, seconds: Option<f64>) -> Reply<Vec<Found>> {
    app.bluetooth().scan(seconds.unwrap_or(4.0)).await.map_err(failed)
}

#[tauri::command]
async fn use_device(app: AppHandle, state: State<'_, App>, device: Device) -> Reply<()> {
    {
        let mut settings = state.settings.lock().await;
        settings.device = Some(device);
        save_settings(&app, &settings);
    }
    restart(&app, &state).await
}

#[tauri::command]
async fn forget_device(app: AppHandle, state: State<'_, App>) -> Reply<()> {
    stop(&app, &state).await;
    let mut settings = state.settings.lock().await;
    settings.device = None;
    save_settings(&app, &settings);
    Ok(())
}

#[tauri::command]
async fn set_audio_input(app: AppHandle, state: State<'_, App>, input: AudioInput) -> Reply<()> {
    {
        let mut settings = state.settings.lock().await;
        if settings.audio_input == input {
            return Ok(());
        }
        settings.audio_input = input;
        save_settings(&app, &settings);
    }
    restart(&app, &state).await // the capture is part of the service
}

/// One request of the socket protocol. `subscribe` answers with the state, which then
/// keeps arriving as `bulb-state` events.
#[tauri::command]
async fn request(state: State<'_, App>, message: Value) -> Reply<Value> {
    let service = match state.running.lock().await.as_ref() {
        Some(running) => running.service.clone(),
        None => return Ok(json!({"ok": false, "error": "no bulb chosen yet"})),
    };
    let mut reply = if message.get("cmd").and_then(Value::as_str) == Some("subscribe") {
        let mut reply = json!({"ok": true});
        if let (Value::Object(reply), Value::Object(fields)) = (&mut reply, service.state()) {
            reply.extend(fields);
        }
        reply
    } else {
        service.handle(&message).await
    };
    if let Some(id) = message.get("id") {
        reply["id"] = id.clone();
    }
    Ok(reply)
}

async fn share_status_of(state: &App) -> share::Status {
    let sharing = state.settings.lock().await.sharing.clone();
    let problem = state.running.lock().await.as_ref().and_then(|running| running.share_problem.clone());
    share::Status {
        enabled: sharing.enabled,
        token: sharing.token,
        addresses: share::addresses(),
        lines_port: chsmartbulb_core::server::LINES_PORT,
        web_port: chsmartbulb_core::server::WEB_PORT,
        problem,
    }
}

#[tauri::command]
async fn share_status(state: State<'_, App>) -> Reply<share::Status> {
    Ok(share_status_of(&state).await)
}

/// Offer the service to the network, or stop; `renew` replaces the token, which turns away
/// every machine that knew the old one.
#[tauri::command]
async fn set_sharing(
    app: AppHandle,
    state: State<'_, App>,
    enabled: bool,
    renew: Option<bool>,
) -> Reply<share::Status> {
    {
        let mut settings = state.settings.lock().await;
        settings.sharing.enabled = enabled;
        if settings.sharing.token.is_none() || renew.unwrap_or(false) {
            settings.sharing.token = Some(share::new_token());
        }
        save_settings(&app, &settings);
    }
    let sharing = state.settings.lock().await.sharing.clone();
    if let Some(running) = state.running.lock().await.as_mut() {
        running.shared = None; // let go of the ports before taking them again
        running.share_problem = None;
        if let (true, Some(token)) = (sharing.enabled, &sharing.token) {
            match share::start(&running.service, token).await {
                Ok(shared) => running.shared = Some(shared),
                Err(problem) => running.share_problem = Some(problem),
            }
        }
    }
    Ok(share_status_of(&state).await)
}

#[tauri::command]
async fn audio_route(app: AppHandle) -> Reply<AudioRoute> {
    app.bluetooth().audio_route().await.map_err(failed)
}

#[tauri::command]
fn open_settings(app: AppHandle, which: String) -> Reply<()> {
    app.bluetooth().open_settings(&which).map_err(failed)
}

fn init_logging() {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("chsmartbulb"),
    );
    #[cfg(not(target_os = "android"))]
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).try_init();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_logging();
    tauri::Builder::default()
        .plugin(tauri_plugin_chsmartbulb::init())
        .setup(|app| {
            let handle = app.handle().clone();
            app.manage(App { settings: Mutex::new(load_settings(&handle)), running: Mutex::default() });
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<App>();
                if let Err(error) = restart(&handle, &state).await {
                    log::warn!("cannot start with the remembered bulb: {error}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            setup,
            prepare,
            scan,
            use_device,
            forget_device,
            set_audio_input,
            request,
            audio_route,
            open_settings,
            share_status,
            set_sharing,
        ])
        .run(tauri::generate_context!())
        .expect("error while running the app");
}
