//! Sound capture on the desktop, for the effects that follow the music.
//!
//! Linux takes the monitor of the default output (or the default input, or a named
//! source) through the `parec` tool, as the Python service does; PipeWire and PulseAudio
//! both offer it. Windows takes a WASAPI loopback of the default output (or the default input).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chsmartbulb_core::audio::{Analyzer, AudioSource};
use chsmartbulb_core::service::AudioCapture;
use chsmartbulb_core::{Error, Result};

pub struct DesktopAudio {
    microphone: bool,
    device: Option<String>,
    running: Mutex<Option<Running>>,
}

impl DesktopAudio {
    /// Listens to what the computer plays, or to its microphone.
    pub fn new(microphone: bool) -> Self {
        Self { microphone, device: None, running: Mutex::new(None) }
    }

    /// Capture from this source instead of the default one (a PulseAudio source name; Linux only).
    pub fn device(mut self, device: Option<String>) -> Self {
        self.device = device;
        self
    }
}

fn analyse(analyzer: &Mutex<Analyzer>, source: &AudioSource, pcm: &[u8]) {
    let blocks = analyzer.lock().unwrap_or_else(|p| p.into_inner()).feed_bytes(pcm);
    for (levels, onset) in blocks {
        source.publish(levels, onset);
    }
}

#[cfg(target_os = "linux")]
struct Running {
    task: tokio::task::JoinHandle<()>,
}

#[cfg(target_os = "linux")]
#[async_trait]
impl AudioCapture for DesktopAudio {
    async fn start(&self, source: Arc<AudioSource>) -> Result<()> {
        use tokio::io::AsyncReadExt;

        const RATE: u32 = chsmartbulb_core::audio::RATE;
        let microphone = self.microphone;
        let default = if microphone { "@DEFAULT_SOURCE@" } else { "@DEFAULT_MONITOR@" };
        let device = self.device.as_deref().unwrap_or(default);
        let mut child = tokio::process::Command::new("parec")
            .args([&format!("--device={device}"), "--format=s16le", &format!("--rate={RATE}")])
            .args(["--channels=2", "--raw", "--latency-msec=20"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Invalid(format!("sound capture needs the 'parec' tool (pulseaudio-utils): {e}")))?;
        let mut stdout = child.stdout.take().expect("piped");
        let analyzer = Mutex::new(Analyzer::new(RATE, 2, microphone, None));
        let task = tokio::spawn(async move {
            let _child = child; // killed when the task ends or is aborted
            let mut buffer = vec![0u8; 4096];
            loop {
                match stdout.read(&mut buffer).await {
                    Ok(0) | Err(_) => {
                        log::warn!("sound capture stopped");
                        return;
                    }
                    Ok(size) => analyse(&analyzer, &source, &buffer[..size]),
                }
            }
        });
        if let Some(old) = self.running.lock().unwrap_or_else(|p| p.into_inner()).replace(Running { task }) {
            old.task.abort();
        }
        Ok(())
    }

    async fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap_or_else(|p| p.into_inner()).take() {
            running.task.abort();
        }
    }

    fn alive(&self) -> bool {
        self.running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|running| !running.task.is_finished())
    }
}

#[cfg(windows)]
struct Running {
    stop: std::sync::mpsc::Sender<()>,
}

#[cfg(windows)]
#[async_trait]
impl AudioCapture for DesktopAudio {
    async fn start(&self, source: Arc<AudioSource>) -> Result<()> {
        let microphone = self.microphone;
        let (stop, stopped) = std::sync::mpsc::channel::<()>();
        let (ready, started) = tokio::sync::oneshot::channel::<Result<()>>();
        std::thread::Builder::new()
            .name("chsmartbulb-capture".into())
            .spawn(move || windows::capture(microphone, source, ready, stopped))
            .map_err(|e| Error::Invalid(e.to_string()))?;
        started.await.map_err(|_| Error::Invalid("sound capture did not start".into()))??;
        if let Some(old) = self.running.lock().unwrap_or_else(|p| p.into_inner()).replace(Running { stop }) {
            let _ = old.stop.send(());
        }
        Ok(())
    }

    async fn stop(&self) {
        if let Some(running) = self.running.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = running.stop.send(());
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chsmartbulb_core::audio::{Analyzer, AudioSource};
    use chsmartbulb_core::{Error, Result};
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::SampleFormat;

    use super::analyse;

    /// A loopback stream delivers nothing while nothing plays; feed silence then, so the light goes dark.
    const QUIET_CHECK: Duration = Duration::from_millis(500);

    fn failed(error: impl std::fmt::Display) -> Error {
        Error::Invalid(format!("sound capture failed: {error}"))
    }

    pub fn capture(
        microphone: bool,
        source: Arc<AudioSource>,
        ready: tokio::sync::oneshot::Sender<Result<()>>,
        stopped: Receiver<()>,
    ) {
        let host = cpal::default_host();
        // an output device opened for input is captured as a loopback by WASAPI
        let device = if microphone { host.default_input_device() } else { host.default_output_device() };
        let Some(device) = device else {
            let _ = ready.send(Err(Error::Invalid("no sound device found".into())));
            return;
        };
        let supported = if microphone { device.default_input_config() } else { device.default_output_config() };
        let supported = match supported {
            Ok(supported) => supported,
            Err(error) => {
                let _ = ready.send(Err(failed(error)));
                return;
            }
        };
        let config = supported.config();
        let channels = usize::from(config.channels);
        let analyzer = Arc::new(Mutex::new(Analyzer::new(config.sample_rate, channels, microphone, None)));
        let heard = Arc::new(AtomicBool::new(false));
        let on_error = |error| log::warn!("sound capture: {error}");
        let stream = match supported.sample_format() {
            SampleFormat::F32 => {
                let (analyzer, source, heard) = (analyzer.clone(), source.clone(), heard.clone());
                device.build_input_stream(
                    config,
                    move |data: &[f32], _: &_| {
                        heard.store(true, Ordering::Relaxed);
                        let pcm: Vec<u8> = data
                            .iter()
                            .flat_map(|sample| ((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())
                            .collect();
                        analyse(&analyzer, &source, &pcm);
                    },
                    on_error,
                    None,
                )
            }
            SampleFormat::I16 => {
                let (analyzer, source, heard) = (analyzer.clone(), source.clone(), heard.clone());
                device.build_input_stream(
                    config,
                    move |data: &[i16], _: &_| {
                        heard.store(true, Ordering::Relaxed);
                        let pcm: Vec<u8> = data.iter().flat_map(|sample| sample.to_le_bytes()).collect();
                        analyse(&analyzer, &source, &pcm);
                    },
                    on_error,
                    None,
                )
            }
            other => {
                let _ = ready.send(Err(Error::Invalid(format!("sound capture: unsupported sample format {other}"))));
                return;
            }
        };
        let stream = match stream.map_err(failed).and_then(|stream| stream.play().map_err(failed).map(|()| stream)) {
            Ok(stream) => stream,
            Err(error) => {
                let _ = ready.send(Err(error));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        let block = analyzer.lock().unwrap_or_else(|p| p.into_inner()).block();
        let silence = vec![0u8; block * channels * 2];
        while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(QUIET_CHECK) {
            if !heard.swap(false, Ordering::Relaxed) {
                analyse(&analyzer, &source, &silence);
            }
        }
        drop(stream);
    }
}
