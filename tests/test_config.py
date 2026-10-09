import pytest

from chsmartbulb import cli, config
from chsmartbulb.cli import main
from chsmartbulb.errors import SmartBulbError


def write(text: str):
    path = config.default_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    return path


def test_settings_file_lives_where_the_platform_keeps_settings(monkeypatch, tmp_path):
    monkeypatch.setattr(config.sys, "platform", "linux")
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "xdg"))
    assert config.default_path() == tmp_path / "xdg" / "chsmartbulb" / "config"
    monkeypatch.delenv("XDG_CONFIG_HOME")
    monkeypatch.setattr(config.Path, "home", lambda: tmp_path)
    assert config.default_path() == tmp_path / ".config" / "chsmartbulb" / "config"

    monkeypatch.setattr(config.sys, "platform", "win32")
    monkeypatch.setenv("APPDATA", str(tmp_path / "roaming"))
    assert config.default_path() == tmp_path / "roaming" / "chsmartbulb" / "config"
    monkeypatch.delenv("APPDATA")
    assert config.default_path() == tmp_path / "AppData" / "Roaming" / "chsmartbulb" / "config"


def test_lines_are_read_with_comments_quotes_and_export():
    text = """
        # the bulb in the study
        CHSMARTBULB_ADDRESS=AA:BB:CC:DD:EE:FF
        export CHSMARTBULB_TOKEN = "se=cret"
        CHSMARTBULB_AUDIO_DEVICE='Speakers #2'
        CHSMARTBULB_HOST=
    """
    assert config.parse(text) == {
        "CHSMARTBULB_ADDRESS": "AA:BB:CC:DD:EE:FF",
        "CHSMARTBULB_TOKEN": "se=cret",
        "CHSMARTBULB_AUDIO_DEVICE": "Speakers #2",
        "CHSMARTBULB_HOST": "",
    }


@pytest.mark.parametrize("line", ["CHSMARTBULB_ADRESS=AA:BB", "just words", "PATH=/usr/bin"])
def test_a_line_that_sets_nothing_known_is_named(line):
    with pytest.raises(SmartBulbError, match=r"my file:2: .*CHSMARTBULB_ADDRESS"):
        config.parse(f"CHSMARTBULB_MONITOR=1\n{line}\n", "my file")


def test_environment_overrides_the_file_and_empty_values_count_as_unset(monkeypatch):
    for name in config.NAMES:
        monkeypatch.delenv(name, raising=False)
    assert config.load() == {}  # no file yet
    write("﻿CHSMARTBULB_ADDRESS=AA:AA\nCHSMARTBULB_TOKEN=filed\nCHSMARTBULB_HOST=\n")  # Notepad leaves a mark
    monkeypatch.setenv("CHSMARTBULB_TOKEN", "exported")
    monkeypatch.setenv("CHSMARTBULB_MONITOR", "")
    assert config.load() == {"CHSMARTBULB_ADDRESS": "AA:AA", "CHSMARTBULB_TOKEN": "exported"}


def test_a_file_that_cannot_be_read_is_a_clear_error():
    config.default_path().mkdir(parents=True)  # a directory where the file should be
    with pytest.raises(SmartBulbError, match="cannot read"):
        config.load()


def test_cli_takes_its_defaults_from_the_settings_and_flags_override_them():
    settings = {
        "CHSMARTBULB_ADDRESS": "AA:AA",
        "CHSMARTBULB_TOKEN": "filed",
        "CHSMARTBULB_HOST": "laptop.local",
        "CHSMARTBULB_TRANSPORT": "ble",
        "CHSMARTBULB_AUDIO_DEVICE": "Speakers",
        "CHSMARTBULB_MONITOR": "2",
    }
    args = cli.build_parser(settings).parse_args(["status"])
    assert (args.address, args.token, args.host) == ("AA:AA", "filed", "laptop.local")
    assert (args.transport, args.audio_device, args.monitor) == ("ble", "Speakers", 2)

    args = cli.build_parser(settings).parse_args(["-a", "BB:BB", "-t", "rfcomm", "--monitor", "0", "status"])
    assert (args.address, args.transport, args.monitor, args.token) == ("BB:BB", "rfcomm", 0, "filed")

    args = cli.build_parser({}).parse_args(["status"])
    assert (args.address, args.token, args.host, args.transport) == (None, None, None, "rfcomm")


@pytest.mark.parametrize(("name", "value"), [("CHSMARTBULB_TRANSPORT", "wifi"), ("CHSMARTBULB_MONITOR", "left")])
def test_cli_refuses_a_setting_it_cannot_use(name, value, capsys):
    with pytest.raises(SystemExit):
        cli.build_parser({name: value}).parse_args(["status"])
    assert name in capsys.readouterr().err


def test_cli_reads_the_file_and_says_where_it_is_when_the_address_is_missing(monkeypatch, capsys, tmp_path):
    for name in config.NAMES:
        monkeypatch.delenv(name, raising=False)
    assert main(["--socket", str(tmp_path / "none.sock"), "on"]) == 1
    assert str(config.default_path()) in capsys.readouterr().err

    write("CHSMARTBULB_HOST=127.0.0.1:1\n")
    assert main(["status"]) == 1
    assert "no service on 127.0.0.1:1" in capsys.readouterr().err

    write("CHSMARTBULB_ADRESS=AA:AA\n")
    assert main(["status"]) == 1
    assert "config:1" in capsys.readouterr().err
