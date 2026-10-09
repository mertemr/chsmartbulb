use serde::{ser::Serializer, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Bluetooth(String),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Core(#[from] chsmartbulb_core::Error),
    #[cfg(mobile)]
    #[error(transparent)]
    PluginInvoke(#[from] tauri::plugin::mobile::PluginInvokeError),
}

#[cfg(desktop)]
impl From<chsmartbulb_desktop::Error> for Error {
    fn from(error: chsmartbulb_desktop::Error) -> Self {
        match error {
            chsmartbulb_desktop::Error::Bluetooth(message) => Error::Bluetooth(message),
            chsmartbulb_desktop::Error::Unsupported(message) => Error::Unsupported(message),
            chsmartbulb_desktop::Error::Core(error) => Error::Core(error),
        }
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.to_string().as_ref())
    }
}

impl From<Error> for chsmartbulb_core::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Core(error) => error,
            other => chsmartbulb_core::Error::ConnectionFailed(other.to_string()),
        }
    }
}
