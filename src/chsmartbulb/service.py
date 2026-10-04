"""Background service: keeps the connection, the running effect and the remembered state.

Requests and replies are JSON objects, one per line, over a Unix socket. The same
:meth:`BulbService.handle` also serves the command line when no service is running.
"""

from __future__ import annotations

import asyncio
import contextlib
import json
import logging
import os
import signal
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from . import catalog, effects
from . import protocol as p
from .color import WHITE, Color, parse_color
from .errors import ConnectionFailed, NotConnected, SmartBulbError, TransportError
from .music import AudioSource, MusicSource
from .protocol import NativeEffect

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping

    from .bulb import ChSmartBulb

log = logging.getLogger(__name__)

_LINK_ERRORS = (ConnectionFailed, NotConnected, TransportError)
_MAX_RETRY_DELAY = 60.0


def default_socket_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    base = Path(runtime) if runtime else Path(tempfile.gettempdir()) / f"chsmartbulb-{os.getuid()}"
    return base / "chsmartbulb.sock"


def default_state_path() -> Path:
    state = os.environ.get("XDG_STATE_HOME")
    base = Path(state) if state else Path.home() / ".local" / "state"
    return base / "chsmartbulb" / "state.json"


@dataclass
class Plan:
    """What the light should be showing; survives reconnects and restarts."""

    on: bool = True
    color: Color = WHITE
    brightness: float = 1.0
    effect: dict[str, Any] | None = None  # {"name": ..., "params": {...}}
    native: dict[str, Any] | None = None  # {"name": ..., "speed": ...}

    def to_json(self) -> dict[str, Any]:
        return {
            "on": self.on,
            "color": self.color.to_hex(),
            "brightness": self.brightness,
            "effect": self.effect,
            "native": self.native,
        }

    @classmethod
    def from_json(cls, data: Mapping[str, Any]) -> Plan:
        return cls(
            on=bool(data["on"]),
            color=parse_color(data["color"]),
            brightness=float(data["brightness"]),
            effect=data.get("effect"),
            native=data.get("native"),
        )


class BulbService:
    def __init__(
        self,
        bulb: ChSmartBulb,
        *,
        state_path: Path | None = None,
        fps: float = effects.DEFAULT_FPS,
        retry_delay: float = 5.0,
        poll_interval: float = 2.0,
        music_factory: Callable[[], AudioSource] = MusicSource,
    ) -> None:
        self._bulb = bulb
        self._state_path = state_path
        self._fps = fps
        self._retry_delay = retry_delay
        self._poll_interval = poll_interval
        self._music_factory = music_factory
        self._plan = Plan()
        self._adopt_on_connect = True  # no remembered state yet: take over what the bulb shows
        self._effect_task: asyncio.Task[None] | None = None
        self._music: AudioSource | None = None
        self._supervisor: asyncio.Task[None] | None = None
        self._commands: dict[str, Callable[[Mapping[str, Any]], Awaitable[dict[str, Any]]]] = {
            "status": self._status,
            "on": self._on,
            "off": self._off,
            "color": self._color,
            "brightness": self._brightness,
            "effect": self._effect,
            "native": self._native,
            "stop": self._stop,
            "effects": self._effects,
            "info": self._info,
            "timers": self._timers,
            "timer": self._timer,
            "raw": self._raw,
        }

    # --- lifecycle --------------------------------------------------------------------

    async def attach(self) -> None:
        """Connect once and take over the bulb's current state (no supervision)."""
        await self._bulb.connect()
        self._adopt()

    async def start(self) -> None:
        """Load the remembered state and keep the bulb connected in the background."""
        self._load()
        self._supervisor = asyncio.create_task(self._supervise())

    async def close(self) -> None:
        supervisor, self._supervisor = self._supervisor, None
        if supervisor is not None:
            supervisor.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await supervisor
        await self._stop_effect()
        await self._bulb.disconnect()

    async def wait_effect(self) -> None:
        """Wait until the running effect ends by itself."""
        if self._effect_task is not None:
            await self._effect_task

    def _adopt(self) -> None:
        self._plan = Plan(on=self._bulb.is_on, color=self._bulb.color, brightness=self._bulb.brightness)
        self._adopt_on_connect = False

    async def _supervise(self) -> None:
        delay = self._retry_delay
        while True:
            if self._bulb.is_connected:
                await asyncio.sleep(self._poll_interval)
                continue
            try:
                await self._bulb.connect()
            except SmartBulbError as exc:
                log.info("bulb unavailable: %s", exc)
                await asyncio.sleep(delay)
                delay = min(delay * 2, _MAX_RETRY_DELAY)
                continue
            log.info("connected to the bulb")
            delay = self._retry_delay
            if self._adopt_on_connect:
                self._adopt()
                self._save()
            else:
                await self._apply()

    # --- state ------------------------------------------------------------------------

    def _load(self) -> None:
        if self._state_path is None or not self._state_path.exists():
            return
        try:
            plan = Plan.from_json(json.loads(self._state_path.read_text()))
            if plan.effect is not None:
                self._check_effect(plan.effect["name"], plan.effect.get("params") or {})
        except (OSError, ValueError, KeyError, TypeError, SmartBulbError) as exc:
            log.warning("ignoring unusable state file %s: %s", self._state_path, exc)
            return
        self._plan = plan
        self._adopt_on_connect = False

    def _save(self) -> None:
        if self._state_path is None:
            return
        try:
            self._state_path.parent.mkdir(parents=True, exist_ok=True)
            self._state_path.write_text(json.dumps(self._plan.to_json(), indent=2) + "\n")
        except OSError as exc:
            log.warning("cannot save state to %s: %s", self._state_path, exc)

    async def _apply(self, *, fade: bool = False, duration: float | None = None) -> None:
        """Make the bulb show the plan. Does nothing while disconnected; the supervisor retries."""
        await self._stop_effect()
        if not self._bulb.is_connected:
            return
        plan = self._plan
        try:
            if self._bulb.brightness != plan.brightness:
                await self._bulb.set_brightness(plan.brightness)
            if not plan.on:
                await self._bulb.turn_off(fade=fade)
            elif plan.effect is not None:
                await self._start_effect(plan.effect, duration)
            elif plan.native is not None:
                native = NativeEffect[plan.native["name"].upper()]
                await self._bulb.set_native_effect(native, plan.color, speed=plan.native["speed"])
            else:
                await self._bulb.set_color(plan.color, fade=fade)
        except _LINK_ERRORS as exc:
            await self._link_lost(exc)

    async def _link_lost(self, exc: Exception) -> None:
        log.info("lost the bulb: %s", exc)
        await self._bulb.disconnect()

    async def _commit(self, *, fade: bool = False, duration: float | None = None) -> None:
        self._adopt_on_connect = False  # a request made while the bulb is away wins over what it shows later
        self._save()
        await self._apply(fade=fade, duration=duration)

    # --- effects ----------------------------------------------------------------------

    async def _start_effect(self, spec: Mapping[str, Any], duration: float | None) -> None:
        info = catalog.CATALOG[spec["name"]]
        if info.needs_audio:
            self._music = self._music_factory()
            await self._music.start()
        effect = catalog.create(spec["name"], spec.get("params"), audio=self._music)
        self._effect_task = asyncio.create_task(self._run_effect(effect, duration))

    async def _run_effect(self, effect: effects.Effect, duration: float | None) -> None:
        try:
            await effects.play(self._bulb, effect, duration=duration, fps=self._fps)
        except _LINK_ERRORS as exc:
            await self._link_lost(exc)  # the plan keeps the effect, so it resumes after reconnecting
            return
        # ran its full duration: go back to the plain colour
        self._plan.effect = None
        self._save()
        music, self._music = self._music, None
        if music is not None:
            await music.stop()
        try:
            await self._bulb.set_color(self._plan.color)
        except _LINK_ERRORS as exc:
            await self._link_lost(exc)

    def _check_effect(self, name: str, params: Mapping[str, Any]) -> None:
        """Build the effect once, so a bad request fails before it reaches the plan."""
        catalog.resolve(name, params)
        audio = self._music_factory() if catalog.CATALOG[name].needs_audio else None
        catalog.create(name, params, audio=audio)

    async def _stop_effect(self) -> None:
        task, self._effect_task = self._effect_task, None
        if task is not None:
            task.cancel()
            with contextlib.suppress(asyncio.CancelledError):
                await task
        music, self._music = self._music, None
        if music is not None:
            await music.stop()

    @property
    def _playing(self) -> bool:
        return self._effect_task is not None and not self._effect_task.done()

    # --- requests ---------------------------------------------------------------------

    async def handle(self, request: Mapping[str, Any]) -> dict[str, Any]:
        """Run one request and return ``{"ok": True, ...}`` or ``{"ok": False, "error": ...}``."""
        try:
            handler = self._commands[request["cmd"]]
        except KeyError:
            return {"ok": False, "error": f"unknown command {request.get('cmd')!r}"}
        try:
            return {"ok": True, **await handler(request)}
        except (SmartBulbError, ValueError, KeyError, TypeError) as exc:
            message = f"missing field {exc}" if isinstance(exc, KeyError) else str(exc)
            return {"ok": False, "error": message}

    def _require_link(self) -> None:
        if not self._bulb.is_connected:
            raise NotConnected("the bulb is not connected")

    def _take_brightness(self, request: Mapping[str, Any]) -> None:
        level = request.get("brightness")
        if level is not None:
            if not 0.0 <= float(level) <= 1.0:
                raise ValueError("brightness must be within 0..1")
            self._plan.brightness = float(level)

    async def _status(self, request: Mapping[str, Any]) -> dict[str, Any]:
        reply: dict[str, Any] = {"connected": self._bulb.is_connected, "playing": self._playing, **self._plan.to_json()}
        if self._bulb.is_connected:
            try:
                reply["bulb"] = (await self._bulb.get_light_state()).color.to_hex()
            except SmartBulbError as exc:
                log.debug("status read-back failed: %s", exc)
        return reply

    async def _on(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._plan.on = True
        await self._commit(fade=bool(request.get("fade")))
        return {}

    async def _off(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._plan.on = False
        await self._commit(fade=bool(request.get("fade")))
        return {}

    async def _color(self, request: Mapping[str, Any]) -> dict[str, Any]:
        color = parse_color(request["color"])
        self._take_brightness(request)
        plan = self._plan
        plan.effect = plan.native = None
        plan.on = not color.is_off
        if plan.on:
            plan.color = color
        await self._commit(fade=bool(request.get("fade")))
        return {}

    async def _brightness(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._take_brightness({"brightness": request["level"]})
        self._adopt_on_connect = False
        self._save()
        if self._playing and self._bulb.is_connected:
            # a running effect picks the new level up on its next frame
            try:
                await self._bulb.set_brightness(self._plan.brightness)
            except _LINK_ERRORS as exc:
                await self._link_lost(exc)
        else:
            await self._apply()
        return {}

    async def _effect(self, request: Mapping[str, Any]) -> dict[str, Any]:
        name, params = request["name"], dict(request.get("params") or {})
        self._check_effect(name, params)
        self._take_brightness(request)
        plan = self._plan
        plan.effect = {"name": name, "params": params}
        plan.native = None
        plan.on = True
        await self._commit(duration=request.get("duration"))
        return {}

    async def _native(self, request: Mapping[str, Any]) -> dict[str, Any]:
        name = str(request["name"]).lower()
        if name.upper() not in NativeEffect.__members__ or name == "fixed":
            raise ValueError(f"unknown native effect {name!r}")
        speed = int(request.get("speed", 8))
        p.speed_byte(NativeEffect[name.upper()], speed)
        self._take_brightness(request)
        plan = self._plan
        if request.get("color"):
            plan.color = parse_color(request["color"])
        plan.native = {"name": name, "speed": speed}
        plan.effect = None
        plan.on = True
        await self._commit()
        return {}

    async def _stop(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._plan.effect = self._plan.native = None
        await self._commit()
        return {}

    async def _effects(self, request: Mapping[str, Any]) -> dict[str, Any]:
        return {"effects": catalog.describe()}

    async def _info(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._require_link()
        info = await self._bulb.get_info()
        return {
            "name": info.name,
            "version": info.version,
            "model": info.model,
            "model_id": info.model_id,
            "device_id": info.device_id,
        }

    @staticmethod
    def _timer_json(timer: p.Timer) -> dict[str, Any]:
        return {
            "index": timer.index,
            "name": timer.name,
            "enabled": timer.enabled,
            "days": timer.days,
            "hour": timer.hour,
            "minute": timer.minute,
        }

    async def _timers(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._require_link()
        return {"timers": [self._timer_json(timer) for timer in await self._bulb.get_timers()]}

    async def _timer(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._require_link()
        timer = await self._bulb.set_timer_enabled(int(request["index"]), bool(request["enabled"]))
        return {"timer": self._timer_json(timer)}

    async def _raw(self, request: Mapping[str, Any]) -> dict[str, Any]:
        self._require_link()
        frame = p.Frame.decode(bytes.fromhex(request["hex"]))
        if frame.type == p.FrameType.QUERY:
            return {"answer": (await self._bulb.request(frame)).encode().hex()}
        await self._bulb.send(frame)
        return {}

    # --- socket -----------------------------------------------------------------------

    async def serve(self, socket_path: Path) -> None:
        """Run until cancelled, answering requests on ``socket_path``."""
        if await is_running(socket_path):
            raise SmartBulbError(f"a service is already listening on {socket_path}")
        _prepare_socket_dir(socket_path)
        await self.start()
        try:
            server = await asyncio.start_unix_server(self._serve_client, path=str(socket_path))
        except OSError as exc:
            await self.close()
            raise SmartBulbError(f"cannot listen on {socket_path}: {exc}") from exc
        _restrict(socket_path)
        loop = asyncio.get_running_loop()
        main = asyncio.current_task()
        assert main is not None
        loop.add_signal_handler(signal.SIGTERM, main.cancel)
        try:
            async with server:
                await server.serve_forever()
        finally:
            loop.remove_signal_handler(signal.SIGTERM)
            await self.close()
            _remove(socket_path)

    async def _serve_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        try:
            while line := await reader.readline():
                try:
                    request = json.loads(line)
                    if not isinstance(request, dict):
                        raise ValueError("request must be a JSON object")
                    reply = await self.handle(request)
                except ValueError as exc:
                    reply = {"ok": False, "error": f"bad request: {exc}"}
                writer.write(json.dumps(reply).encode() + b"\n")
                await writer.drain()
        except ConnectionError:
            pass
        finally:
            writer.close()


def _prepare_socket_dir(socket_path: Path) -> None:
    socket_path.parent.mkdir(parents=True, exist_ok=True)
    _remove(socket_path)  # left behind by a service that was killed


def _restrict(socket_path: Path) -> None:
    socket_path.chmod(0o600)


def _remove(socket_path: Path) -> None:
    socket_path.unlink(missing_ok=True)


async def is_running(socket_path: Path) -> bool:
    """Whether a service answers on ``socket_path``."""
    try:
        _reader, writer = await asyncio.open_unix_connection(str(socket_path))
    except OSError:
        return False
    writer.close()
    return True


async def call(socket_path: Path, request: Mapping[str, Any], *, wait: float = 15.0) -> dict[str, Any]:
    """Send one request to a running service and return its reply."""
    try:
        reader, writer = await asyncio.open_unix_connection(str(socket_path))
    except OSError as exc:
        raise ConnectionFailed(f"no service on {socket_path}: {exc}") from exc
    try:
        writer.write(json.dumps(request).encode() + b"\n")
        await writer.drain()
        line = await asyncio.wait_for(reader.readline(), wait)
    except asyncio.TimeoutError:
        raise SmartBulbError("the service did not answer in time") from None
    finally:
        writer.close()
    if not line:
        raise SmartBulbError("the service closed the connection without answering")
    return json.loads(line)
