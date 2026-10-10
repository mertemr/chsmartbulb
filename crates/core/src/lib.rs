//! The CHSmartBulb / BL08A speaker bulb: protocol, effects and the background service.
//!
//! Shared by `chsmartbulbd`, the desktop and mobile app and, through `crates/python`,
//! the `chsmartbulb` Python package. The service speaks a JSON socket protocol, which
//! is all the web interface, the command line and the agents know of it. Bluetooth
//! itself is left to a platform layer that implements [`transport::Connector`].

pub mod audio;
pub mod bulb;
pub mod catalog;
pub mod color;
pub mod effects;
pub mod error;
pub mod protocol;
pub mod screen;
#[cfg(feature = "server")]
pub mod server;
pub mod service;
pub mod sim;
pub mod transport;

pub use bulb::Bulb;
pub use color::Color;
pub use error::{Error, Result};
pub use service::{Service, Session};
pub use transport::{Bearer, Connector, Link};
