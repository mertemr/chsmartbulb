use serde::{Deserialize, Serialize};

/// A Bluetooth device seen nearby or remembered by the system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub address: String,
    pub name: Option<String>,
    pub rssi: Option<i16>,
    /// Paired with this device already.
    pub bonded: bool,
    /// Reachable over Bluetooth Classic (SPP).
    pub classic: bool,
    /// Seen advertising over BLE.
    pub le: bool,
    /// Its name, maker or address says it is one of these bulbs.
    pub likely: bool,
}

/// What the sound-reactive effects listen to on this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AudioInput {
    /// What this device plays, wherever it plays it (Android 10+ playback capture).
    #[default]
    Playback,
    /// The microphone: the room, the TV, a party.
    Microphone,
}

/// Whether the app may use Bluetooth, and whether Bluetooth is on.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Readiness {
    pub granted: bool,
    pub enabled: bool,
}

/// Where this device's sound goes right now, so the interface can explain the choices.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AudioRoute {
    /// Output devices in use, e.g. "Phone speaker", "Bluetooth: JBL Flip 5".
    pub outputs: Vec<String>,
    /// Addresses of Bluetooth devices connected as speakers (A2DP).
    pub speakers: Vec<String>,
    /// The platform can capture what it plays (Android 10+).
    pub can_capture_playback: bool,
}

#[cfg(desktop)]
pub(crate) fn likely(name: Option<&str>, address: &str, company_ids: impl IntoIterator<Item = u16>) -> bool {
    let named = name.is_some_and(|name| {
        chsmartbulb_core::protocol::ADVERTISED_NAMES.iter().any(|known| name.eq_ignore_ascii_case(known))
    });
    let maker = company_ids.into_iter().any(|id| id == chsmartbulb_core::protocol::BLE_COMPANY_ID);
    named || maker || address.to_ascii_uppercase().starts_with("F4:4E:FD")
}
