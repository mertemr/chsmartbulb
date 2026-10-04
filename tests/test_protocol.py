"""Protocol codec checked against bytes captured from the real bulb."""

import datetime as dt

import pytest

from chsmartbulb import protocol as p
from chsmartbulb.color import Color
from chsmartbulb.errors import ProtocolError
from fakes import HARDWARE_ANSWER, NAME_ANSWER, TIMERS_ANSWER


def test_query_frames_match_the_vendor_app():
    assert p.query(p.Command.IDENTIFY).encode().hex() == "01fe0000510210000000008000000080"
    assert p.query(p.Command.LIGHT_STATE).encode().hex() == "01fe0000518210000000000000000000"
    assert p.query(p.Command.STATUS).encode().hex() == "01fe0000510010000000008000000080"
    assert p.query(p.Command.ALARMS).encode().hex() == "01fe000051321000ffffffff00000080"


def test_light_frame_orders_channels_green_blue_red():
    assert p.light(Color(r=255)).encode().hex() == "01fe0000538310000000ff0050000000"
    assert p.light(Color(g=255)).encode().hex() == "01fe000053831000ff00000050000000"
    assert p.light(Color(b=255)).encode().hex() == "01fe00005383100000ff000050000000"
    assert p.light(Color(w=255)).encode().hex() == "01fe0000538310000000000050ff0000"


def test_light_frame_fade_effect_and_speed():
    frame = p.light(Color(r=1, g=2, b=3), effect=p.NativeEffect.BREATHING, speed=0x82, fade=True)
    assert frame.body.hex() == "0203018252000001"


def test_speed_byte_uses_the_vendor_packing():
    assert [p.speed_byte(p.NativeEffect.BREATHING, i) for i in (0, 4, 8, 15)] == [0x00, 0x41, 0x82, 0xF3]
    assert p.speed_byte(p.NativeEffect.HEARTBEAT, 8) == 0x80
    assert p.speed_byte(p.NativeEffect.FIXED, 8) == 0
    with pytest.raises(ValueError):
        p.speed_byte(p.NativeEffect.FLASH, 16)


def test_set_clock_matches_capture():
    frame = p.set_clock(dt.datetime(2017, 11, 11, 17, 46, 10))
    assert frame.encode().hex() == "01fe0000530018000000000000000080e1070b0b112e0a00"


def test_frame_round_trip_and_length_check():
    frame = p.Frame(p.FrameType.ANSWER, 0x82, bytes(range(8)))
    assert p.Frame.decode(frame.encode()) == frame
    with pytest.raises(ProtocolError):
        p.Frame.decode(frame.encode() + b"\0")
    with pytest.raises(ProtocolError):
        p.Frame.decode(b"01234567")


def test_reader_reassembles_split_and_concatenated_frames():
    reader = p.FrameReader()
    stream = TIMERS_ANSWER + HARDWARE_ANSWER
    frames = []
    for i in range(0, len(stream), 7):
        frames += reader.feed(stream[i : i + 7])
    assert [f.command for f in frames] == [p.Command.TIMERS, p.Command.HARDWARE]
    assert frames[0].encode() == TIMERS_ANSWER


def test_reader_resyncs_after_garbage():
    reader = p.FrameReader()
    frames = reader.feed(b"\x00junk\x01\xfe" + HARDWARE_ANSWER)
    assert [f.command for f in frames] == [p.Command.HARDWARE]


def test_parse_light_state():
    state = p.parse_light_state(p.Frame.decode(bytes.fromhex("01fe000041821000b66e260000ff0000")))
    assert state.color == Color(r=0x26, g=0xB6, b=0x6E, w=0xFF)
    assert state.is_on
    dark = p.parse_light_state(p.Frame.decode(bytes.fromhex("01fe0000418210000000000000000000")))
    assert not dark.is_on


def test_parse_identity_answers():
    assert p.parse_model_id(p.Frame.decode(bytes.fromhex("01fe000041021000470c000000000000"))) == p.MODEL_ID
    name = p.parse_name(p.Frame.decode(NAME_ANSWER))
    assert (name.name, name.version) == ("SmartBulb Bluetooth", "1.0.")
    hardware = p.parse_hardware(p.Frame.decode(HARDWARE_ANSWER))
    assert (hardware.model, hardware.device_id) == ("BL04", 0x587C)


def test_parse_timers():
    off, on = p.parse_timers(p.Frame.decode(TIMERS_ANSWER))
    assert (off.name, off.index, off.enabled, off.days, off.hour, off.minute) == ("power off", 6, True, 0x7F, 6, 20)
    assert (on.name, on.index, on.enabled, on.hour, on.minute) == ("power on", 5, False, 10, 35)


def test_parsers_reject_the_wrong_answer():
    with pytest.raises(ProtocolError):
        p.parse_light_state(p.Frame.decode(HARDWARE_ANSWER))
