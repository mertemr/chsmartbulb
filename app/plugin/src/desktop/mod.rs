//! Linux and Windows: BLE through btleplug, SPP through an RFCOMM socket.

use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager};
use chsmartbulb_core::service::AudioCapture;
use chsmartbulb_core::{Bearer, Connector};
use serde::de::DeserializeOwned;
use tauri::plugin::PluginApi;
use tauri::{AppHandle, Runtime};

use crate::models::{likely, AudioInput, AudioRoute, Found, Readiness};
use crate::{Error, Result};

mod ble;
#[cfg(target_os = "linux")]
mod rfcomm_linux;
#[cfg(windows)]
mod rfcomm_windows;

pub fn init<R: Runtime, C: DeserializeOwned>(
    _app: &AppHandle<R>,
    _api: PluginApi<R, C>,
) -> crate::Result<Bluetooth<R>> {
    Ok(Bluetooth { runtime: PhantomData })
}

pub struct Bluetooth<R: Runtime> {
    runtime: PhantomData<fn() -> R>,
}

pub(crate) async fn adapter() -> Result<Adapter> {
    let manager = Manager::new().await.map_err(|e| Error::Bluetooth(format!("no Bluetooth here: {e}")))?;
    let adapters = manager.adapters().await.map_err(|e| Error::Bluetooth(e.to_string()))?;
    adapters.into_iter().next().ok_or_else(|| Error::Bluetooth("no Bluetooth adapter found".into()))
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
        match bearer {
            Bearer::Ble => Ok(Arc::new(ble::BleConnector::new(address))),
            #[cfg(target_os = "linux")]
            Bearer::Spp => Ok(Arc::new(rfcomm_linux::RfcommConnector::new(address)?)),
            #[cfg(windows)]
            Bearer::Spp => Ok(Arc::new(rfcomm_windows::RfcommConnector::new(address)?)),
            #[cfg(not(any(target_os = "linux", windows)))]
            Bearer::Spp => Err(Error::Unsupported("SPP is not available on this platform; use BLE".into())),
        }
    }

    /// Desktop sound capture is the Python agent's job for now.
    pub fn audio_capture(&self, _input: AudioInput) -> Option<Arc<dyn AudioCapture>> {
        None
    }

    pub async fn audio_route(&self) -> Result<AudioRoute> {
        Ok(AudioRoute::default())
    }

    /// Desktop apps keep running in the background anyway.
    pub async fn keep_running(&self, _running: bool, _title: &str) -> Result<()> {
        Ok(())
    }

    pub fn open_settings(&self, _which: &str) -> Result<()> {
        Err(Error::Unsupported("open the system settings from the desktop".into()))
    }
}
