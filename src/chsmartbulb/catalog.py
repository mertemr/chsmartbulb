"""Effects that can be requested by name with plain parameters (JSON, command line)."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from . import effects, music, screen
from .color import BLUE, GREEN, RED, Color, parse_color

if TYPE_CHECKING:
    from collections.abc import Callable, Mapping

    from .effects import Effect


@dataclass(frozen=True)
class EffectInfo:
    name: str
    summary: str
    build: Callable[..., Effect]
    defaults: Mapping[str, Any]
    needs: str | None = None  # the input the effect follows: "audio" or "screen"
    ranges: Mapping[str, tuple[float, float, float]] | None = None  # where a slider differs from _RANGES


#: Slider limits a front end offers per parameter: lowest, highest, step. The builders
#: still decide what is valid; these only keep a control within a useful span.
_RANGES: dict[str, tuple[float, float, float]] = {
    "period": (0.2, 60.0, 0.1),
    "floor": (0.0, 1.0, 0.01),
    "saturation": (0.0, 1.0, 0.01),
    "decay": (0.5, 20.0, 0.5),
    "hz": (0.5, 10.0, 0.5),
    "duty": (0.05, 0.95, 0.05),
    "depth": (0.0, 1.0, 0.01),
    "hold": (0.0, 30.0, 0.5),
    "fade_in": (0.0, 30.0, 0.5),
    "delay": (0.0, music.MAX_DELAY, 0.01),
    "sensitivity": (0.0, 1.0, 0.01),
    "release": (0.5, 10.0, 0.5),
    "width": (1.0, 10.0, 0.5),
    "smoothing": (0.0, 2.0, 0.05),
    "white": (0.0, 1.0, 0.01),
    "balance": (0.0, 1.0, 0.01),
    "speed": (0.1, 5.0, 0.1),
}

_CUSTOM_STEPS = tuple(
    {"color": color, "hold": 1.0, "fade": 1.0, "ease": "ease-in-out"} for color in ("#ff0000", "#ff8000", "#0040ff")
)


def _police(period: float) -> Effect:
    return effects.sequence([(RED, period / 2), (BLUE, period / 2)])


_ENTRIES = [
    EffectInfo("breathe", "swell and fade", effects.breathe, {"color": RED, "period": 4.0, "floor": 0.0}),
    EffectInfo("hue", "walk around the colour wheel", effects.hue_cycle, {"period": 10.0, "saturation": 1.0}),
    EffectInfo("pulse", "flash, then decay", effects.pulse, {"color": RED, "period": 1.0, "decay": 4.0}),
    EffectInfo("strobe", "hard blinking", effects.strobe, {"color": RED, "hz": 5.0, "duty": 0.5}),
    EffectInfo("candle", "uneven flicker", effects.candle, {"color": effects.WARM, "depth": 0.6}),
    EffectInfo(
        "palette",
        "drift through a list of colours",
        effects.palette,
        {"colors": (RED, GREEN, BLUE), "hold": 2.0, "fade_in": 1.0},
    ),
    EffectInfo("police", "alternate red and blue", _police, {"period": 1.0}),
    EffectInfo(
        "custom",
        "your own colours, timings and fades",
        effects.custom,
        {"steps": _CUSTOM_STEPS, "speed": 1.0},
    ),
    EffectInfo(
        "music",
        "flash on the beat of the computer's audio",
        music.music_pulse,
        {"color": None, "decay": 5.0, "delay": 0.0, "sensitivity": music.DEFAULT_SENSITIVITY},
        needs="audio",
    ),
    EffectInfo(
        "spectrum",
        "bass, mids, treble as red, green, blue",
        music.music_spectrum,
        {"release": 3.0, "delay": 0.0},
        needs="audio",
    ),
    EffectInfo(
        "volume",
        "one colour, as bright as the audio is loud",
        music.music_volume,
        {"color": RED, "release": 3.0, "floor": 0.0, "delay": 0.0},
        needs="audio",
    ),
    EffectInfo(
        "stereo",
        "blend two colours by where the sound sits between left and right",
        music.music_stereo,
        {"left": BLUE, "right": RED, "width": 4.0, "release": 3.0, "delay": 0.0},
        needs="audio",
    ),
    EffectInfo(
        "screen",
        "follow the colour of the screen",
        screen.screen_follow,
        {"smoothing": 0.2, "saturation": 1.5, "white": 1.0, "balance": 0.0},
        needs="screen",
        ranges={"saturation": (0.0, 3.0, 0.1)},
    ),
]

CATALOG: dict[str, EffectInfo] = {info.name: info for info in _ENTRIES}


def _as_color(value: Any) -> Color:
    return value if isinstance(value, Color) else parse_color(str(value))


def _coerce(key: str, default: Any, value: Any) -> Any:
    if key == "color" or isinstance(default, Color):
        return None if value is None and default is None else _as_color(value)
    if key == "colors":
        items = value.split(",") if isinstance(value, str) else value
        colors = tuple(_as_color(item) for item in items)
        if not colors:
            raise ValueError("colors must not be empty")
        return colors
    if key == "steps":
        if isinstance(value, str):  # from the command line
            try:
                value = json.loads(value)
            except ValueError:
                value = None
        if not isinstance(value, (list, tuple)):
            raise ValueError("steps must be a list of steps, as JSON when given as text")
        return tuple(value)  # the effect checks each step when it is built
    if isinstance(default, float) and not isinstance(value, bool):
        try:
            return float(value)
        except (TypeError, ValueError):
            pass
    raise ValueError(f"{key} must be a number, got {value!r}")


def resolve(name: str, params: Mapping[str, Any] | None = None) -> dict[str, Any]:
    """Validate ``params`` for effect ``name`` and fill in the defaults."""
    info = CATALOG.get(name)
    if info is None:
        raise ValueError(f"unknown effect {name!r}; available: {', '.join(CATALOG)}")
    resolved = dict(info.defaults)
    for key, value in (params or {}).items():
        if key not in info.defaults:
            raise ValueError(f"effect {name!r} has no parameter {key!r}; it takes: {', '.join(info.defaults)}")
        resolved[key] = _coerce(key, info.defaults[key], value)
    return resolved


def create(
    name: str,
    params: Mapping[str, Any] | None = None,
    *,
    audio: music.AudioSource | None = None,
    screen: screen.ScreenSource | None = None,
) -> Effect:
    """Build the named effect. Those that follow sound or the screen need that source, running."""
    resolved = resolve(name, params)
    info = CATALOG[name]
    if info.needs is None:
        return info.build(**resolved)
    source = audio if info.needs == "audio" else screen
    if source is None:
        raise ValueError(f"effect {name!r} was given no {info.needs} source to follow")
    return info.build(source, **resolved)


def _plain(value: Any) -> Any:
    if isinstance(value, Color):
        return value.to_hex()
    if isinstance(value, tuple):
        return [_plain(item) for item in value]
    return value


def _schema(info: EffectInfo, key: str, default: Any) -> dict[str, Any]:
    if key == "colors":
        return {"type": "colors"}
    if key == "steps":
        return {"type": "steps", "most": effects.MAX_STEPS, "easings": list(effects.EASINGS)}
    if key == "color" or isinstance(default, Color):
        return {"type": "color", "optional": default is None}
    low, high, step = (info.ranges or {}).get(key) or _RANGES[key]
    return {"type": "number", "min": low, "max": high, "step": step}


def describe() -> list[dict[str, Any]]:
    """The catalog as plain data, for listings and for front ends that build their controls from it."""
    return [
        {
            "name": info.name,
            "summary": info.summary,
            "params": {key: _plain(value) for key, value in info.defaults.items()},
            "schema": {key: _schema(info, key, value) for key, value in info.defaults.items()},
            "needs": info.needs,
        }
        for info in CATALOG.values()
    ]
