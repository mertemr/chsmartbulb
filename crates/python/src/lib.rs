//! Python bindings to the Rust core: what the `chsmartbulb` package can hand over to Rust.

use std::sync::{Arc, Mutex, PoisonError};

use chsmartbulb_core::catalog::{self, Sources};
use chsmartbulb_core::color::Color;
use chsmartbulb_core::protocol::{self, Frame};
use chsmartbulb_core::{audio, effects, screen};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyBytes;

/// Analyses signed 16-bit PCM into band levels and onsets, block by block, as
/// `chsmartbulb.music.MusicSource` does with numpy.
#[pyclass(module = "chsmartbulb_native")]
struct Analyzer {
    inner: audio::Analyzer,
}

#[pymethods]
impl Analyzer {
    #[new]
    #[pyo3(signature = (rate, channels = 1, mic = false, block = None))]
    fn new(rate: u32, channels: usize, mic: bool, block: Option<usize>) -> PyResult<Self> {
        if rate == 0 || channels == 0 || block == Some(0) {
            return Err(PyValueError::new_err("rate, channels and block must be positive"));
        }
        Ok(Self { inner: audio::Analyzer::new(rate, channels, mic, block) })
    }

    /// Consume little-endian samples, interleaved when there are several channels. Returns
    /// `((bass, mid, treble, balance), onset)` for each block completed.
    fn feed(&mut self, pcm: &[u8]) -> Vec<((f64, f64, f64, f64), f64)> {
        self.inner
            .feed_bytes(pcm)
            .into_iter()
            .map(|(levels, onset)| ((levels.bass, levels.mid, levels.treble, levels.balance), onset))
            .collect()
    }

    #[getter]
    fn rate(&self) -> u32 {
        self.inner.rate()
    }

    #[getter]
    fn channels(&self) -> usize {
        self.inner.channels()
    }

    #[getter]
    fn block(&self) -> usize {
        self.inner.block()
    }
}

/// Reassembles frames from arbitrary chunks; yields `(type, command, body)`.
#[pyclass(module = "chsmartbulb_native")]
#[derive(Default)]
struct FrameReader {
    inner: protocol::FrameReader,
}

#[pymethods]
impl FrameReader {
    #[new]
    fn new() -> Self {
        Self::default()
    }

    fn feed<'py>(&mut self, py: Python<'py>, data: &[u8]) -> Vec<(u8, u8, Bound<'py, PyBytes>)> {
        self.inner
            .feed(data)
            .into_iter()
            .map(|frame| (frame.kind, frame.command, PyBytes::new(py, &frame.body)))
            .collect()
    }
}

/// One frame on the wire.
#[pyfunction]
fn encode_frame<'py>(py: Python<'py>, kind: u8, command: u8, body: &[u8]) -> PyResult<Bound<'py, PyBytes>> {
    let encoded = Frame::new(kind, command, body).encode().map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(PyBytes::new(py, &encoded))
}

/// How far the bass must rise above its recent average to count as a beat.
#[pyfunction]
fn beat_ratio(sensitivity: f64) -> f64 {
    audio::beat_ratio(sensitivity)
}

/// The effect catalog as the services describe it, as JSON text.
#[pyfunction]
fn describe() -> String {
    catalog::describe().to_string()
}

/// What the sound-reactive effects read: publish every analysed block into it.
#[pyclass(module = "chsmartbulb_native")]
struct AudioSource {
    inner: Arc<audio::AudioSource>,
}

#[pymethods]
impl AudioSource {
    #[new]
    fn new() -> Self {
        Self { inner: audio::AudioSource::new(audio::monotonic()) }
    }

    /// One block: band levels (0..1), where the sound sits (-1 left, +1 right) and its onset.
    #[pyo3(signature = (bass, mid, treble, balance = 0.0, onset = 0.0))]
    fn publish(&self, bass: f64, mid: f64, treble: f64, balance: f64, onset: f64) {
        self.inner.publish(audio::Levels { bass, mid, treble, balance }, onset);
    }

    /// Forget what was pending and go silent, for when the feed stops.
    fn clear(&self) {
        self.inner.clear();
    }
}

/// What the screen effects read: push the colour of the picture into it.
#[pyclass(module = "chsmartbulb_native")]
struct ScreenSource {
    inner: Arc<screen::ScreenSource>,
}

#[pymethods]
impl ScreenSource {
    #[new]
    fn new() -> Self {
        Self { inner: screen::ScreenSource::new() }
    }

    fn push(&self, r: u8, g: u8, b: u8) {
        self.inner.push(Color::rgb(r, g, b));
    }

    /// Go dark, for when the feed stops.
    fn clear(&self) {
        self.inner.clear();
    }
}

/// An effect of the catalog: call it with the seconds elapsed for the colour as `(r, g, b, w)`.
#[pyclass(module = "chsmartbulb_native")]
struct Effect {
    inner: Mutex<effects::Effect>,
}

#[pymethods]
impl Effect {
    fn __call__(&self, elapsed: f64) -> (u8, u8, u8, u8) {
        let mut effect = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let color = effect(elapsed);
        (color.r, color.g, color.b, color.w)
    }
}

fn parameters(params: Option<&str>) -> PyResult<Option<serde_json::Map<String, serde_json::Value>>> {
    match params.map(serde_json::from_str::<serde_json::Value>) {
        None | Some(Ok(serde_json::Value::Null)) => Ok(None),
        Some(Ok(serde_json::Value::Object(map))) => Ok(Some(map)),
        Some(_) => Err(PyValueError::new_err("params must be a JSON object")),
    }
}

/// Build the named effect from its parameters, given as JSON text. Those that follow the
/// sound or the screen need that source.
#[pyfunction]
#[pyo3(signature = (name, params = None, audio = None, screen = None))]
fn create(
    name: &str,
    params: Option<&str>,
    audio: Option<PyRef<'_, AudioSource>>,
    screen: Option<PyRef<'_, ScreenSource>>,
) -> PyResult<Effect> {
    let sources = Sources { audio: audio.map(|a| a.inner.clone()), screen: screen.map(|s| s.inner.clone()) };
    let effect = catalog::create(name, parameters(params)?.as_ref(), &sources)
        .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(Effect { inner: Mutex::new(effect) })
}

/// Raise `ValueError` unless `params` (JSON text) fit the named effect.
#[pyfunction]
#[pyo3(signature = (name, params = None))]
fn check(name: &str, params: Option<&str>) -> PyResult<()> {
    catalog::resolve(name, parameters(params)?.as_ref()).map(drop).map_err(|e| PyValueError::new_err(e.to_string()))
}

#[pymodule]
fn chsmartbulb_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_class::<Analyzer>()?;
    module.add_class::<FrameReader>()?;
    module.add_class::<AudioSource>()?;
    module.add_class::<ScreenSource>()?;
    module.add_class::<Effect>()?;
    module.add_function(wrap_pyfunction!(encode_frame, module)?)?;
    module.add_function(wrap_pyfunction!(beat_ratio, module)?)?;
    module.add_function(wrap_pyfunction!(describe, module)?)?;
    module.add_function(wrap_pyfunction!(create, module)?)?;
    module.add_function(wrap_pyfunction!(check, module)?)?;
    Ok(())
}
