"""ChSmartBulb behaviour against a fake that mimics the real bulb."""

import asyncio

import pytest

from chsmartbulb import ChSmartBulb, Color, NativeEffect
from chsmartbulb.errors import ConnectionFailed, NotConnected, ProtocolError, RequestTimeout
from fakes import FakeBulbTransport


def run(coroutine):
    return asyncio.run(coroutine)


def test_connect_reads_state_and_colour_commands_reach_the_bulb():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            assert not bulb.is_on
            await bulb.set_rgb(255, 0, 100)
            assert transport.last_light == dict(r=255, g=0, b=100, w=0, y=0, speed=0, effect=0x50, fade=0)
            assert bulb.is_on
            state = await bulb.get_light_state()
            assert state.color == Color(255, 0, 100)
        assert not transport.is_open

    run(scenario())


def test_brightness_scales_channels_and_survives_colour_changes():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            await bulb.set_color(Color(r=200, g=100))
            await bulb.set_brightness(0.5)
            assert (transport.last_light["r"], transport.last_light["g"]) == (100, 50)
            await bulb.set_color(Color(b=255), fade=True)
            assert (transport.last_light["b"], transport.last_light["fade"]) == (128, 1)
            assert bulb.color == Color(b=255)

    run(scenario())


def test_off_sends_zeros_and_on_restores_the_colour():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            await bulb.set_color(Color(g=255))
            await bulb.turn_off()
            assert transport.channels == [0, 0, 0, 0, 0]
            assert not bulb.is_on
            sent = len(transport.light_bodies)
            await bulb.set_brightness(0.25)  # must not light the bulb while off
            assert len(transport.light_bodies) == sent
            await bulb.turn_on()
            assert transport.last_light["g"] == 64

    run(scenario())


def test_connect_adopts_the_colour_the_bulb_already_shows():
    async def scenario():
        transport = FakeBulbTransport()
        transport.channels = [0, 0, 0, 120, 0]  # white LEDs at partial level
        async with ChSmartBulb(transport) as bulb:
            assert bulb.is_on
            assert bulb.color == Color(w=255)  # the bulb reports the mix, not the level

    run(scenario())


def test_native_effect_sends_effect_and_packed_speed():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            await bulb.set_native_effect(NativeEffect.BREATHING, Color(r=255), speed=8)
            assert transport.last_light == dict(r=255, g=0, b=0, w=0, y=0, speed=0x82, effect=0x52, fade=0)
            await bulb.set_native_effect(NativeEffect.RAINBOW, Color(r=255), speed=0)
            assert transport.last_light["r"] == 0  # colourless effects send no colour

    run(scenario())


def test_queries_parse_answers_even_when_split_into_chunks():
    async def scenario():
        transport = FakeBulbTransport(chunk=20)  # BLE delivers answers in pieces
        async with ChSmartBulb(transport) as bulb:
            info = await bulb.get_info()
            assert (info.name, info.version, info.model, info.model_id) == (
                "SmartBulb Bluetooth", "1.0.", "BL04", 0x0C47,
            )  # fmt: skip
            timers = await bulb.get_timers()
            assert [t.name for t in timers] == ["power off", "power on"]
            assert len(await bulb.get_status_raw()) == 24

    run(scenario())


def test_timer_can_be_disabled_and_the_change_is_verified_by_reading_back():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            timer = await bulb.set_timer_enabled(6, False)
            assert (timer.name, timer.enabled, timer.hour, timer.minute, timer.days) == (
                "power off", False, 6, 20, 0x7F,
            )  # fmt: skip
            # same layout as the vendor app's frame: index, flag, name[32], 12-byte tail
            assert transport.timer_writes[-1].hex() == (
                "01fe000053303c00" "06000000" "00000000"
                + b"power off".ljust(32, b"\0").hex()
                + "0600007f0614000301000000"
            )  # fmt: skip
            with pytest.raises(ValueError):
                await bulb.set_timer_enabled(9, True)
            transport.ignore_timer_writes = True
            with pytest.raises(ProtocolError):
                await bulb.set_timer_enabled(6, True)

    run(scenario())


def test_reconnects_once_when_the_link_dropped():
    async def scenario():
        transport = FakeBulbTransport()
        async with ChSmartBulb(transport) as bulb:
            transport.drop_link()
            await asyncio.sleep(0)
            await bulb.set_color(Color(r=255))
            assert transport.opened == 2
            assert transport.last_light["r"] == 255
            transport.fail_next_write = True
            await bulb.set_color(Color(b=255))
            assert transport.opened == 3
            assert transport.last_light["b"] == 255

    run(scenario())


def test_without_auto_reconnect_a_lost_link_is_an_error():
    async def scenario():
        transport = FakeBulbTransport()
        bulb = ChSmartBulb(transport, auto_reconnect=False)
        await bulb.connect()
        transport.drop_link()
        await asyncio.sleep(0)
        with pytest.raises(NotConnected):
            await bulb.set_color(Color(r=255))
        await bulb.disconnect()

    run(scenario())


def test_errors_before_connect_and_on_failed_connect():
    async def scenario():
        transport = FakeBulbTransport()
        bulb = ChSmartBulb(transport)
        with pytest.raises(NotConnected):
            await bulb.set_color(Color(r=255))
        transport.fail_open = True
        with pytest.raises(ConnectionFailed):
            await bulb.connect()
        assert not bulb.is_connected

    run(scenario())


def test_query_times_out_when_the_bulb_stays_silent():
    async def scenario():
        from chsmartbulb import protocol as p

        transport = FakeBulbTransport()
        async with ChSmartBulb(transport, request_timeout=0.05) as bulb:
            with pytest.raises(RequestTimeout):
                await bulb.request(p.query(p.Command.RINGTONES))  # the fake does not answer this one

    run(scenario())
