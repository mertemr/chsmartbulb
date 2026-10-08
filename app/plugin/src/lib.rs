//! Bluetooth for the chsmartbulb app, on every platform it runs on.
//!
//! The bulb is reached over BLE (GATT) or Bluetooth Classic (SPP). On Android both
//! go through the Kotlin side of this plugin, which also captures sound for the
//! effects that follow it. On Linux and Windows BLE goes through btleplug and SPP
//! through an RFCOMM socket.

use tauri::plugin::{Builder, TauriPlugin};
use tauri::{Manager, Runtime};

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

mod error;
mod models;

pub use error::{Error, Result};
pub use models::*;

#[cfg(desktop)]
pub use desktop::{Bluetooth, PresenceWatcher};
#[cfg(mobile)]
pub use mobile::{Bluetooth, PresenceWatcher};

/// Access to the plugin from anything that can reach the app's state.
pub trait BluetoothExt<R: Runtime> {
    fn bluetooth(&self) -> &Bluetooth<R>;
}

impl<R: Runtime, T: Manager<R>> BluetoothExt<R> for T {
    fn bluetooth(&self) -> &Bluetooth<R> {
        self.state::<Bluetooth<R>>().inner()
    }
}

pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("chsmartbulb")
        .setup(|app, api| {
            #[cfg(mobile)]
            let bluetooth = mobile::init(app, api)?;
            #[cfg(desktop)]
            let bluetooth = desktop::init(app, api)?;
            app.manage(bluetooth);
            Ok(())
        })
        .build()
}
