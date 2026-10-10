# chsmartbulb-native

The Rust core of [chsmartbulb](../../README.md) for Python: the effect catalog, the sound
analysis and the frame codec, from [`crates/core`](../core).

The effects exist in Rust only. With this module installed the `chsmartbulb` package plays them
itself (`chsmartbulb effect ...` without a service, `chsmartbulb.effects.create`), and its audio
agent analyses sound in Rust instead of with numpy. Without it the package still drives the light
and the service, and the agents still run.

```bash
pip install "chsmartbulb-native @ git+https://github.com/mertemr/chsmartbulb#subdirectory=crates/python"
```

Building it needs a Rust toolchain (`rustup`). In a checkout, `uv pip install ./crates/python`
or `maturin develop -m crates/python/Cargo.toml` puts it into the project's environment.
`CHSMARTBULB_NATIVE=0` makes the package ignore it.
