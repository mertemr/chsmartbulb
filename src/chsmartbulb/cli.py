"""Command line front end: ``chsmartbulb --address AA:BB:CC:DD:EE:FF <command>``.

Commands go to the background service when one is running (or to one on another
machine with ``--host``), and straight to the bulb otherwise.
"""

from __future__ import annotations

import argparse
import asyncio
import functools
import logging
import os
import sys
from pathlib import Path
from typing import Any

from . import catalog, client, effects, service, web
from . import protocol as p
from .bulb import ChSmartBulb
from .color import NAMED, Color, parse_color
from .errors import SmartBulbError
from .music import BACKENDS, DEFAULT_DEVICE, MusicSource
from .protocol import NativeEffect
from .screen import PRIMARY, ScreenCapture

ENV_ADDRESS = "CHSMARTBULB_ADDRESS"
ENV_TOKEN = "CHSMARTBULB_TOKEN"
_DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]
_AGENTS = ("audio-agent", "screen-agent")


def _color(text: str) -> Color:
    try:
        return parse_color(text)
    except ValueError as exc:
        raise argparse.ArgumentTypeError(str(exc)) from None


def _percent(text: str) -> float:
    value = float(text)
    if not 0 <= value <= 100:
        raise argparse.ArgumentTypeError("must be within 0..100")
    return value / 100


def _assignment(text: str) -> tuple[str, str]:
    key, separator, value = text.partition("=")
    if not separator or not key:
        raise argparse.ArgumentTypeError("expected NAME=VALUE")
    return key, value


def _effect_params(args: argparse.Namespace) -> dict[str, Any]:
    params: dict[str, Any] = dict(args.set or [])
    if args.color is not None:
        params["color"] = args.color.to_hex()
    if args.period is not None:
        params["period"] = args.period
    catalog.resolve(args.name, params)
    return params


def _request(args: argparse.Namespace) -> dict[str, Any]:
    """Translate parsed arguments into a service request."""
    command = args.command
    if command in ("on", "off"):
        return {"cmd": command, "fade": args.fade}
    if command == "color":
        return {"cmd": "color", "color": args.value.to_hex(), "fade": args.fade, "brightness": args.brightness}
    if command == "rgb":
        color = Color(args.r, args.g, args.b, args.white)
        return {"cmd": "color", "color": color.to_hex(), "fade": args.fade, "brightness": args.brightness}
    if command == "white":
        return {"cmd": "color", "color": Color(w=round(255 * args.level)).to_hex()}
    if command == "brightness":
        return {"cmd": "brightness", "level": args.level}
    if command == "native":
        color = None if args.color is None else args.color.to_hex()
        return {"cmd": "native", "name": args.name, "color": color, "speed": args.speed, "brightness": args.brightness}
    if command == "effect":
        return {
            "cmd": "effect",
            "name": args.name,
            "params": _effect_params(args),
            "duration": args.duration,
            "brightness": args.brightness,
        }
    if command == "timer":
        return {"cmd": "timer", "index": args.index, "enabled": args.state == "on"}
    if command == "raw":
        return {"cmd": "raw", "hex": args.hex}
    return {"cmd": command}  # status, stop, effects, info, timers


def _print_timer(timer: dict[str, Any]) -> None:
    days = ",".join(day for i, day in enumerate(_DAYS) if timer["days"] >> i & 1) or "-"
    state = "enabled" if timer["enabled"] else "disabled"
    print(f"#{timer['index']} {timer['name']!r} {timer['hour']:02d}:{timer['minute']:02d} days={days} {state}")


def _print(command: str, reply: dict[str, Any]) -> None:
    if command == "status":
        print(f"bulb:       {'connected' if reply['connected'] else 'not connected'}")
        print(f"audio:      {reply['audio']}")
        if "screen" in reply:
            print(f"screen:     {reply['screen']}")
        print(f"light:      {'on' if reply['on'] else 'off'}")
        print(f"colour:     {reply['color']}")
        print(f"brightness: {round(reply['brightness'] * 100)}%")
        if reply["effect"]:
            running = "" if reply["playing"] else " (not running)"
            print(f"effect:     {reply['effect']['name']} {reply['effect']['params'] or ''}{running}")
        if reply["native"]:
            print(f"native:     {reply['native']['name']} speed {reply['native']['speed']}")
        if "bulb" in reply:
            print(f"reported:   {reply['bulb']} (colour mix; the bulb does not report brightness)")
    elif command == "info":
        print(f"name:      {reply['name']}")
        print(f"version:   {reply['version']}")
        print(f"model:     {reply['model']} (protocol id 0x{reply['model_id']:04x})")
        print(f"device id: 0x{reply['device_id']:04x}")
    elif command == "timers":
        if not reply["timers"]:
            print("no timers stored")
        for timer in reply["timers"]:
            _print_timer(timer)
    elif command == "timer":
        timer = reply["timer"]
        print(f"#{timer['index']} {timer['name']!r} is now {'enabled' if timer['enabled'] else 'disabled'}")
    elif command == "effects":
        for info in reply["effects"]:
            params = ", ".join(f"{key}={value}" for key, value in info["params"].items())
            needs = f" [{info['needs']}]" if info["needs"] else ""
            print(f"{info['name']:9s} {info['summary']}{needs}\n          {params}")
    elif command == "raw" and "answer" in reply:
        print(reply["answer"])


def _bulb(args: argparse.Namespace, **options: Any) -> ChSmartBulb:
    if not args.address:
        raise SmartBulbError(f"no address given: use --address or set ${ENV_ADDRESS}")
    if args.transport == "ble":
        return ChSmartBulb.ble(args.address, **options)
    return ChSmartBulb.rfcomm(args.address, args.channel, **options)


def _music(args: argparse.Namespace) -> functools.partial[MusicSource]:
    return functools.partial(MusicSource, device=args.audio_device, backend=args.audio_backend)


def _screen(args: argparse.Namespace) -> functools.partial[ScreenCapture]:
    return functools.partial(ScreenCapture, monitor=args.monitor)


def _listen(text: str) -> tuple[str, int]:
    host, separator, port = text.rpartition(":")
    if not port.isdigit():
        raise argparse.ArgumentTypeError("expected PORT or HOST:PORT")
    return (host if separator else "0.0.0.0", int(port))


def _remote(args: argparse.Namespace) -> client.Remote:
    if not args.token:
        raise SmartBulbError(f"--host needs the service's token: use --token or set ${ENV_TOKEN}")
    return client.Remote.parse(args.host, args.token)


async def _daemon(args: argparse.Namespace) -> None:
    for option in ("listen", "web"):
        if getattr(args, option) is not None and not args.token:
            raise SmartBulbError(f"--{option} needs a token: use --token or set ${ENV_TOKEN}")
    bulb = _bulb(args, auto_reconnect=False)
    state_path = None if args.no_state else service.default_state_path()
    daemon = service.BulbService(
        bulb, state_path=state_path, fps=args.fps, music_factory=_music(args), screen_factory=_screen(args)
    )
    await daemon.serve(args.socket, listen=args.listen, web=args.web, token=args.token)


async def _agent(args: argparse.Namespace) -> None:
    target = _remote(args) if args.host else args.socket
    if args.command == "screen-agent":
        _screen(args)()  # fail now if capture cannot work on this machine
        await client.run_screen_agent(target, source_factory=_screen(args))
        return
    _music(args)()
    await client.run_agent(target, source_factory=_music(args))


async def _direct(args: argparse.Namespace, request: dict[str, Any]) -> dict[str, Any]:
    fps = getattr(args, "fps", effects.DEFAULT_FPS)
    direct = service.BulbService(_bulb(args), fps=fps, music_factory=_music(args), screen_factory=_screen(args))
    await direct.attach()
    try:
        reply = await direct.handle(request)
        if reply["ok"] and request["cmd"] == "effect":
            await direct.wait_effect()  # no service to keep it going, so run it here
        return reply
    finally:
        await direct.close()


async def _run(args: argparse.Namespace) -> None:
    if args.command == "daemon":
        await _daemon(args)
        return
    if args.command in _AGENTS:
        await _agent(args)
        return
    request = _request(args)
    if request["cmd"] == "effects":
        reply: dict[str, Any] = {"ok": True, "effects": catalog.describe()}
    elif args.host:
        reply = await client.call(_remote(args), request)
    elif not args.direct and await client.is_running(args.socket):
        reply = await client.call(args.socket, request)
    else:
        reply = await _direct(args, request)
    if not reply["ok"]:
        raise SmartBulbError(reply["error"])
    _print(args.command, reply)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="chsmartbulb", description="Control a CHSmartBulb / BL08A bulb.")
    parser.add_argument(
        "-a",
        "--address",
        default=os.environ.get(ENV_ADDRESS),
        help=f"Bluetooth address of the bulb (or set ${ENV_ADDRESS})",
    )
    parser.add_argument("-t", "--transport", choices=("rfcomm", "ble"), default="rfcomm")
    parser.add_argument("--channel", type=int, default=p.RFCOMM_CHANNEL, help="RFCOMM channel (default 2)")
    parser.add_argument("--socket", type=Path, default=service.default_socket_path(), help="service socket path")
    parser.add_argument("--direct", action="store_true", help="talk to the bulb even if a service is running")
    parser.add_argument("--host", metavar="HOST[:PORT]", help="use the service on another machine")
    parser.add_argument(
        "--token", default=os.environ.get(ENV_TOKEN), help=f"shared secret for network access (or set ${ENV_TOKEN})"
    )
    parser.add_argument(
        "--audio-device",
        default=DEFAULT_DEVICE,
        metavar="SOURCE",
        help="audio source for sound-reactive effects (default: monitor of the default output)",
    )
    parser.add_argument("--audio-backend", choices=BACKENDS, default="auto", help="how the audio is captured")
    parser.add_argument(
        "--monitor", type=int, default=PRIMARY, metavar="N", help="which monitor the screen effect follows; 0 is all"
    )
    parser.add_argument("-v", "--verbose", action="store_true")
    sub = parser.add_subparsers(dest="command", required=True)

    def with_brightness(cmd: argparse.ArgumentParser) -> None:
        cmd.add_argument("-b", "--brightness", type=_percent, default=None, metavar="PCT", help="0..100")

    cmd = sub.add_parser("daemon", help="run the background service in the foreground")
    cmd.add_argument("--fps", type=float, default=effects.DEFAULT_FPS, help="effect frame rate")
    cmd.add_argument("--no-state", action="store_true", help="do not remember the light state across restarts")
    cmd.add_argument(
        "--listen",
        type=_listen,
        metavar="[HOST:]PORT",
        help=f"also accept other machines on this address (needs a token; usual port {client.DEFAULT_PORT})",
    )
    cmd.add_argument(
        "--web",
        type=_listen,
        metavar="[HOST:]PORT",
        help=f"also serve the web interface on this address (needs a token; usual port {web.DEFAULT_PORT})",
    )

    sub.add_parser("audio-agent", help="analyse this machine's audio and feed it to a service (see --host)")
    sub.add_parser("screen-agent", help="watch this machine's screen and feed its colour to a service (see --host)")

    sub.add_parser("status", help="show the connection, the light state and the running effect")
    sub.add_parser("info", help="show name, version and model")
    for name in ("on", "off"):
        cmd = sub.add_parser(name, help=f"turn the light {name}")
        cmd.add_argument("--fade", action="store_true", help="soft one-second transition")

    cmd = sub.add_parser("color", help="set a colour by name or hex (rrggbb or rrggbbww)")
    cmd.add_argument("value", type=_color, metavar="COLOR", help=f"hex or one of: {', '.join(NAMED)}")
    cmd.add_argument("--fade", action="store_true")
    with_brightness(cmd)

    cmd = sub.add_parser("rgb", help="set a colour by channel values 0..255")
    for channel in ("r", "g", "b"):
        cmd.add_argument(channel, type=int)
    cmd.add_argument("-w", "--white", type=int, default=0, help="white LED level 0..255")
    cmd.add_argument("--fade", action="store_true")
    with_brightness(cmd)

    cmd = sub.add_parser("white", help="white LEDs only")
    cmd.add_argument("level", type=_percent, nargs="?", default=1.0, metavar="PCT")

    cmd = sub.add_parser("brightness", help="dim whatever is showing, effects included")
    cmd.add_argument("level", type=_percent, metavar="PCT")

    cmd = sub.add_parser("native", help="start an effect built into the bulb")
    cmd.add_argument("name", choices=[e.name.lower() for e in NativeEffect if e is not NativeEffect.FIXED])
    cmd.add_argument("-c", "--color", type=_color, default=None)
    cmd.add_argument(
        "-s", "--speed", type=int, default=8, choices=range(16), metavar="0..15", help="0 fastest, 15 slowest"
    )
    with_brightness(cmd)

    cmd = sub.add_parser("effect", help="run an effect generated on this computer")
    cmd.add_argument("name", choices=list(catalog.CATALOG))
    cmd.add_argument("-c", "--color", type=_color, default=None)
    cmd.add_argument("-p", "--period", type=float, default=None, help="seconds per cycle")
    cmd.add_argument("-s", "--set", type=_assignment, action="append", metavar="NAME=VALUE", help="other parameters")
    cmd.add_argument("-d", "--duration", type=float, default=None, help="stop after this many seconds")
    cmd.add_argument("--fps", type=float, default=effects.DEFAULT_FPS, help="frame rate when no service is running")
    with_brightness(cmd)

    sub.add_parser("effects", help="list the available effects and their parameters")
    sub.add_parser("stop", help="stop the running effect and return to the plain colour")
    sub.add_parser("timers", help="list schedule entries stored in the bulb")
    cmd = sub.add_parser("timer", help="enable or disable a stored schedule entry")
    cmd.add_argument("index", type=int)
    cmd.add_argument("state", choices=("on", "off"))
    cmd = sub.add_parser("raw", help="send one raw frame given as hex; prints the answer to queries")
    cmd.add_argument("hex")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    quiet = logging.INFO if args.command in ("daemon", *_AGENTS) else logging.WARNING
    logging.basicConfig(level=logging.DEBUG if args.verbose else quiet, format="%(levelname)s %(message)s")
    try:
        asyncio.run(_run(args))
    except KeyboardInterrupt:
        return 130
    except asyncio.CancelledError:
        return 0  # the service was asked to stop
    except (SmartBulbError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
