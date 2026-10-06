//! Errors shared by every transport and device, mirroring `chsmartbulb.errors`.

/// Everything that can go wrong talking to a bulb or handling a request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// The transport could not be opened.
    #[error("{0}")]
    ConnectionFailed(String),
    /// An operation needed an open connection and there was none.
    #[error("{0}")]
    NotConnected(String),
    /// The transport failed or was closed while in use.
    #[error("{0}")]
    Transport(String),
    /// The device sent something that does not fit the protocol.
    #[error("{0}")]
    Protocol(String),
    /// The device did not answer a query in time.
    #[error("{0}")]
    Timeout(String),
    /// A request or parameter was not acceptable.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Whether the link to the bulb is gone, as opposed to a bad request or a confused bulb.
    pub fn is_link_error(&self) -> bool {
        matches!(self, Error::ConnectionFailed(_) | Error::NotConnected(_) | Error::Transport(_))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
