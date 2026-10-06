//! Python bindings to the Rust core: what the `chsmartbulb` package can hand over to Rust.

use chsmartbulb_core::audio;
use chsmartbulb_core::protocol::{self, Frame};
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
    chsmartbulb_core::catalog::describe().to_string()
}

#[pymodule]
fn chsmartbulb_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("__version__", env!("CARGO_PKG_VERSION"))?;
    module.add_class::<Analyzer>()?;
    module.add_class::<FrameReader>()?;
    module.add_function(wrap_pyfunction!(encode_frame, module)?)?;
    module.add_function(wrap_pyfunction!(beat_ratio, module)?)?;
    module.add_function(wrap_pyfunction!(describe, module)?)?;
    Ok(())
}
