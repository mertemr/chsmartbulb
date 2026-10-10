//! Sound capture on the desktop, for the effects that follow the music.
//!
//! Linux takes the monitor of the default output (or the default input, or a named
//! source) through the `parec` tool; PipeWire and PulseAudio
//! both offer it. It can also take one program alone, whichever of its streams is playing,
//! found with `pactl`. Windows takes a WASAPI loopback of the default output (or the default input).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chsmartbulb_core::audio::{Analyzer, AudioSource};
use chsmartbulb_core::service::AudioCapture;
use chsmartbulb_core::Result;

pub struct DesktopAudio {
    microphone: bool,
    device: Option<String>,
    /// The one program to hear instead of the whole output (Linux).
    app: Mutex<Option<String>>,
    running: Mutex<Option<Running>>,
}

impl DesktopAudio {
    /// Listens to what the computer plays, or to its microphone.
    pub fn new(microphone: bool) -> Self {
        Self { microphone, device: None, app: Mutex::new(None), running: Mutex::new(None) }
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

        let microphone = self.microphone;
        let app = self.app.lock().unwrap_or_else(|p| p.into_inner()).clone();
        let task = if let Some(app) = app {
            linux::playing().await?; // fails here, where it can be told, when the tools are missing
            tokio::spawn(linux::follow_app(app, source))
        } else {
            let default = if microphone { "@DEFAULT_SOURCE@" } else { "@DEFAULT_MONITOR@" };
            let device = self.device.as_deref().unwrap_or(default);
            let (child, mut stdout) = linux::record(&format!("--device={device}"))?;
            let analyzer = Mutex::new(Analyzer::new(linux::RATE, 2, microphone, None));
            tokio::spawn(async move {
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
            })
        };
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

    fn follows_apps(&self) -> bool {
        !self.microphone
    }

    async fn apps(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = linux::playing().await?.into_iter().map(|stream| stream.app).collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        Ok(names)
    }

    fn follow(&self, app: Option<String>) {
        *self.app.lock().unwrap_or_else(|p| p.into_inner()) = app;
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::process::Stdio;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use chsmartbulb_core::audio::{Analyzer, AudioSource};
    use chsmartbulb_core::{Error, Result};
    use serde_json::Value;
    use tokio::io::AsyncReadExt;
    use tokio::process::{Child, ChildStdout, Command};

    use super::analyse;

    pub const RATE: u32 = chsmartbulb_core::audio::RATE;
    /// How often the streams of the followed program are looked up: they come and go with each
    /// song or tab, and a capture of one that ended goes on delivering silence.
    const LOOK: Duration = Duration::from_secs(1);

    /// One stream a program plays into an output.
    #[derive(Debug, PartialEq)]
    pub struct Stream {
        pub index: u64,
        pub app: String,
        /// Paused by its program.
        pub corked: bool,
    }

    /// `parec` delivering 16-bit stereo from `from`, a `--device=` or `--monitor-stream=` argument.
    pub fn record(from: &str) -> Result<(Child, ChildStdout)> {
        let mut child = Command::new("parec")
            .args([from, "--format=s16le", &format!("--rate={RATE}")])
            .args(["--channels=2", "--raw", "--latency-msec=20"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::Invalid(format!("sound capture needs the 'parec' tool (pulseaudio-utils): {e}")))?;
        let stdout = child.stdout.take().expect("piped");
        Ok((child, stdout))
    }

    /// The name a program goes by. Its binary says more than the name its sound library gave
    /// it (Discord plays as "WEBRTC VoiceEngine"), except under Wine, where every program shares one.
    fn app_name(properties: &Value) -> Option<String> {
        let text = |key: &str| properties.get(key).and_then(Value::as_str).filter(|text| !text.is_empty());
        let binary = text("application.process.binary").filter(|binary| !binary.starts_with("wine"));
        binary.or(text("application.name")).or(text("node.name")).map(str::to_string)
    }

    /// The streams in what `pactl -f json list sink-inputs` prints.
    pub fn streams(listing: &str) -> Vec<Stream> {
        let Ok(Value::Array(inputs)) = serde_json::from_str(listing) else { return Vec::new() };
        let stream = |input: &Value| {
            Some(Stream {
                index: input.get("index")?.as_u64()?,
                app: app_name(input.get("properties")?)?,
                corked: input.get("corked").and_then(Value::as_bool).unwrap_or(false),
            })
        };
        inputs.iter().filter_map(stream).collect()
    }

    /// What plays on this computer now.
    pub async fn playing() -> Result<Vec<Stream>> {
        let needs = "following one program needs 'pactl' with JSON output (PipeWire, or PulseAudio 16)";
        let listed = Command::new("pactl")
            .args(["-f", "json", "list", "sink-inputs"])
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|e| Error::Invalid(format!("{needs}: {e}")))?;
        if !listed.status.success() {
            return Err(Error::Invalid(needs.into()));
        }
        Ok(streams(&String::from_utf8_lossy(&listed.stdout)))
    }

    /// The stream of `app` to hear: the one heard already while it plays, else one that plays,
    /// else a paused one, so that the next song is not missed.
    pub fn pick(streams: &[Stream], app: &str, heard: Option<u64>) -> Option<u64> {
        let of_app = || streams.iter().filter(|stream| stream.app.eq_ignore_ascii_case(app));
        let kept = of_app().find(|stream| Some(stream.index) == heard);
        kept.filter(|stream| !stream.corked)
            .or(of_app().find(|stream| !stream.corked))
            .or(kept)
            .or(of_app().next())
            .map(|stream| stream.index)
    }

    async fn read(heard: &mut Option<(u64, Child, ChildStdout)>, buffer: &mut [u8]) -> usize {
        match heard {
            Some((_, _, stdout)) => stdout.read(buffer).await.unwrap_or(0),
            None => std::future::pending().await,
        }
    }

    /// Publish what `app` plays, moving along its streams as they come and go; silence while it plays nothing.
    pub async fn follow_app(app: String, source: Arc<AudioSource>) {
        let analyzer = Mutex::new(Analyzer::new(RATE, 2, false, None));
        let silence = vec![0u8; analyzer.lock().unwrap_or_else(|p| p.into_inner()).block() * 4];
        let mut heard: Option<(u64, Child, ChildStdout)> = None; // dropping it ends its parec
        let mut delivered = false;
        let mut buffer = vec![0u8; 4096];
        let mut look = tokio::time::interval(LOOK);
        look.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = look.tick() => {
                    let index = heard.as_ref().map(|(index, ..)| *index);
                    let wanted = pick(&playing().await.unwrap_or_default(), &app, index);
                    if wanted != index {
                        heard = wanted.and_then(|index| {
                            let opened = record(&format!("--monitor-stream={index}"));
                            opened.map_err(|error| log::warn!("{error}")).ok().map(|(child, stdout)| (index, child, stdout))
                        });
                    }
                    if !std::mem::take(&mut delivered) {
                        analyse(&analyzer, &source, &silence); // a paused stream delivers nothing: go dark
                    }
                }
                size = read(&mut heard, &mut buffer) => match size {
                    0 => heard = None,
                    size => {
                        delivered = true;
                        analyse(&analyzer, &source, &buffer[..size]);
                    }
                },
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const LISTING: &str = r#"[
            {"index": 7, "corked": true, "properties": {"application.name": "Spotify"}},
            {"index": 9, "corked": false,
             "properties": {"application.name": "WEBRTC VoiceEngine", "application.process.binary": "Discord"}},
            {"index": 12, "corked": false, "properties": {"application.name": "Spotify"}},
            {"index": 14, "corked": false,
             "properties": {"application.name": "A Game", "application.process.binary": "wine64-preloader"}},
            {"index": 15, "corked": false, "properties": {}}
        ]"#;

        #[test]
        fn streams_are_named_after_their_program() {
            let names: Vec<(u64, String)> = streams(LISTING).into_iter().map(|s| (s.index, s.app)).collect();
            let expected = [(7, "Spotify"), (9, "Discord"), (12, "Spotify"), (14, "A Game")];
            assert_eq!(names, expected.map(|(index, app)| (index, app.to_string())));
            assert_eq!(streams("not json"), Vec::new());
        }

        #[test]
        fn the_stream_that_plays_is_heard() {
            let listed = streams(LISTING);
            assert_eq!(pick(&listed, "spotify", None), Some(12));
            assert_eq!(pick(&listed, "spotify", Some(7)), Some(12)); // the one heard was paused
            assert_eq!(pick(&listed, "spotify", Some(12)), Some(12));
            assert_eq!(pick(&listed, "Discord", Some(3)), Some(9)); // the one heard is gone
            assert_eq!(pick(&listed, "mpv", Some(9)), None);
            let paused = [Stream { index: 7, app: "Spotify".into(), corked: true }];
            assert_eq!(pick(&paused, "Spotify", None), Some(7));
        }
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
            .map_err(|e| chsmartbulb_core::Error::Invalid(e.to_string()))?;
        started.await.map_err(|_| chsmartbulb_core::Error::Invalid("sound capture did not start".into()))??;
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
