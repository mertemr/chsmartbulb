"""Command line front end: ``chsmartbulb --address AA:BB:CC:DD:EE:FF <command>``."""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import sys

from . import effects
from . import protocol as p
from .bulb import ChSmartBulb
from .color import NAMED, Color, parse_color
from .errors import SmartBulbError
from .protocol import NativeEffect

ENV_ADDRESS = "CHSMARTBULB_ADDRESS"
_DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]


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


def _host_effect(args: argparse.Namespace) -> effects.Effect:
    color = args.color
    if args.name == "breathe":
        return effects.breathe(color, args.period)
    if args.name == "hue":
        return effects.hue_cycle(args.period)
    if args.name == "pulse":
        return effects.pulse(color, args.period)
    if args.name == "strobe":
        return effects.strobe(color, 1 / args.period)
    # "police": a custom sequence, as an example of sequence()
    return effects.sequence([(Color(r=255), args.period / 2), (Color(b=255), args.period / 2)])


async def _run(args: argparse.Namespace) -> None:
    bulb = ChSmartBulb.ble(args.address) if args.transport == "ble" else ChSmartBulb.rfcomm(args.address, args.channel)
    async with bulb:
        command = args.command
        if command == "info":
            info = await bulb.get_info()
            print(f"name:      {info.name}")
            print(f"version:   {info.version}")
            print(f"model:     {info.model} (protocol id 0x{info.model_id:04x})")
            print(f"device id: 0x{info.device_id:04x}")
        elif command == "state":
            state = await bulb.get_light_state()
            print("on" if state.is_on else "off")
            print(
                f"colour mix (brightness is not reported): {state.color.to_hex()} "
                f"r={state.color.r} g={state.color.g} b={state.color.b} w={state.color.w}"
            )
        elif command == "on":
            await bulb.turn_on(fade=args.fade)
        elif command == "off":
            await bulb.turn_off(fade=args.fade)
        elif command == "color":
            await bulb.set_brightness(args.brightness)
            await bulb.set_color(args.value, fade=args.fade)
        elif command == "rgb":
            await bulb.set_brightness(args.brightness)
            await bulb.set_rgb(args.r, args.g, args.b, args.white, fade=args.fade)
        elif command == "white":
            await bulb.set_white(round(255 * args.level))
        elif command == "brightness":
            if not bulb.is_on:
                raise SmartBulbError("the light is off; set a colour first")
            await bulb.set_brightness(args.level)
        elif command == "native":
            await bulb.set_brightness(args.brightness)
            await bulb.set_native_effect(NativeEffect[args.name.upper()], args.color, speed=args.speed)
        elif command == "effect":
            await bulb.set_brightness(args.brightness)
            await effects.play(bulb, _host_effect(args), duration=args.duration, fps=args.fps)
        elif command == "timers":
            timers = await bulb.get_timers()
            if not timers:
                print("no timers stored")
            for timer in timers:
                days = ",".join(d for i, d in enumerate(_DAYS) if timer.days >> i & 1) or "-"
                state = "enabled" if timer.enabled else "disabled"
                print(f"#{timer.index} {timer.name!r} {timer.hour:02d}:{timer.minute:02d} days={days} {state}")
        elif command == "timer":
            timer = await bulb.set_timer_enabled(args.index, args.state == "on")
            print(f"#{timer.index} {timer.name!r} is now {'enabled' if timer.enabled else 'disabled'}")
        elif command == "raw":
            data = bytes.fromhex(args.hex)
            frame = p.Frame.decode(data)
            if frame.type == p.FrameType.QUERY:
                print((await bulb.request(frame)).encode().hex())
            else:
                await bulb.send(frame)


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
    parser.add_argument("-v", "--verbose", action="store_true")
    sub = parser.add_subparsers(dest="command", required=True)

    def with_brightness(cmd: argparse.ArgumentParser) -> None:
        cmd.add_argument("-b", "--brightness", type=_percent, default=1.0, metavar="PCT", help="0..100")

    sub.add_parser("info", help="show name, version and model")
    sub.add_parser("state", help="show whether the light is on and its colour mix")
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

    cmd = sub.add_parser("brightness", help="dim the current colour")
    cmd.add_argument("level", type=_percent, metavar="PCT")

    cmd = sub.add_parser("native", help="start an effect built into the bulb")
    cmd.add_argument("name", choices=[e.name.lower() for e in NativeEffect if e is not NativeEffect.FIXED])
    cmd.add_argument("-c", "--color", type=_color, default=Color(r=255))
    cmd.add_argument(
        "-s", "--speed", type=int, default=8, choices=range(16), metavar="0..15", help="0 fastest, 15 slowest"
    )
    with_brightness(cmd)

    cmd = sub.add_parser("effect", help="run an effect generated on this computer (Ctrl+C stops)")
    cmd.add_argument("name", choices=("breathe", "hue", "pulse", "strobe", "police"))
    cmd.add_argument("-c", "--color", type=_color, default=Color(r=255))
    cmd.add_argument("-p", "--period", type=float, default=4.0, help="seconds per cycle")
    cmd.add_argument("-d", "--duration", type=float, default=None, help="stop after this many seconds")
    cmd.add_argument("--fps", type=float, default=effects.DEFAULT_FPS)
    with_brightness(cmd)

    sub.add_parser("timers", help="list schedule entries stored in the bulb")
    cmd = sub.add_parser("timer", help="enable or disable a stored schedule entry")
    cmd.add_argument("index", type=int)
    cmd.add_argument("state", choices=("on", "off"))
    cmd = sub.add_parser("raw", help="send one raw frame given as hex; prints the answer to queries")
    cmd.add_argument("hex")
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    if not args.address:
        parser.error(f"no address given: use --address or set ${ENV_ADDRESS}")
    logging.basicConfig(level=logging.DEBUG if args.verbose else logging.WARNING)
    try:
        asyncio.run(_run(args))
    except KeyboardInterrupt:
        return 130
    except (SmartBulbError, ValueError) as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
