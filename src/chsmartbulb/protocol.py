"""Wire format of the CHSmartBulb / BL08A control protocol (no I/O in this module).

Every message is one frame::

    01 fe 00 00 | type | command | length (u16 LE, whole frame) | body

The same frames travel over SPP/RFCOMM and over the BLE characteristics.
See ``docs/protocol.md`` for how each field was verified.
"""

from __future__ import annotations

import datetime as dt
from dataclasses import dataclass
from enum import IntEnum

from .color import Color
from .errors import ProtocolError

MAGIC = b"\x01\xfe\x00\x00"
HEADER_LEN = 8

RFCOMM_CHANNEL = 2
BLE_WRITE_UUID = "00008877-0000-1000-8000-00805f9b34fb"
BLE_NOTIFY_UUID = "00008888-0000-1000-8000-00805f9b34fb"

#: Sent by the vendor app right after connecting. The bulb works without it.
HELLO = b"01234567"

#: Reply body prefix to :data:`Command.IDENTIFY` on this protocol dialect.
MODEL_ID = 0x0C47

_ARGS_QUERY = bytes.fromhex("0000008000000080")
_ARGS_ZERO = bytes(8)
_ARGS_ALL = bytes.fromhex("ffffffff00000080")


class FrameType(IntEnum):
    ANSWER = 0x41  # 'A', device -> host
    QUERY  = 0x51  # 'Q', host -> device, answered with the same command byte
    SET    = 0x53  # 'S', host -> device, not acknowledged


class Command(IntEnum):
    STATUS      = 0x00  # as QUERY; as SET the same byte sets the clock
    IDENTIFY    = 0x02
    TIMERS      = 0x30
    RINGTONES   = 0x31
    ALARMS      = 0x32
    NAME        = 0x80
    HARDWARE    = 0x81
    LIGHT_STATE = 0x82
    LIGHT       = 0x83


_QUERY_ARGS = {
    Command.STATUS: _ARGS_QUERY,
    Command.IDENTIFY: _ARGS_QUERY,
    Command.TIMERS: _ARGS_QUERY,
    Command.RINGTONES: _ARGS_QUERY,
    Command.ALARMS: _ARGS_ALL,
    Command.NAME: _ARGS_ZERO,
    Command.HARDWARE: _ARGS_ZERO,
    Command.LIGHT_STATE: _ARGS_ZERO,
}


class NativeEffect(IntEnum):
    """Effect byte of the light command. Unknown values make the bulb ignore the frame."""

    FIXED = 0x50
    MUSIC = 0x51  # sound reactive; dark while silent, ignores the colour
    BREATHING = 0x52
    RAINBOW = 0x53
    FLASH = 0x54
    HEARTBEAT = 0x56
    AUTOMATIC = 0x58
    CANDLE = 0x5A
    OCEAN = 0x5C
    NATURAL = 0x5D
    SUNSET = 0x5E
    PASSION = 0x5F
    RGB_CUT = 0x61

    @property
    def uses_color(self) -> bool:
        return self in (
            NativeEffect.FIXED,
            NativeEffect.BREATHING,
            NativeEffect.FLASH,
            NativeEffect.HEARTBEAT,
            NativeEffect.CANDLE,
        )


@dataclass(frozen=True)
class Frame:
    type: int
    command: int
    body: bytes = b""

    def encode(self) -> bytes:
        length = HEADER_LEN + len(self.body)
        if length > 0xFFFF:
            raise ProtocolError("frame body too long")
        return MAGIC + bytes((self.type, self.command)) + length.to_bytes(2, "little") + self.body

    @classmethod
    def decode(cls, data: bytes) -> Frame:
        if len(data) < HEADER_LEN or data[:4] != MAGIC:
            raise ProtocolError(f"not a frame: {data.hex()}")
        length = int.from_bytes(data[6:8], "little")
        if length != len(data):
            raise ProtocolError(f"frame length field says {length}, got {len(data)} bytes")
        return cls(data[4], data[5], bytes(data[HEADER_LEN:]))


class FrameReader:
    """Reassembles frames from arbitrary chunks (RFCOMM reads or BLE notifications)."""

    def __init__(self) -> None:
        self._buffer = bytearray()

    def feed(self, data: bytes) -> list[Frame]:
        self._buffer += data
        frames: list[Frame] = []
        while True:
            start = self._buffer.find(MAGIC)
            if start < 0:
                # keep a possible partial magic at the tail, drop the rest
                del self._buffer[: max(0, len(self._buffer) - len(MAGIC) + 1)]
                return frames
            del self._buffer[:start]
            if len(self._buffer) < HEADER_LEN:
                return frames
            length = int.from_bytes(self._buffer[6:8], "little")
            if length < HEADER_LEN:
                del self._buffer[: len(MAGIC)]  # corrupt header, resync on the next magic
                continue
            if len(self._buffer) < length:
                return frames
            frames.append(Frame(self._buffer[4], self._buffer[5], bytes(self._buffer[HEADER_LEN:length])))
            del self._buffer[:length]


# --- builders ---------------------------------------------------------------------------


def query(command: int, args: bytes | None = None) -> Frame:
    """A query frame with the argument bytes the vendor app uses for that command."""
    if args is None:
        try:
            args = _QUERY_ARGS[Command(command)]
        except (KeyError, ValueError):
            raise ProtocolError(f"no default arguments known for query 0x{command:02x}") from None
    return Frame(FrameType.QUERY, command, args)


def light(
    color: Color,
    *,
    effect: int = NativeEffect.FIXED,
    speed: int = 0,
    fade: bool = False,
) -> Frame:
    """The light command. Channel values are the brightness; there is no separate dimmer."""
    if not 0 <= speed <= 255:
        raise ValueError("speed must be 0..255")
    # body: GG BB RR speed effect WW YY fade  (YY drives no LED on this model)
    body = bytes((color.g, color.b, color.r, speed, int(effect), color.w, 0, 1 if fade else 0))
    return Frame(FrameType.SET, Command.LIGHT, body)


def speed_byte(effect: NativeEffect, level: int) -> int:
    """Speed byte for ``level`` 0 (fastest) .. 15 (slowest), packed the way the vendor app does."""
    if not 0 <= level <= 15:
        raise ValueError("speed level must be 0..15")
    if effect is NativeEffect.FIXED:
        return 0
    if effect is NativeEffect.MUSIC:
        return level * 2  # app range 0..30
    if effect is NativeEffect.CANDLE:
        return level * 4  # app range 0..60
    if effect is NativeEffect.HEARTBEAT:
        return level << 4
    return (level << 4) | (level >> 2)


def set_clock(when: dt.datetime) -> Frame:
    """Clock-set frame as sent by the vendor app on connect (not exercised on a device here)."""
    body = (
        bytes.fromhex("0000000000000080")
        + when.year.to_bytes(2, "little")
        + bytes((when.month, when.day, when.hour, when.minute, when.second, 0))
    )
    return Frame(FrameType.SET, Command.STATUS, body)


# --- parsers ----------------------------------------------------------------------------


def _expect(frame: Frame, command: int, min_body: int) -> None:
    if frame.type != FrameType.ANSWER or frame.command != command:
        raise ProtocolError(f"expected answer 0x{command:02x}, got {frame.type:02x} {frame.command:02x}")
    if len(frame.body) < min_body:
        raise ProtocolError(f"answer 0x{command:02x} too short: {frame.body.hex()}")


def _cstring(raw: bytes, encoding: str = "utf-8") -> str:
    return raw.split(b"\0", 1)[0].decode(encoding, "replace")


@dataclass(frozen=True)
class LightState:
    """What the bulb reports for its light.

    The bulb rescales the channels so the largest is 255, so this tells the
    colour mix and whether the light is on, but not the brightness.
    """

    color: Color
    yellow: int
    flags: int

    @property
    def is_on(self) -> bool:
        return not self.color.is_off


def parse_light_state(frame: Frame) -> LightState:
    _expect(frame, Command.LIGHT_STATE, 8)
    g, b, r, _speed, _effect, w, y, flags = frame.body[:8]
    return LightState(Color(r, g, b, w), y, flags)


def parse_model_id(frame: Frame) -> int:
    _expect(frame, Command.IDENTIFY, 2)
    return int.from_bytes(frame.body[:2], "little")


@dataclass(frozen=True)
class DeviceName:
    version: str
    name: str


def parse_name(frame: Frame) -> DeviceName:
    _expect(frame, Command.NAME, 17)
    return DeviceName(_cstring(frame.body[8:15], "ascii"), _cstring(frame.body[16:]))


@dataclass(frozen=True)
class Hardware:
    device_id: int
    model: str


def parse_hardware(frame: Frame) -> Hardware:
    _expect(frame, Command.HARDWARE, 8)
    body = frame.body
    return Hardware(int.from_bytes(body[:2], "little"), body[4:8][::-1].decode("ascii", "replace"))


@dataclass(frozen=True)
class Timer:
    """A stored schedule entry, as read from the bulb."""

    index: int
    name: str
    enabled: bool
    days: int  # bit mask, 0x7f = every day
    hour: int
    minute: int
    raw: bytes

    _RECORD = 44
    _NAME = 32


def write_timer(timer: Timer, *, enabled: bool) -> Frame:
    """Rewrite a stored timer with only its enabled flag changed."""
    name = timer.raw[: Timer._NAME].split(b"\0", 1)[0].ljust(Timer._NAME, b"\0")
    flag = int(enabled)
    tail = bytes((timer.index, 0, flag, timer.days, timer.hour, timer.minute)) + timer.raw[Timer._NAME + 6 :]
    body = timer.index.to_bytes(4, "little") + flag.to_bytes(4, "little") + name + tail
    return Frame(FrameType.SET, Command.TIMERS, body)


def parse_timers(frame: Frame) -> list[Timer]:
    _expect(frame, Command.TIMERS, 8)
    count = int.from_bytes(frame.body[:4], "little")
    records = frame.body[8:]
    if len(records) < count * Timer._RECORD:
        raise ProtocolError(f"timer list truncated: {count} entries in {len(records)} bytes")
    timers = []
    for i in range(count):
        raw = records[i * Timer._RECORD : (i + 1) * Timer._RECORD]
        tail = raw[Timer._NAME :]
        timers.append(Timer(tail[0], _cstring(raw[: Timer._NAME]), bool(tail[2]), tail[3], tail[4], tail[5], raw))
    return timers
