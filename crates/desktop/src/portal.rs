//! The screen of a Wayland session, the way screen sharing gets it: the desktop portal
//! asks the user which screen to share and hands over a PipeWire stream of it.
//!
//! The choice is remembered through the portal's restore token, kept in a file, so the
//! question comes once. Frames arrive whenever the picture changes; a grid of their
//! pixels becomes one colour, at most [`RATE`] times a second.

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ashpd::desktop::screencast::{CursorMode, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::{PersistMode, Session};
use ashpd::enumflags2::BitFlags;
use chsmartbulb_core::screen::{picture_color, ScreenSource, RATE, SAMPLES};
use pipewire as pw;
use pw::spa;
use pw::spa::param::video::{VideoFormat, VideoInfoRaw};

use crate::{Error, Result};

/// How long the user has to answer the question about which screen to share.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);

fn failed(error: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("cannot share the screen: {error}"))
}

/// A running screen cast; [`Cast::stop`] ends it.
pub struct Cast {
    quit: pw::channel::Sender<()>,
    session: Session<Screencast>,
    ended: Arc<AtomicBool>,
}

impl Cast {
    pub fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst)
    }

    /// Stop reading frames and tell the desktop the sharing is over.
    pub fn stop(self) {
        let _ = self.quit.send(());
        tokio::spawn(async move {
            let _ = self.session.close().await;
        });
    }
}

fn remembered(path: Option<&Path>) -> Option<String> {
    let token = std::fs::read_to_string(path?).ok()?;
    Some(token.trim().to_string()).filter(|token| !token.is_empty())
}

fn remember(path: Option<&Path>, token: Option<&str>) {
    let (Some(path), Some(token)) = (path, token) else { return };
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(path, token));
    if let Err(error) = written {
        log::warn!("cannot remember the shared screen in {}: {error}", path.display());
    }
}

/// Ask the desktop for a screen (silently, when one was chosen before) and follow its colour.
pub async fn start(source: Arc<ScreenSource>, token_path: Option<PathBuf>) -> Result<Cast> {
    let portal = Screencast::new().await.map_err(failed)?;
    let session = portal.create_session(Default::default()).await.map_err(failed)?;
    let token = remembered(token_path.as_deref());
    let options = SelectSourcesOptions::default()
        .set_cursor_mode(CursorMode::Hidden)
        .set_sources(BitFlags::from(SourceType::Monitor))
        .set_multiple(false)
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(token.as_deref());
    let opened = async {
        portal.select_sources(&session, options).await.map_err(failed)?;
        let answer = tokio::time::timeout(ANSWER_TIMEOUT, async {
            portal.start(&session, None, Default::default()).await?.response()
        });
        let streams = match answer.await {
            Ok(streams) => streams.map_err(|error| failed(format!("no screen was chosen ({error})")))?,
            Err(_) => return Err(failed("nobody chose a screen")),
        };
        let node =
            streams.streams().first().ok_or_else(|| failed("the desktop offered no stream"))?.pipe_wire_node_id();
        remember(token_path.as_deref(), streams.restore_token());
        let remote = portal.open_pipe_wire_remote(&session, Default::default()).await.map_err(failed)?;
        Ok((node, remote))
    };
    let (node, remote) = match opened.await {
        Ok(opened) => opened,
        Err(error) => {
            let _ = session.close().await;
            return Err(error);
        }
    };

    let (quit, stopped) = pw::channel::channel::<()>();
    let (ready, started) = tokio::sync::oneshot::channel::<Result<()>>();
    let ended = Arc::new(AtomicBool::new(false));
    let finished = ended.clone();
    let spawned = std::thread::Builder::new().name("chsmartbulb-screen".into()).spawn(move || {
        if let Err(error) = follow(node, remote, &source, stopped, ready) {
            log::warn!("screen capture stopped: {error}");
        }
        source.clear();
        finished.store(true, Ordering::SeqCst);
    });
    let begun = match spawned {
        Ok(_) => started.await.unwrap_or_else(|_| Err(failed("the stream did not start"))),
        Err(error) => Err(failed(error)),
    };
    if let Err(error) = begun {
        let _ = session.close().await;
        return Err(error);
    }
    Ok(Cast { quit, session, ended })
}

/// What the stream's callbacks share.
struct Following {
    format: VideoInfoRaw,
    source: Arc<ScreenSource>,
    last: Option<Instant>,
}

/// The formats asked for: four bytes a pixel, in whichever order the desktop prefers.
fn wanted() -> std::result::Result<Vec<u8>, String> {
    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Video),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle { width: 1920, height: 1080 },
            spa::utils::Rectangle { width: 1, height: 1 },
            spa::utils::Rectangle { width: 16384, height: 16384 }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: RATE as u32, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction { num: 1000, denom: 1 }
        ),
    );
    let pod = spa::pod::Value::Object(object);
    let written = spa::pod::serialize::PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &pod);
    Ok(written.map_err(|error| error.to_string())?.0.into_inner())
}

/// The colour of one frame, from a grid of its pixels; `None` for a frame that does not fit its format.
fn frame_color(bytes: &[u8], stride: usize, format: VideoInfoRaw) -> Option<chsmartbulb_core::Color> {
    let (width, height) = (format.size().width as usize, format.size().height as usize);
    let blue_first = match format.format() {
        VideoFormat::BGRx | VideoFormat::BGRA => true,
        VideoFormat::RGBx | VideoFormat::RGBA => false,
        _ => return None,
    };
    if width == 0 || height == 0 || stride < width * 4 || bytes.len() < stride * (height - 1) + width * 4 {
        return None;
    }
    let step = (width.min(height) / SAMPLES as usize).max(1);
    let rows = (0..height).step_by(step);
    let pixels = rows.flat_map(|row| (0..width).step_by(step).map(move |column| row * stride + column * 4));
    Some(picture_color(pixels.map(|at| {
        let pixel = &bytes[at..at + 3];
        if blue_first {
            [pixel[2], pixel[1], pixel[0]]
        } else {
            [pixel[0], pixel[1], pixel[2]]
        }
    })))
}

/// Read the stream on this thread until told to stop or the desktop ends it.
fn follow(
    node: u32,
    remote: OwnedFd,
    source: &Arc<ScreenSource>,
    stopped: pw::channel::Receiver<()>,
    ready: tokio::sync::oneshot::Sender<Result<()>>,
) -> Result<()> {
    pw::init();
    let connected = (|| {
        let mainloop = pw::main_loop::MainLoopRc::new(None)?;
        let context = pw::context::ContextRc::new(&mainloop, None)?;
        let core = context.connect_fd_rc(remote, None)?;
        Ok::<_, pw::Error>((mainloop, core))
    })();
    let (mainloop, core) = match connected {
        Ok(connected) => connected,
        Err(error) => {
            let _ = ready.send(Err(failed("PipeWire is not reachable")));
            return Err(failed(error));
        }
    };
    let properties = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let stream = match pw::stream::StreamBox::new(&core, "chsmartbulb-screen", properties) {
        Ok(stream) => stream,
        Err(error) => {
            let _ = ready.send(Err(failed(&error)));
            return Err(failed(error));
        }
    };

    let gone = mainloop.clone();
    let following = Following { format: Default::default(), source: source.clone(), last: None };
    let listener = stream
        .add_local_listener_with_user_data(following)
        .state_changed(move |_, _, _, state| match state {
            pw::stream::StreamState::Error(error) => {
                log::warn!("the screen stream failed: {error}");
                gone.quit();
            }
            pw::stream::StreamState::Unconnected => gone.quit(), // the sharing was ended from the desktop
            _ => {}
        })
        .param_changed(|_, following, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if following.format.parse(param).is_ok() {
                let size = following.format.size();
                log::info!("watching the shared screen ({}x{})", size.width, size.height);
            }
        })
        .process(|stream, following| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let now = Instant::now();
            if following.last.is_some_and(|last| now.duration_since(last).as_secs_f64() < 1.0 / RATE) {
                return; // the desktop sends every frame it draws; the light needs far fewer
            }
            let Some(data) = buffer.datas_mut().first_mut() else { return };
            let (offset, stride) = (data.chunk().offset() as usize, data.chunk().stride().max(0) as usize);
            let Some(bytes) = data.data().and_then(|bytes| bytes.get(offset..)) else { return };
            if let Some(color) = frame_color(bytes, stride, following.format) {
                following.last = Some(now);
                following.source.push(color);
            }
        })
        .register();
    let wanted = wanted().and_then(|wanted| {
        let pod = spa::pod::Pod::from_bytes(&wanted).ok_or("unusable format description")?;
        let flags = pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
        stream.connect(spa::utils::Direction::Input, Some(node), flags, &mut [pod]).map_err(|error| error.to_string())
    });
    let _listener = match (listener, wanted) {
        (Ok(listener), Ok(())) => listener,
        (Err(error), _) => {
            let _ = ready.send(Err(failed(&error)));
            return Err(failed(error));
        }
        (_, Err(error)) => {
            let _ = ready.send(Err(failed(&error)));
            return Err(failed(error));
        }
    };
    let quit = mainloop.clone();
    let _stopped = stopped.attach(mainloop.loop_(), move |()| quit.quit());
    let _ = ready.send(Ok(()));
    mainloop.run();
    Ok(())
}

#[cfg(test)]
mod tests {
    use chsmartbulb_core::Color;

    use super::*;

    fn format(format: VideoFormat, width: u32, height: u32) -> VideoInfoRaw {
        let mut info = VideoInfoRaw::new();
        info.set_format(format);
        info.set_size(spa::utils::Rectangle { width, height });
        info
    }

    #[test]
    fn a_frame_becomes_its_colour_in_either_byte_order_and_with_padded_rows() {
        // two rows of two pixels, each row padded to twelve bytes
        let bgrx = [10, 20, 30, 0, 10, 20, 30, 0, 9, 9, 9, 9, 10, 20, 30, 0, 10, 20, 30, 0];
        assert_eq!(frame_color(&bgrx, 12, format(VideoFormat::BGRx, 2, 2)), Some(Color::rgb(30, 20, 10)));
        assert_eq!(frame_color(&bgrx, 12, format(VideoFormat::RGBA, 2, 2)), Some(Color::rgb(10, 20, 30)));
        // a buffer shorter than its format says, or a format nobody asked for, is skipped
        assert_eq!(frame_color(&bgrx[..12], 12, format(VideoFormat::BGRx, 2, 2)), None);
        assert_eq!(frame_color(&bgrx, 12, format(VideoFormat::I420, 2, 2)), None);
        assert_eq!(frame_color(&bgrx, 12, format(VideoFormat::BGRx, 0, 0)), None);
    }
}
