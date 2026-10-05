//! The CHSmartBulb / BL08A speaker bulb: protocol, effects and the background service.
//!
//! A Rust port of the `chsmartbulb` Python package's core, shared by the desktop and
//! mobile app. It speaks the same JSON socket protocol as the Python service, so the
//! web interface and the agents work with either. Bluetooth itself is left to a
//! platform layer that implements [`transport::Connector`].

pub mod audio;
pub mod bulb;
pub mod catalog;
pub mod color;
pub mod effects;
pub mod error;
pub mod protocol;
pub mod screen;
pub mod service;
pub mod transport;

pub use bulb::Bulb;
pub use color::Color;
pub use error::{Error, Result};
pub use service::{Service, Session};
pub use transport::{Bearer, Connector, Link};
