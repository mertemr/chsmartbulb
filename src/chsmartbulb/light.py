"""The device-independent lighting interface.

Effects and applications should depend on :class:`Light` only, so that other
bulbs (BLE, Wi-Fi, ...) can be added later by implementing this one class.
"""

from __future__ import annotations

from abc import ABC, abstractmethod

from .color import Color


class Light(ABC):
    """A single colour light that can be switched, coloured and dimmed."""

    @abstractmethod
    async def connect(self) -> None:
        """Open the connection to the device."""

    @abstractmethod
    async def disconnect(self) -> None:
        """Close the connection; safe to call when already closed."""

    @property
    @abstractmethod
    def is_connected(self) -> bool:
        """Whether the connection is currently open."""

    @property
    @abstractmethod
    def brightness(self) -> float:
        """Current brightness factor in 0..1."""

    @abstractmethod
    async def set_color(self, color: Color, *, fade: bool = False) -> None:
        """Show ``color`` at the current brightness. ``fade`` asks for a soft transition."""

    @abstractmethod
    async def set_brightness(self, level: float) -> None:
        """Set brightness to ``level`` in 0..1, keeping the colour."""

    @abstractmethod
    async def turn_on(self, *, fade: bool = False) -> None:
        """Light up with the last colour."""

    @abstractmethod
    async def turn_off(self, *, fade: bool = False) -> None:
        """Go dark, remembering the colour for :meth:`turn_on`."""

    async def set_rgb(self, r: int, g: int, b: int, w: int = 0, *, fade: bool = False) -> None:
        await self.set_color(Color(r, g, b, w), fade=fade)

    async def __aenter__(self) -> Light:
        await self.connect()
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.disconnect()
