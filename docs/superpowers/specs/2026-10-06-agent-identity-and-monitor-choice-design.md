# Agent identity and monitor choice

## Intent

The interface cannot say which machine feeds the sound or the screen, and a screen effect cannot be
pointed at a second monitor. Both come from one fact: an agent's connection is one-way (it streams,
the service never answers). Success: the interface names the machine that feeds the light, and the
user picks the monitor the screen effect follows, from the interface, whether the screen is read by
an agent or by the service's own computer.

Assumptions (from the conversation, not yet confirmed one by one): the choice is per agent;
it is remembered only while the service runs; sound-source choice, merging monitors of several
agents and saving to disk are out of scope.

## Protocol additions (docs/usage.md, socket protocol)

1. **`hello`**, agent → service, sent once after auth and before the stream:
   `{"cmd":"hello","kind":"screen"|"audio","name":"MERT-PC","monitors":[{"index":1,"width":1920,"height":1080}],"monitor":1}`.
   `monitors` and `monitor` only for `screen`. Unanswered, like the stream. An agent that never says
   hello keeps working and shows as an unnamed computer without a monitor choice.
2. **`monitor`**, any client → service: `{"cmd":"monitor","agent":"<id>","index":N}`; `agent`
   `"local"` addresses the service's own capture. `index` 0 is every monitor together (mss's numbering).
   Reply `{"ok":true}`, or `{"ok":false,"error":...}` for an unknown agent or an index not in its list.
3. **`monitor` event**, service → agent, on the agent's own connection: `{"event":"monitor","monitor":N}`.
   Sent when someone chooses, and right after the hello of a returning agent that chose before.
4. **State** gains `agentInfo`: `{"screen":[{"id","name","monitors","monitor"}],"audio":[{"id","name"}]}`
   and `localMonitors` / `localMonitor` for the service's own capture (absent when it cannot capture).
   `agents` (counts), `audio` and `screen` stay as they are.

An agent's `id` is assigned by the service per connection. The choice is remembered by `(kind, name)`.

## Service (Python `service.py`, Rust `crates/core/src/service.rs`)

- A registry of connected agents (id, kind, name, monitors, monitor, a way to send an event to it).
  An agent joins the registry at its hello, or at its first block when it never said hello.
- `monitor` updates the entry, remembers `(kind, name) -> index`, pushes the event, notifies watchers.
- The Python service's own capture: `monitor` with agent `local` restarts the local screen source
  with that index (`screen_factory(monitor=N)`; a factory that takes no argument keeps working).
  The Rust service has no capture of its own (the platform layer has it), so it lists none.
- Both services behave the same; the Rust tests mirror the Python ones.

## Agent (`client.py`, `cli.py`)

`_stream` reads the connection line by line instead of waiting for it to end. It sends the hello
(`socket.gethostname()`, the monitors mss lists) and, on a `monitor` event, restarts the capture on
that monitor. `--monitor` stays as the first choice. `screen.list_monitors()` reads the list.

## Interface (`web/`)

- Screen panel: the source's name, and a choice "1 · 1920×1080 … / All" for the agent that feeds it
  (or the service's own computer), sent as `monitor`.
- Main screen: a standing badge while a sound or screen effect runs: "Screen: MERT-PC (agent) · 2",
  "Sound: this device". It reads `agentInfo`; without a name it says "another computer".
- Connections list names the machine too.

## Testing

Python: hello and registry, monitor routing and error cases, remembered choice after a reconnect,
the agent restarting on an event (fake source), local restart, an old agent without hello.
Rust: the same against `Session`. Web: `pnpm`/`npm run check` and build.
