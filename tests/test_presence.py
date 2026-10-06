"""The computer coming and going, as the service hears it."""

import pytest

from chsmartbulb import presence


@pytest.mark.parametrize(
    ("message", "wparam", "event"),
    [
        (presence.WM_WTSSESSION_CHANGE, 7, "lock"),
        (presence.WM_WTSSESSION_CHANGE, 8, "unlock"),
        (presence.WM_WTSSESSION_CHANGE, 5, None),  # a remote session connecting is not for us
        (presence.WM_POWERBROADCAST, 4, "sleep"),
        (presence.WM_POWERBROADCAST, 0x12, "resume"),  # automatic resume
        (presence.WM_POWERBROADCAST, 7, "resume"),  # resume after the user did something
        (presence.WM_POWERBROADCAST, 0x0A, None),  # a power status change
        (presence.WM_ENDSESSION, 1, "shutdown"),
        (presence.WM_ENDSESSION, 0, None),  # the shutdown was cancelled
        (0x0001, 0, None),
    ],
)
def test_windows_messages_become_presence_events(message, wparam, event):
    assert presence.event_of(message, wparam) == event


def test_events_become_away_and_back():
    assert [presence.request_of(event) for event in ("lock", "sleep", "shutdown")] == [
        {"cmd": "away", "reason": "lock"},
        {"cmd": "away", "reason": "sleep"},
        {"cmd": "away", "reason": "shutdown"},
    ]
    assert presence.request_of("unlock") == presence.request_of("resume") == {"cmd": "back"}
