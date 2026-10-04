"""Background service: keeps the connection, the running effect and the remembered state.

Requests and replies are JSON objects, one per line, over a Unix socket and,
optionally, over TCP for other machines (those must present a token first). The
same :meth:`BulbService.handle` also serves the command line when no service is
running.
"""

from __future__ import annotations

import asyncio
import contextlib
import functools
import hmac
import json
import logging
import math
import os
import signal
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

from . import catalog, effects
from . import protocol as p
from .client import is_running
from .color import WHITE, Color, parse_color
from .errors import ConnectionFailed, NotConnected, SmartBulbError, TransportError
from .music import AudioSource, Levels, MusicSource, RemoteAudio
from .protocol import NativeEffect

if TYPE_CHECKING:
    from collections.abc import Awaitable, Callable, Mapping

    from .bulb import ChSmartBulb

log = logging.getLogger(__name__)

_LINK_ERRORS = (ConnectionFailed, NotConnected, TransportError)
_MAX_RETRY_DELAY = 60.0


def default_socket_path() -> Path:
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    user = os.getuid() if hasattr(os, "getuid") else os.environ.get("USERNAME", "user")
    base = Path(runtime) if runtime else Path(tempfile.gettempdir()) / f"chsmartbulb-{user}"
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
        self._remote = RemoteAudio()
        self._agents = 0
        self._lock = asyncio.Lock()  # one request changes the plan at a time
        self._supervisor: asyncio.Task[None] | None = None
        self.tcp_address: tuple[str, int] | None = None
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
            self._music = self._audio_source()
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
        audio = self._audio_source() if catalog.CATALOG[name].needs_audio else None
        catalog.create(name, params, audio=audio)

    def _audio_source(self) -> AudioSource:
        """An agent's feed when one is connected, local capture otherwise."""
        return self._remote if self._agents else self._music_factory()

    async def _audio_changed(self) -> None:
        effect = self._plan.effect
        if self._playing and effect is not None and catalog.CATALOG[effect["name"]].needs_audio:
            await self._apply()  # restart the effect on the other source

    async def _agent_joined(self) -> None:
        async with self._lock:
            self._agents += 1
            log.info("audio agent connected")
            await self._audio_changed()

    async def _agent_left(self) -> None:
        async with self._lock:
            self._agents -= 1
            log.info("audio agent disconnected")
            if not self._agents:
                self._remote.clear()
                await self._audio_changed()

    def _push_audio(self, request: Mapping[str, Any]) -> None:
        try:
            bass, mid, treble = (min(1.0, max(0.0, float(value))) for value in request["levels"])
            balance = min(1.0, max(-1.0, float(request.get("balance", 0.0))))
            # agents from before the sensitivity setting decided on the beat themselves
            onset = float(request["onset"]) if "onset" in request else math.inf if request.get("beat") else 0.0
        except (KeyError, TypeError, ValueError):
            return  # a malformed block is not worth a reply at forty a second
        self._remote.push(Levels(bass, mid, treble, balance), onset)

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
            async with self._lock:
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
        reply: dict[str, Any] = {
            "connected": self._bulb.is_connected,
            "playing": self._playing,
            "audio": "agent" if self._agents else "local",
            **self._plan.to_json(),
        }
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

    async def serve(
        self,
        socket_path: Path | None,
        *,
        listen: tuple[str, int] | None = None,
        token: str | None = None,
    ) -> None:
        """Run until cancelled, answering requests on ``socket_path`` and, if given, on TCP ``listen``.

        Network clients must present ``token``; without one the service refuses to listen on TCP.
        """
        if listen is not None and not token:
            raise SmartBulbError("listening on the network needs a token")
        if socket_path is not None and await is_running(socket_path):
            raise SmartBulbError(f"a service is already listening on {socket_path}")
        await self.start()
        servers: list[asyncio.AbstractServer] = []
        where = str(socket_path)
        try:
            if socket_path is not None:
                _prepare_socket_dir(socket_path)
                servers.append(await asyncio.start_unix_server(self._serve_client, path=str(socket_path)))
                _restrict(socket_path)
            if listen is not None:
                where = f"{listen[0]}:{listen[1]}"
                remote = functools.partial(self._serve_client, token=token)
                server = await asyncio.start_server(remote, listen[0], listen[1])
                servers.append(server)
                host, port = server.sockets[0].getsockname()[:2]
                self.tcp_address = (host, port)
                log.info("listening on %s:%s", host, port)
        except OSError as exc:
            for server in servers:
                server.close()
            await self.close()
            raise SmartBulbError(f"cannot listen on {where}: {exc}") from exc

        loop = asyncio.get_running_loop()
        main = asyncio.current_task()
        assert main is not None
        with contextlib.suppress(NotImplementedError):  # no signal handlers on Windows
            loop.add_signal_handler(signal.SIGTERM, main.cancel)
        try:
            await asyncio.gather(*(server.serve_forever() for server in servers))
        finally:
            with contextlib.suppress(NotImplementedError):
                loop.remove_signal_handler(signal.SIGTERM)
            for server in servers:
                server.close()
            await self.close()
            if socket_path is not None:
                _remove(socket_path)

    async def _serve_client(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter, *, token: str | None = None
    ) -> None:
        """Serve one connection. ``token`` is what a network client must present first."""
        trusted = token is None
        is_agent = False
        try:
            while line := await reader.readline():
                try:
                    request = json.loads(line)
                    if not isinstance(request, dict):
                        raise ValueError("request must be a JSON object")
                except ValueError as exc:
                    reply = {"ok": False, "error": f"bad request: {exc}"}
                else:
                    command = request.get("cmd")
                    if command == "auth":
                        offered = str(request.get("token", "")).encode()
                        trusted = trusted or hmac.compare_digest(offered, (token or "").encode())
                        reply = {"ok": True} if trusted else {"ok": False, "error": "wrong token"}
                    elif not trusted:
                        reply = {"ok": False, "error": "not authorised"}
                    elif command == "audio":
                        if not is_agent:
                            is_agent = True
                            await self._agent_joined()
                        self._push_audio(request)
                        continue  # audio blocks are not acknowledged
                    else:
                        reply = await self.handle(request)
                writer.write(json.dumps(reply).encode() + b"\n")
                await writer.drain()
                if not trusted:
                    break
        except ConnectionError:
            pass
        finally:
            writer.close()
            if is_agent:
                await self._agent_left()


def _prepare_socket_dir(socket_path: Path) -> None:
    socket_path.parent.mkdir(parents=True, exist_ok=True)
    _remove(socket_path)  # left behind by a service that was killed


def _restrict(socket_path: Path) -> None:
    socket_path.chmod(0o600)


def _remove(socket_path: Path) -> None:
    socket_path.unlink(missing_ok=True)
