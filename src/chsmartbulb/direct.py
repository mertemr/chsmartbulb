"""One request carried out on a bulb this process holds itself, when no service is running.

The requests are the service's. Nothing is remembered between two of them: what the
service keeps (the colour behind an effect, the brightness) is read from the bulb or
left alone here.
"""

from __future__ import annotations

import asyncio
import contextlib
from typing import TYPE_CHECKING, Any

from . import effects
from . import protocol as p
from .color import Color, parse_color
from .errors import SmartBulbError
from .protocol import NativeEffect

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

    from .bulb import ChSmartBulb
    from .music import Capture, Levels
    from .screen import ScreenFeed


def native_names() -> list[str]:
    return [effect.name.lower() for effect in NativeEffect if effect is not NativeEffect.FIXED]


def _timer(timer: p.Timer) -> dict[str, Any]:
    return {
        "index": timer.index,
        "name": timer.name,
        "enabled": timer.enabled,
        "days": timer.days,
        "hour": timer.hour,
        "minute": timer.minute,
    }


async def _dim(bulb: ChSmartBulb, level: Any) -> None:
    if level is not None:
        await bulb.set_brightness(float(level))


async def _shown(bulb: ChSmartBulb) -> Color:
    """The colour the bulb shows, at full scale: it reports the mix and not how bright it is."""
    return (await bulb.get_light_state()).color


async def run(
    bulb: ChSmartBulb,
    request: Mapping[str, Any],
    *,
    fps: float = effects.DEFAULT_FPS,
    music: Callable[[], Capture] | None = None,
    screen: Callable[[], ScreenFeed] | None = None,
) -> dict[str, Any]:
    """Connect, carry out ``request`` and return what a service would reply with.

    An ``effect`` request plays here until it ends or the caller is cancelled. ``music``
    and ``screen`` make the captures for the effects that follow one.
    """
    command = request["cmd"]
    playing = _prepare(request, music, screen) if command == "effect" else None  # refused before connecting
    async with bulb:
        if command == "status":
            return {"connected": True, "bulb": (await _shown(bulb)).to_hex()}
        if command == "on":
            # a new process does not know the colour that was showing when the light went off
            await bulb.turn_on(fade=bool(request.get("fade")))
        elif command == "off":
            await bulb.turn_off(fade=bool(request.get("fade")))
        elif command == "color":
            await _dim(bulb, request.get("brightness"))
            await bulb.set_color(parse_color(request["color"]), fade=bool(request.get("fade")))
        elif command == "brightness":
            color = await _shown(bulb)
            if color.is_off:
                raise SmartBulbError("the light is off")
            await _dim(bulb, request["level"])
            await bulb.set_color(color)
        elif command == "native":
            name = str(request["name"]).lower()
            if name not in native_names():
                raise ValueError(f"unknown native effect {name!r}")
            color = parse_color(request["color"]) if request.get("color") else await _shown(bulb)
            await _dim(bulb, request.get("brightness"))
            await bulb.set_native_effect(NativeEffect[name.upper()], color, speed=int(request.get("speed", 8)))
        elif command == "effect":
            assert playing is not None
            await _dim(bulb, request.get("brightness"))
            await _play(bulb, *playing, duration=request.get("duration"), fps=fps)
        elif command == "stop":
            await bulb.set_color(await _shown(bulb))  # any light command ends an effect of the bulb's own
        elif command == "info":
            info = await bulb.get_info()
            return {
                "name": info.name,
                "version": info.version,
                "model": info.model,
                "model_id": info.model_id,
                "device_id": info.device_id,
            }
        elif command == "timers":
            return {"timers": [_timer(timer) for timer in await bulb.get_timers()]}
        elif command == "timer":
            return {"timer": _timer(await bulb.set_timer_enabled(int(request["index"]), bool(request["enabled"])))}
        elif command == "raw":
            frame = p.Frame.decode(bytes.fromhex(request["hex"]))
            if frame.type == p.FrameType.QUERY:
                return {"answer": (await bulb.request(frame)).encode().hex()}
            await bulb.send(frame)
        else:
            raise ValueError(f"unknown command {command!r}")
    return {}


def _prepare(
    request: Mapping[str, Any],
    music: Callable[[], Capture] | None,
    screen: Callable[[], ScreenFeed] | None,
) -> tuple[effects.Effect, list[Any]]:
    """The effect asked for and the captures it follows, not started yet."""
    name = request["name"]
    info = next((info for info in effects.describe() if info["name"] == name), None)
    follows = set() if info is None else {info.get("needs"), info.get("also")}
    captures: list[Any] = []
    heard = seen = None
    if "audio" in follows:
        if music is None:
            raise SmartBulbError(f"effect {name!r} follows the sound and nothing captures it here")
        heard, listening = effects.audio_source(), music()

        def forward(levels: Levels, onset: float) -> None:
            heard.publish(levels.bass, levels.mid, levels.treble, levels.balance, onset)

        listening.on_block = forward
        captures.append(listening)
    if "screen" in follows:
        if screen is None:
            raise SmartBulbError(f"effect {name!r} follows the screen and nothing captures it here")
        seen, watching = effects.screen_source(), screen()

        def show(color: Color) -> None:
            seen.push(color.r, color.g, color.b)

        watching.on_color = show
        captures.append(watching)
    return effects.create(name, request.get("params"), audio=heard, screen=seen), captures


async def _play(bulb: ChSmartBulb, effect: effects.Effect, captures: list[Any], *, duration: Any, fps: float) -> None:
    tasks: list[asyncio.Future[None]] = []
    try:
        for capture in captures:
            await capture.start()
            tasks.append(asyncio.ensure_future(capture.wait()))
        tasks.append(asyncio.ensure_future(effects.play(bulb, effect, duration=duration, fps=fps)))
        done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
        for task in done:
            task.result()  # a capture that failed ends the effect with its error
    finally:
        for task in tasks:
            task.cancel()
        for capture in captures:
            await capture.stop()
        with contextlib.suppress(asyncio.CancelledError):
            await asyncio.gather(*tasks, return_exceptions=True)
