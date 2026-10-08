import pytest


@pytest.fixture(autouse=True)
def _own_settings(monkeypatch, tmp_path):
    """Keep the settings file of whoever runs the suite out of it."""
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "config"))
    monkeypatch.setenv("APPDATA", str(tmp_path / "config"))
