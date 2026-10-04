"""Control CHSmartBulb / BL08A Bluetooth speaker bulbs without the vendor app."""

from .bulb import ChSmartBulb, DeviceInfo
from .color import Color, parse_color
from .errors import (
    ConnectionFailed,
    NotConnected,
    ProtocolError,
    RequestTimeout,
    SmartBulbError,
    TransportError,
)
from .light import Light
from .protocol import LightState, NativeEffect, Timer
from .sync import BlockingLight

__all__ = [
    "BlockingLight",
    "ChSmartBulb",
    "Color",
    "ConnectionFailed",
    "DeviceInfo",
    "Light",
    "LightState",
    "NativeEffect",
    "NotConnected",
    "ProtocolError",
    "RequestTimeout",
    "SmartBulbError",
    "Timer",
    "TransportError",
    "parse_color",
]
