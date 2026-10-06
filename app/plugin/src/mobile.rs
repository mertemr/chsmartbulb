//! Android: the Kotlin side of the plugin holds the Bluetooth links and the audio capture.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use chsmartbulb_core::audio::{Analyzer, AudioSource};
use chsmartbulb_core::service::AudioCapture;
use chsmartbulb_core::transport::{ChannelLink, LinkFeed, Writer};
use chsmartbulb_core::{Bearer, Connector, Link};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::plugin::{PluginApi, PluginHandle};
use tauri::{AppHandle, Runtime};

use crate::models::{AudioInput, AudioRoute, Found, Readiness};
use crate::Result;

const PLUGIN_PACKAGE: &str = "io.github.mertemr.chsmartbulb.bluetooth";

pub fn init<R: Runtime, C: DeserializeOwned>(_app: &AppHandle<R>, api: PluginApi<R, C>) -> Result<Bluetooth<R>> {
    #[cfg(target_os = "android")]
    let handle = api.register_android_plugin(PLUGIN_PACKAGE, "BluetoothPlugin")?;
    #[cfg(target_os = "ios")]
    let handle: PluginHandle<R> = {
        let _ = api;
        return Err(crate::Error::Unsupported("iOS is not supported".into()));
    };
    Ok(Bluetooth { handle })
}

pub struct Bluetooth<R: Runtime> {
    handle: PluginHandle<R>,
}

/// A message the Kotlin side sends through a channel.
#[derive(Deserialize)]
struct Event {
    event: String,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    rate: Option<u32>,
    #[serde(default)]
    channels: Option<usize>,
    #[serde(default)]
    reason: Option<String>,
}

fn event(body: InvokeResponseBody) -> Option<Event> {
    match body {
        InvokeResponseBody::Json(text) => serde_json::from_str(&text).ok(),
        InvokeResponseBody::Raw(bytes) => serde_json::from_slice(&bytes).ok(),
    }
}

impl<R: Runtime> Bluetooth<R> {
    /// Ask for the Bluetooth permissions and to switch Bluetooth on, as needed.
    pub async fn prepare(&self) -> Result<Readiness> {
        Ok(self.handle.run_mobile_plugin_async("prepare", json!({})).await?)
    }

    /// Bonded devices and those advertising over BLE nearby.
    pub async fn scan(&self, seconds: f64) -> Result<Vec<Found>> {
        #[derive(Deserialize)]
        struct Scanned {
            devices: Vec<Found>,
        }
        let scanned: Scanned = self.handle.run_mobile_plugin_async("scan", json!({ "seconds": seconds })).await?;
        Ok(scanned.devices)
    }

    pub fn connector(&self, address: &str, bearer: Bearer) -> Result<Arc<dyn Connector>> {
        Ok(Arc::new(AndroidConnector { handle: self.handle.clone(), address: address.to_ascii_uppercase(), bearer }))
    }

    pub fn audio_capture(&self, input: AudioInput) -> Option<Arc<dyn AudioCapture>> {
        Some(Arc::new(AndroidAudio { handle: self.handle.clone(), input }))
    }

    pub async fn audio_route(&self) -> Result<AudioRoute> {
        Ok(self.handle.run_mobile_plugin_async("audioRoute", json!({})).await?)
    }

    /// Open a system settings screen: `bluetooth` or `sound`.
    pub fn open_settings(&self, which: &str) -> Result<()> {
        let _: Value = self.handle.run_mobile_plugin("openSettings", json!({ "which": which }))?;
        Ok(())
    }

    /// Keep the app running with a notification while it drives the bulb, so effects survive the screen going off.
    pub async fn keep_running(&self, running: bool, title: &str) -> Result<()> {
        let _: Value =
            self.handle.run_mobile_plugin_async("keepRunning", json!({ "running": running, "title": title })).await?;
        Ok(())
    }
}

struct AndroidConnector<R: Runtime> {
    handle: PluginHandle<R>,
    address: String,
    bearer: Bearer,
}

struct AndroidWriter<R: Runtime> {
    handle: PluginHandle<R>,
}

#[async_trait]
impl<R: Runtime> Writer for AndroidWriter<R> {
    async fn write(&self, data: &[u8]) -> chsmartbulb_core::Result<()> {
        self.handle
            .run_mobile_plugin_async::<Value>("write", json!({ "data": STANDARD.encode(data) }))
            .await
            .map(drop)
            .map_err(|e| chsmartbulb_core::Error::Transport(e.to_string()))
    }

    async fn close(&self) {
        let _ = self.handle.run_mobile_plugin_async::<Value>("disconnect", json!({})).await;
    }
}

fn link_channel(feed: LinkFeed) -> Channel<Value> {
    Channel::new(move |body| {
        match event(body) {
            Some(Event { event, data: Some(data), .. }) if event == "data" => {
                if let Ok(bytes) = STANDARD.decode(data) {
                    feed.push(bytes);
                }
            }
            Some(Event { event, reason, .. }) if event == "closed" => {
                log::info!("link closed: {}", reason.unwrap_or_default());
                feed.closed();
            }
            _ => {}
        }
        Ok(())
    })
}

#[async_trait]
impl<R: Runtime> Connector for AndroidConnector<R> {
    async fn connect(&self) -> chsmartbulb_core::Result<Arc<dyn Link>> {
        let (link, feed) = ChannelLink::new(Box::new(AndroidWriter { handle: self.handle.clone() }));
        let channel = link_channel(feed);
        let bearer = match self.bearer {
            Bearer::Ble => "ble",
            Bearer::Spp => "spp",
        };
        let request = json!({ "address": self.address, "bearer": bearer, "onEvent": channel });
        match self.handle.run_mobile_plugin_async::<Value>("connect", request).await {
            Ok(_) => Ok(link as Arc<dyn Link>),
            Err(error) => Err(chsmartbulb_core::Error::ConnectionFailed(error.to_string())),
        }
    }

    fn describe(&self) -> String {
        let bearer = match self.bearer {
            Bearer::Ble => "BLE",
            Bearer::Spp => "SPP",
        };
        format!("{} over {bearer}", self.address)
    }

    fn bearer(&self) -> Bearer {
        self.bearer
    }
}

struct AndroidAudio<R: Runtime> {
    handle: PluginHandle<R>,
    input: AudioInput,
}

#[async_trait]
impl<R: Runtime> AudioCapture for AndroidAudio<R> {
    async fn start(&self, source: Arc<AudioSource>) -> chsmartbulb_core::Result<()> {
        let mic = self.input == AudioInput::Microphone;
        let analyzer: Arc<Mutex<Option<Analyzer>>> = Arc::default();
        let channel: Channel<Value> = Channel::new(move |body| {
            let Some(Event { event, data: Some(data), rate, channels, .. }) = event(body) else { return Ok(()) };
            if event != "pcm" {
                return Ok(());
            }
            let Ok(pcm) = STANDARD.decode(data) else { return Ok(()) };
            let (rate, channels) = (rate.unwrap_or(44100), channels.unwrap_or(1));
            let mut analyzer = analyzer.lock().unwrap_or_else(|p| p.into_inner());
            let fits = analyzer.as_ref().is_some_and(|a| a.rate() == rate && a.channels() == channels);
            if !fits {
                *analyzer = Some(Analyzer::new(rate, channels, mic, None));
            }
            if let Some(analyzer) = analyzer.as_mut() {
                for (levels, onset) in analyzer.feed_bytes(&pcm) {
                    source.publish(levels, onset);
                }
            }
            Ok(())
        });
        let input = match self.input {
            AudioInput::Playback => "playback",
            AudioInput::Microphone => "microphone",
        };
        self.handle
            .run_mobile_plugin_async::<Value>("startAudio", json!({ "input": input, "onEvent": channel }))
            .await
            .map(drop)
            .map_err(|e| chsmartbulb_core::Error::Invalid(format!("cannot listen to the {input}: {e}")))
    }

    async fn stop(&self) {
        let _ = self.handle.run_mobile_plugin_async::<Value>("stopAudio", json!({})).await;
    }
}
