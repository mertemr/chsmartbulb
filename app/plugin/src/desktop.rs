//! Linux and Windows, through `chsmartbulb-desktop`: BLE through btleplug, SPP through an RFCOMM socket.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use btleplug::api::{Central, Peripheral as _, ScanFilter};
use chsmartbulb_core::service::AudioCapture;
use chsmartbulb_core::{Bearer, Connector};
use serde::de::DeserializeOwned;
use tauri::plugin::PluginApi;
use tauri::{AppHandle, Runtime};

pub use chsmartbulb_desktop::presence::Watcher as PresenceWatcher;
use chsmartbulb_desktop::{adapter, presence};

use crate::models::{likely, AudioInput, AudioRoute, Found, Presence, Readiness};
use crate::{Error, Result};

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<Bluetooth<R>> {
    Ok(Bluetooth { runtime: PhantomData })
}

pub struct Bluetooth<R: Runtime> {
    runtime: PhantomData<fn() -> R>,
}

impl<R: Runtime> Bluetooth<R> {
    /// Nothing to ask for on the desktop; the adapter is either there or not.
    pub async fn prepare(&self) -> Result<Readiness> {
        let enabled = adapter().await.is_ok();
        Ok(Readiness { granted: true, enabled })
    }

    /// Devices the system knows and those advertising nearby.
    pub async fn scan(&self, seconds: f64) -> Result<Vec<Found>> {
        let adapter = adapter().await?;
        adapter.start_scan(ScanFilter::default()).await.map_err(|e| Error::Bluetooth(e.to_string()))?;
        tokio::time::sleep(Duration::from_secs_f64(seconds.clamp(0.5, 30.0))).await;
        let _ = adapter.stop_scan().await;
        let mut found = Vec::new();
        for peripheral in adapter.peripherals().await.map_err(|e| Error::Bluetooth(e.to_string()))? {
            let Ok(Some(properties)) = peripheral.properties().await else { continue };
            let address = properties.address.to_string();
            let name = properties.local_name.clone();
            let likely = likely(name.as_deref(), &address, properties.manufacturer_data.keys().copied());
            found.push(Found {
                le: properties.rssi.is_some(),
                classic: likely || properties.rssi.is_none(), // BlueZ also lists paired Classic devices
                bonded: properties.rssi.is_none(),
                rssi: properties.rssi,
                address,
                name,
                likely,
            });
        }
        found.sort_by_key(|f| (!f.likely, f.name.is_none(), -(f.rssi.unwrap_or(-200) as i32)));
        Ok(found)
    }

    pub fn connector(&self, address: &str, bearer: Bearer) -> Result<Arc<dyn Connector>> {
        Ok(chsmartbulb_desktop::connector(address, bearer)?)
    }

    /// What this computer plays (a loopback of the default output) or its default input.
    pub fn audio_capture(&self, input: AudioInput) -> Option<Arc<dyn AudioCapture>> {
        chsmartbulb_desktop::audio_capture(input == AudioInput::Microphone, None)
    }

    pub async fn audio_route(&self) -> Result<AudioRoute> {
        Ok(AudioRoute { can_capture_playback: chsmartbulb_desktop::CAN_CAPTURE_PLAYBACK, ..AudioRoute::default() })
    }

    /// Report the computer being locked, put to sleep or shut down, until the watcher is dropped.
    pub async fn watch_presence(&self, on_event: Arc<dyn Fn(Presence) + Send + Sync>) -> Result<PresenceWatcher> {
        Ok(presence::watch(on_event).await?)
    }

    /// Desktop apps keep running in the background anyway.
    pub async fn keep_running(&self, _running: bool, _title: &str) -> Result<()> {
        Ok(())
    }

    pub fn open_settings(&self, _which: &str) -> Result<()> {
        Err(Error::Unsupported("open the system settings from the desktop".into()))
    }
}
