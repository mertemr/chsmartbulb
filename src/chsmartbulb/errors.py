"""Exception hierarchy shared by every transport and device."""


class SmartBulbError(Exception):
    """Base class for all errors raised by this package."""


class ConnectionFailed(SmartBulbError):
    """The transport could not be opened."""


class NotConnected(SmartBulbError):
    """An operation needed an open connection and there was none."""


class TransportError(SmartBulbError):
    """The transport failed or was closed while in use."""


class ProtocolError(SmartBulbError):
    """The device sent something that does not fit the protocol."""


class RequestTimeout(SmartBulbError):
    """The device did not answer a query in time."""
