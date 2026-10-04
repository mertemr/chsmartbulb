"""Transport interface: an ordered, reliable byte pipe to one device."""

from __future__ import annotations

from abc import ABC, abstractmethod


class Transport(ABC):
    """Moves raw bytes; knows nothing about frames or commands."""

    @abstractmethod
    async def open(self) -> None:
        """Connect. Raises :class:`~smartbulb.errors.ConnectionFailed`."""

    @abstractmethod
    async def close(self) -> None:
        """Disconnect; safe to call when already closed."""

    @property
    @abstractmethod
    def is_open(self) -> bool:
        """Whether the pipe is currently usable."""

    @abstractmethod
    async def write(self, data: bytes) -> None:
        """Send bytes. Raises :class:`~smartbulb.errors.TransportError` on failure."""

    @abstractmethod
    async def read(self) -> bytes:
        """Wait for the next chunk of received bytes (any size, never empty).

        Raises :class:`~smartbulb.errors.TransportError` when the pipe closes.
        """
