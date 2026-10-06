# chsmartbulb-native

The Rust core of [chsmartbulb](../../README.md) for Python: the frame codec, the sound analysis
and the effect catalog, from [`crates/core`](../core).

The `chsmartbulb` package uses it when it is installed and falls back to its own Python code
otherwise. With it, the sound-reactive effects and the audio agent need no numpy, and the
analysis runs in Rust.

```bash
pip install "chsmartbulb-native @ git+https://github.com/mertemr/chsmartbulb#subdirectory=crates/python"
```

Building it needs a Rust toolchain (`rustup`). In a checkout, `uv pip install ./crates/python`
or `maturin develop -m crates/python/Cargo.toml` puts it into the project's environment.
`CHSMARTBULB_NATIVE=0` makes the package ignore it.
