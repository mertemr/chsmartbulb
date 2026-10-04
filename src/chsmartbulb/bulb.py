"""High-level control of one bulb."""

from __future__ import annotations

import asyncio
import contextlib
import datetime as dt
import logging
from dataclasses import dataclass

from . import protocol as p
from .color import WHITE, Color
from .errors import NotConnected, ProtocolError, RequestTimeout, TransportError
from .light import Light
from .transport import BleTransport, RfcommTransport, Transport

log = logging.getLogger(__name__)


@dataclass(frozen=True)
class DeviceInfo:
    name: str
    version: str
    model: str
    model_id: int
    device_id: int


class ChSmartBulb(Light):
    """A bulb reachable through any :class:`~smartbulb.transport.Transport`.

    The bulb has no dimmer and no power switch on the wire: brightness is the
    magnitude of the channel values and "off" is all channels at zero. This
    class keeps the chosen colour and brightness itself and sends the product.
    The bulb cannot report brightness, so after connecting it is assumed to be 1.0.
    """

    def __init__(
        self,
        transport: Transport,
        *,
        auto_reconnect: bool = True,
        request_timeout: float = 2.0,
    ) -> None:
        self._transport = transport
        self._auto_reconnect = auto_reconnect
        self._request_timeout = request_timeout
        self._reader_task: asyncio.Task | None = None
        self._pending: dict[int, asyncio.Future[p.Frame]] = {}
        self._write_lock = asyncio.Lock()
        self._request_lock = asyncio.Lock()
        self._wanted = False  # the user asked for a connection and has not closed it
        self._color = WHITE
        self._brightness = 1.0
        self._on = False

    @classmethod
    def rfcomm(cls, address: str, channel: int = p.RFCOMM_CHANNEL, **options) -> ChSmartBulb:
        """Bluetooth Classic serial port. Works while the bulb is also used as a speaker."""
        return cls(RfcommTransport(address, channel), **options)

    @classmethod
    def ble(cls, address: str, **options) -> ChSmartBulb:
        """Bluetooth Low Energy. Only available while no Classic link to the bulb is up."""
        return cls(BleTransport(address, p.BLE_WRITE_UUID, p.BLE_NOTIFY_UUID), **options)

    # --- connection -------------------------------------------------------------------

    @property
    def is_connected(self) -> bool:
        return self._transport.is_open

    async def connect(self) -> None:
        """Open the link, check the bulb speaks this protocol, and read its light state."""
        self._wanted = True
        await self._open()
        try:
            model_id = p.parse_model_id(await self.request(p.query(p.Command.IDENTIFY)))
            if model_id != p.MODEL_ID:
                log.warning("unexpected model id 0x%04x (expected 0x%04x)", model_id, p.MODEL_ID)
            state = await self.get_light_state()
        except BaseException:
            await self.disconnect()
            raise
        self._on = state.is_on
        if state.is_on:
            self._color = state.color
        self._brightness = 1.0

    async def disconnect(self) -> None:
        self._wanted = False
        await self._drop()

    async def _open(self) -> None:
        await self._transport.open()
        self._reader_task = asyncio.create_task(self._read_loop())

    async def _drop(self) -> None:
        task, self._reader_task = self._reader_task, None
        if task is not None and task is not asyncio.current_task():
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
        await self._transport.close()

    async def _read_loop(self) -> None:
        reader = p.FrameReader()
        try:
            while True:
                for frame in reader.feed(await self._transport.read()):
                    waiter = self._pending.get(frame.command)
                    if frame.type == p.FrameType.ANSWER and waiter is not None and not waiter.done():
                        waiter.set_result(frame)
                    else:
                        log.debug("unsolicited frame %02x %02x %s", frame.type, frame.command, frame.body.hex())
        except TransportError as exc:
            log.info("connection lost: %s", exc)
            await self._transport.close()
            for waiter in self._pending.values():
                if not waiter.done():
                    waiter.set_exception(exc)

    # --- raw access -------------------------------------------------------------------

    async def send(self, frame: p.Frame) -> None:
        """Send a frame that expects no answer. Reconnects once if the link has dropped."""
        data = frame.encode()
        async with self._write_lock:
            for attempt in range(2):
                if not self._transport.is_open:
                    if not self._wanted:
                        raise NotConnected("not connected; call connect() first")
                    if not self._auto_reconnect:
                        raise NotConnected("connection lost and auto_reconnect is off")
                    await self._drop()
                    await self._open()
                try:
                    await self._transport.write(data)
                    return
                except TransportError:
                    await self._drop()
                    if attempt or not self._auto_reconnect:
                        raise

    async def request(self, frame: p.Frame) -> p.Frame:
        """Send a query and wait for the answer carrying the same command byte."""
        async with self._request_lock:
            waiter: asyncio.Future[p.Frame] = asyncio.get_running_loop().create_future()
            self._pending[frame.command] = waiter
            try:
                await self.send(frame)
                return await asyncio.wait_for(waiter, self._request_timeout)
            except asyncio.TimeoutError:
                raise RequestTimeout(f"no answer to query 0x{frame.command:02x}") from None
            finally:
                self._pending.pop(frame.command, None)

    # --- light ------------------------------------------------------------------------

    @property
    def color(self) -> Color:
        """The colour shown when on, at full scale (before brightness is applied)."""
        return self._color

    @property
    def brightness(self) -> float:
        return self._brightness

    @property
    def is_on(self) -> bool:
        return self._on

    async def set_color(self, color: Color, *, fade: bool = False) -> None:
        """Show ``color`` scaled by the current brightness. An all-zero colour turns the light off."""
        if color.is_off:
            await self.turn_off(fade=fade)
            return
        self._color = color
        self._on = True
        await self.send(p.light(color.scaled(self._brightness), fade=fade))

    async def set_white(self, level: int = 255, *, fade: bool = False) -> None:
        """Use the dedicated white LEDs only."""
        await self.set_color(Color(w=level), fade=fade)

    async def set_brightness(self, level: float) -> None:
        if not 0.0 <= level <= 1.0:
            raise ValueError("brightness must be within 0..1")
        self._brightness = level
        if self._on:
            await self.send(p.light(self._color.scaled(level)))

    async def turn_on(self, *, fade: bool = False) -> None:
        self._on = True
        await self.send(p.light(self._color.scaled(self._brightness), fade=fade))

    async def turn_off(self, *, fade: bool = False) -> None:
        self._on = False
        await self.send(p.light(Color(), fade=fade))

    async def set_native_effect(
        self,
        effect: p.NativeEffect,
        color: Color | None = None,
        *,
        speed: int = 8,
    ) -> None:
        """Start one of the bulb's built-in effects.

        ``speed`` is a level from 0 (fastest) to 15 (slowest). ``color`` is used
        only by effects that take one; any light command ends the effect.
        """
        if color is not None and not color.is_off:
            self._color = color
        shown = self._color.scaled(self._brightness) if effect.uses_color else Color()
        self._on = True
        await self.send(p.light(shown, effect=effect, speed=p.speed_byte(effect, speed)))

    async def get_light_state(self) -> p.LightState:
        return p.parse_light_state(await self.request(p.query(p.Command.LIGHT_STATE)))

    # --- device -----------------------------------------------------------------------

    async def get_info(self) -> DeviceInfo:
        name = p.parse_name(await self.request(p.query(p.Command.NAME)))
        hardware = p.parse_hardware(await self.request(p.query(p.Command.HARDWARE)))
        model_id = p.parse_model_id(await self.request(p.query(p.Command.IDENTIFY)))
        return DeviceInfo(name.name, name.version, hardware.model, model_id, hardware.device_id)

    async def get_timers(self) -> list[p.Timer]:
        """Schedule entries stored in the bulb (read only)."""
        return p.parse_timers(await self.request(p.query(p.Command.TIMERS)))

    async def set_timer_enabled(self, index: int, enabled: bool) -> p.Timer:
        """Switch a stored timer on or off, keeping its name, time and days.

        The bulb does not acknowledge the write, so the timer is read back and
        the call fails if the change did not take.
        """
        timer = next((t for t in await self.get_timers() if t.index == index), None)
        if timer is None:
            raise ValueError(f"the bulb has no timer with index {index}")
        await self.send(p.write_timer(timer, enabled=enabled))
        updated = next((t for t in await self.get_timers() if t.index == index), None)
        if updated is None or updated.enabled != enabled:
            raise ProtocolError(f"the bulb did not apply the change to timer {index}")
        return updated

    async def get_status_raw(self) -> bytes:
        """The 24 status bytes the vendor app polls every second; meaning unknown."""
        frame = await self.request(p.query(p.Command.STATUS))
        if len(frame.body) < 8:
            raise ProtocolError(f"status answer too short: {frame.body.hex()}")
        return frame.body[8:]

    async def sync_clock(self, when: dt.datetime | None = None) -> None:
        """Set the bulb's clock, which its stored timers run on.

        Built from vendor-app captures and not yet exercised on a real bulb.
        """
        await self.send(p.set_clock(when or dt.datetime.now()))
