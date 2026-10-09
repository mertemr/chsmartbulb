pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Bluetooth(String),
    #[error("{0}")]
    Unsupported(String),
    #[error(transparent)]
    Core(#[from] chsmartbulb_core::Error),
}

impl From<Error> for chsmartbulb_core::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Core(error) => error,
            other => chsmartbulb_core::Error::ConnectionFailed(other.to_string()),
        }
    }
}
