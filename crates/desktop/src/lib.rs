//! What the chsmartbulb service needs from a Linux or Windows computer, shared by the
//! app and the daemon: BLE through btleplug, SPP through an RFCOMM socket, the sound
//! the computer plays, what its screen shows, and the computer being locked or asleep.

use std::sync::Arc;

use btleplug::api::Manager as _;
use btleplug::platform::{Adapter, Manager};
use chsmartbulb_core::service::{AudioCapture, ScreenCapture};
use chsmartbulb_core::{Bearer, Connector};

#[cfg(any(target_os = "linux", windows))]
mod audio;
mod ble;
mod error;
#[cfg(all(target_os = "linux", feature = "portal"))]
mod portal;
pub mod presence;
#[cfg(target_os = "linux")]
mod rfcomm_linux;
#[cfg(windows)]
mod rfcomm_windows;
#[cfg(any(target_os = "linux", windows))]
mod screen;

pub use error::{Error, Result};

/// The system's Bluetooth adapter, for scanning and BLE.
pub async fn adapter() -> Result<Adapter> {
    let manager = Manager::new().await.map_err(|e| Error::Bluetooth(format!("no Bluetooth here: {e}")))?;
    let adapters = manager.adapters().await.map_err(|e| Error::Bluetooth(e.to_string()))?;
    adapters.into_iter().next().ok_or_else(|| Error::Bluetooth("no Bluetooth adapter found".into()))
}

/// How to reach the bulb at `address` over `bearer`.
pub fn connector(address: &str, bearer: Bearer) -> Result<Arc<dyn Connector>> {
    match bearer {
        Bearer::Ble => Ok(Arc::new(ble::BleConnector::new(address))),
        Bearer::Spp => serial_port(address, chsmartbulb_core::protocol::RFCOMM_CHANNEL),
    }
}

/// The bulb's serial port (SPP) on an RFCOMM `channel` of your choosing.
pub fn serial_port(address: &str, channel: u8) -> Result<Arc<dyn Connector>> {
    #[cfg(target_os = "linux")]
    return Ok(Arc::new(rfcomm_linux::RfcommConnector::new(address)?.channel(channel)));
    #[cfg(windows)]
    return Ok(Arc::new(rfcomm_windows::RfcommConnector::new(address)?.channel(channel)));
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (address, channel);
        Err(Error::Unsupported("SPP is not available on this platform; use BLE".into()))
    }
}

/// What this computer plays (a loopback of the default output), or its microphone;
/// `device` names another source to capture from, where the platform has such names.
pub fn audio_capture(microphone: bool, device: Option<String>) -> Option<Arc<dyn AudioCapture>> {
    #[cfg(any(target_os = "linux", windows))]
    return Some(Arc::new(audio::DesktopAudio::new(microphone).device(device)));
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (microphone, device);
        None
    }
}

/// What this computer's screens show: an X11 desktop on Linux, the desktop on Windows.
///
/// In a Wayland session the desktop is asked to share a screen (with the `portal` feature):
/// it lets the user choose one, so no monitors are listed, and the choice is remembered in
/// `shared_screen` when a path is given.
pub fn screen_capture(shared_screen: Option<std::path::PathBuf>) -> Option<Arc<dyn ScreenCapture>> {
    #[cfg(any(target_os = "linux", windows))]
    return Some(Arc::new(screen::DesktopScreen::new().remember_in(shared_screen)));
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = shared_screen;
        None
    }
}

/// Whether [`audio_capture`] can hear what the computer plays.
pub const CAN_CAPTURE_PLAYBACK: bool = cfg!(any(target_os = "linux", windows));
