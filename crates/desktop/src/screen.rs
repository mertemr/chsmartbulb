//! The colour of what a screen shows, for the `screen` effect, as `chsmartbulb.screen` captures it.
//!
//! Linux reads the X11 root window. A Wayland session shows X11 nothing of its picture:
//! there the desktop portal shares a screen the user chooses (the `portal` feature, see
//! [`crate::portal`]). Windows reads the desktop through GDI. Only a grid of pixels is
//! looked at: the bulb is one light, so the picture becomes one colour.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chsmartbulb_core::screen::{pace, similar, ScreenSource, RATE, SAMPLES};
use chsmartbulb_core::service::ScreenCapture;
use chsmartbulb_core::Color;
use serde_json::{json, Value};

use crate::{Error, Result};

#[cfg(target_os = "linux")]
use x11::Grabber;

#[cfg(windows)]
use gdi::Grabber;

/// Where a monitor lies on the desktop.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Area {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl Area {
    /// Pixels between two samples, so the shorter side keeps [`SAMPLES`] of them.
    fn step(&self) -> u32 {
        (self.width.min(self.height) / SAMPLES).max(1)
    }
}

/// The rectangle that holds all of `areas`: monitor 0.
fn together(areas: &[Area]) -> Option<Area> {
    let left = areas.iter().map(|area| area.x).min()?;
    let top = areas.iter().map(|area| area.y).min()?;
    let right = areas.iter().map(|area| area.x + area.width as i32).max()?;
    let bottom = areas.iter().map(|area| area.y + area.height as i32).max()?;
    Some(Area { x: left, y: top, width: (right - left) as u32, height: (bottom - top) as u32 })
}

fn chosen(areas: &[Area], monitor: i64) -> Result<Area> {
    let found = match monitor {
        0 => together(areas),
        index => usize::try_from(index - 1).ok().and_then(|index| areas.get(index).copied()),
    };
    found.ok_or_else(|| Error::Unsupported(format!("no monitor {monitor}; this machine has 1 to {}", areas.len())))
}

enum Running {
    /// A thread that grabs the picture again and again.
    Grabbing { stop: mpsc::Sender<()>, ended: Arc<AtomicBool> },
    /// A stream the desktop shares.
    #[cfg(all(target_os = "linux", feature = "portal"))]
    Shared(crate::portal::Cast),
}

impl Running {
    fn ended(&self) -> bool {
        match self {
            Running::Grabbing { ended, .. } => ended.load(Ordering::SeqCst),
            #[cfg(all(target_os = "linux", feature = "portal"))]
            Running::Shared(cast) => cast.ended(),
        }
    }

    fn stop(self) {
        match self {
            Running::Grabbing { stop, .. } => drop(stop.send(())),
            #[cfg(all(target_os = "linux", feature = "portal"))]
            Running::Shared(cast) => cast.stop(),
        }
    }
}

/// Whether this is a Wayland session, whose picture X11 cannot read.
#[cfg(target_os = "linux")]
fn wayland_session() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|name| !name.is_empty())
        || std::env::var_os("XDG_SESSION_TYPE").is_some_and(|kind| kind == "wayland")
}

/// Watches one monitor of this computer on a thread of its own, since a grab blocks.
#[derive(Default)]
pub struct DesktopScreen {
    running: Mutex<Option<Running>>,
    /// Where the screen chosen in a Wayland session is remembered.
    token_path: Option<PathBuf>,
}

impl DesktopScreen {
    pub fn new() -> Self {
        Self::default()
    }

    /// Keep the choice of a shared screen in this file, so a Wayland desktop asks for it once.
    pub fn remember_in(mut self, path: Option<PathBuf>) -> Self {
        self.token_path = path;
        self
    }

    fn replace(&self, running: Option<Running>) {
        let old = std::mem::replace(&mut *self.running.lock().unwrap_or_else(|p| p.into_inner()), running);
        if let Some(old) = old {
            old.stop();
        }
    }
}

fn watch(
    monitor: i64,
    source: Arc<ScreenSource>,
    ready: tokio::sync::oneshot::Sender<Result<()>>,
    stopped: mpsc::Receiver<()>,
) {
    let opened = Grabber::open().and_then(|grabber| {
        let area = chosen(&grabber.monitors(), monitor)?;
        Ok((grabber, area))
    });
    let (mut grabber, area) = match opened {
        Ok(opened) => opened,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    log::info!("watching monitor {monitor} ({}x{})", area.width, area.height);
    let _ = ready.send(Ok(()));
    let interval = Duration::from_secs_f64(1.0 / RATE);
    let (mut still, mut last) = (0, None::<Color>);
    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(interval * pace(still)) {
        let color = match grabber.grab(&area) {
            Ok(color) => color,
            Err(error) => {
                log::warn!("screen capture failed: {error}");
                source.clear();
                return;
            }
        };
        still = if last.is_some_and(|last| similar(color, last)) { still + 1 } else { 0 };
        last = Some(color);
        source.push(color);
    }
}

#[async_trait]
impl ScreenCapture for DesktopScreen {
    fn monitors(&self) -> Vec<Value> {
        let areas = match Grabber::open() {
            Ok(grabber) => grabber.monitors(),
            Err(error) => {
                log::debug!("cannot list the monitors: {error}"); // no display, no permission: just no list
                return Vec::new();
            }
        };
        let listed = areas.iter().enumerate();
        listed.map(|(index, area)| json!({"index": index + 1, "width": area.width, "height": area.height})).collect()
    }

    async fn start(&self, source: Arc<ScreenSource>, monitor: i64) -> chsmartbulb_core::Result<()> {
        #[cfg(all(target_os = "linux", feature = "portal"))]
        if wayland_session() {
            // the desktop asks which screen; `monitor` counts X11's, which are not there
            let cast = crate::portal::start(source, self.token_path.clone()).await;
            let cast = cast.map_err(|error| chsmartbulb_core::Error::Invalid(error.to_string()))?;
            self.replace(Some(Running::Shared(cast)));
            return Ok(());
        }
        let (stop, stopped) = mpsc::channel::<()>();
        let (ready, started) = tokio::sync::oneshot::channel::<Result<()>>();
        let ended = Arc::new(AtomicBool::new(false));
        let finished = ended.clone();
        std::thread::Builder::new()
            .name("chsmartbulb-screen".into())
            .spawn(move || {
                watch(monitor, source, ready, stopped);
                finished.store(true, Ordering::SeqCst);
            })
            .map_err(|e| chsmartbulb_core::Error::Invalid(e.to_string()))?;
        let opened = started.await.unwrap_or_else(|_| Err(Error::Unsupported("screen capture did not start".into())));
        opened.map_err(|error| chsmartbulb_core::Error::Invalid(error.to_string()))?;
        self.replace(Some(Running::Grabbing { stop, ended }));
        Ok(())
    }

    async fn stop(&self) {
        self.replace(None);
    }

    fn asks(&self) -> bool {
        cfg!(all(target_os = "linux", feature = "portal")) && {
            #[cfg(target_os = "linux")]
            let wayland = wayland_session();
            #[cfg(not(target_os = "linux"))]
            let wayland = false;
            wayland
        }
    }

    async fn choose_again(&self) {
        if let Some(path) = &self.token_path {
            let _ = std::fs::remove_file(path);
        }
        self.replace(None);
    }

    fn alive(&self) -> bool {
        let running = self.running.lock().unwrap_or_else(|p| p.into_inner());
        running.as_ref().is_some_and(|running| !running.ended())
    }
}

#[cfg(target_os = "linux")]
mod x11 {
    use chsmartbulb_core::screen::picture_color;
    use chsmartbulb_core::Color;
    use x11rb::connection::Connection;
    use x11rb::protocol::randr::ConnectionExt as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat, ImageOrder, Window};
    use x11rb::rust_connection::RustConnection;

    use super::Area;
    use crate::{Error, Result};

    fn failed(error: impl std::fmt::Display) -> Error {
        Error::Unsupported(format!("cannot read the X11 screen: {error}"))
    }

    /// Where one colour channel sits in a pixel value.
    #[derive(Clone, Copy)]
    struct Channel {
        shift: u32,
        bits: u32,
    }

    impl Channel {
        fn of(mask: u32) -> Self {
            Self { shift: mask.trailing_zeros().min(31), bits: mask.count_ones() }
        }

        /// The channel of `pixel` as 0..255, whatever its depth.
        fn read(self, pixel: u32) -> u8 {
            let mask = if self.bits >= 32 { u32::MAX } else { (1u32 << self.bits) - 1 };
            let value = (pixel >> self.shift) & mask;
            match self.bits {
                0 => 0,
                bits if bits >= 8 => (value >> (bits - 8)) as u8,
                bits => (value << (8 - bits)) as u8,
            }
        }
    }

    pub struct Grabber {
        connection: RustConnection,
        root: Window,
        whole: Area,
        channels: [Channel; 3],
        /// Bytes per pixel in what the root window returns.
        size: usize,
        most_significant_first: bool,
    }

    impl Grabber {
        pub fn open() -> Result<Self> {
            if super::wayland_session() {
                let how = if cfg!(feature = "portal") {
                    "the desktop shares it when the effect starts"
                } else {
                    "this build cannot ask the desktop to share it (the portal feature)"
                };
                return Err(Error::Unsupported(format!("a Wayland session has no monitors to list: {how}")));
            }
            let (connection, number) = RustConnection::connect(None).map_err(failed)?;
            let setup = connection.setup();
            let screen = setup.roots.get(number).ok_or_else(|| failed("no such screen"))?;
            let depths = screen.allowed_depths.iter();
            let visual = depths
                .flat_map(|depth| depth.visuals.iter())
                .find(|visual| visual.visual_id == screen.root_visual)
                .ok_or_else(|| failed("the root window has no visual"))?;
            let format = setup
                .pixmap_formats
                .iter()
                .find(|format| format.depth == screen.root_depth)
                .ok_or_else(|| failed("unknown pixel format"))?;
            if !matches!(format.bits_per_pixel, 16 | 24 | 32) {
                return Err(failed(format!("{} bits per pixel are not supported", format.bits_per_pixel)));
            }
            Ok(Self {
                root: screen.root,
                whole: Area {
                    x: 0,
                    y: 0,
                    width: screen.width_in_pixels.into(),
                    height: screen.height_in_pixels.into(),
                },
                channels: [visual.red_mask, visual.green_mask, visual.blue_mask].map(Channel::of),
                size: usize::from(format.bits_per_pixel / 8),
                most_significant_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
                connection,
            })
        }

        /// The monitors as RandR knows them, the primary one first; the whole screen without RandR.
        pub fn monitors(&self) -> Vec<Area> {
            let listed =
                self.connection.randr_get_monitors(self.root, true).ok().and_then(|cookie| cookie.reply().ok());
            let mut monitors = listed.map(|reply| reply.monitors).unwrap_or_default();
            monitors.sort_by_key(|monitor| !monitor.primary); // stable: the others keep their order
            let areas: Vec<Area> = monitors
                .iter()
                .map(|monitor| Area {
                    x: monitor.x.into(),
                    y: monitor.y.into(),
                    width: monitor.width.into(),
                    height: monitor.height.into(),
                })
                .filter(|area| area.width > 0 && area.height > 0)
                .collect();
            if areas.is_empty() {
                vec![self.whole]
            } else {
                areas
            }
        }

        /// One row of pixels for every sampled line, asked for together and read in turn.
        pub fn grab(&mut self, area: &Area) -> Result<Color> {
            let step = area.step();
            let width = u16::try_from(area.width).map_err(failed)?;
            let mut rows = Vec::new();
            for line in (0..area.height).step_by(step as usize) {
                let (x, y) = (area.x as i16, (area.y + line as i32) as i16);
                let row = self.connection.get_image(ImageFormat::Z_PIXMAP, self.root, x, y, width, 1, !0);
                rows.push(row.map_err(failed)?);
            }
            let mut pixels = Vec::new();
            for row in rows {
                let data = row.reply().map_err(failed)?.data;
                pixels.extend(data.chunks_exact(self.size).step_by(step as usize).map(|bytes| self.pixel(bytes)));
            }
            Ok(picture_color(pixels))
        }

        fn pixel(&self, bytes: &[u8]) -> [u8; 3] {
            let fold = |value: u32, byte: &u8| (value << 8) | u32::from(*byte);
            let value =
                if self.most_significant_first { bytes.iter().fold(0, fold) } else { bytes.iter().rev().fold(0, fold) };
            self.channels.map(|channel| channel.read(value))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::Channel;

        #[test]
        fn channels_are_read_at_any_depth() {
            let pixel = 0x00ff_8040;
            let read = |mask| Channel::of(mask).read(pixel);
            assert_eq!([read(0xff0000), read(0x00ff00), read(0x0000ff)], [0xff, 0x80, 0x40]);
            // ten bits a channel, as deep-colour screens have: the top eight count
            assert_eq!(Channel::of(0x3ff0_0000).read(0x3ff0_0000), 0xff);
            assert_eq!(Channel::of(0x000f_fc00).read(0x0008_0000), 0x80);
            // five and six bits, as a 16-bit screen has
            assert_eq!(Channel::of(0xf800).read(0xf800), 0xf8);
            assert_eq!(Channel::of(0x07e0).read(0x0400), 0x80);
        }
    }
}

#[cfg(windows)]
mod gdi {
    use std::ptr::{null, null_mut};

    use chsmartbulb_core::screen::picture_color;
    use chsmartbulb_core::Color;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::Foundation::{LPARAM, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EnumDisplayMonitors, GetDC, GetDIBits,
        ReleaseDC, SelectObject, SetStretchBltMode, StretchBlt, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, COLORONCOLOR,
        DIB_RGB_COLORS, HDC, HMONITOR, SRCCOPY,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SetProcessDPIAware;

    use super::Area;
    use crate::{Error, Result};

    fn failed(what: &str) -> Error {
        Error::Unsupported(format!("cannot read the screen: {what} failed"))
    }

    unsafe extern "system" fn collect(_monitor: HMONITOR, _context: HDC, area: *mut RECT, list: LPARAM) -> BOOL {
        // SAFETY: `list` is the vector `monitors` passed, alive for the whole enumeration, and
        // `area` is a rectangle the system owns for the duration of this call.
        let (list, area) = unsafe { (&mut *(list as *mut Vec<RECT>), *area) };
        list.push(area);
        1
    }

    pub struct Grabber;

    impl Grabber {
        pub fn open() -> Result<Self> {
            // SAFETY: takes no arguments; it fails harmlessly where the awareness is already set.
            unsafe { SetProcessDPIAware() }; // real pixels, not ones scaled for an unaware program
            Ok(Self)
        }

        /// The monitors of the desktop, the primary one (whose corner is the origin) first.
        pub fn monitors(&self) -> Vec<Area> {
            let mut found: Vec<RECT> = Vec::new();
            // SAFETY: the callback only runs during this call and gets the vector it expects.
            unsafe { EnumDisplayMonitors(null_mut(), null(), Some(collect), &mut found as *mut Vec<RECT> as LPARAM) };
            found.sort_by_key(|area| (area.left, area.top) != (0, 0)); // stable: the others keep their order
            found
                .iter()
                .map(|area| Area {
                    x: area.left,
                    y: area.top,
                    width: (area.right - area.left).max(0) as u32,
                    height: (area.bottom - area.top).max(0) as u32,
                })
                .filter(|area| area.width > 0 && area.height > 0)
                .collect()
        }

        /// The area shrunk to its sampled pixels by the system, then read as 32-bit BGRX.
        pub fn grab(&mut self, area: &Area) -> Result<Color> {
            let step = area.step();
            let (columns, rows) = (area.width.div_ceil(step) as i32, area.height.div_ceil(step) as i32);
            let mut pixels = vec![0u8; columns as usize * rows as usize * 4];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: columns,
                    biHeight: -rows, // top row first
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB,
                    ..Default::default()
                },
                ..Default::default()
            };
            // SAFETY: every handle is checked before use and released below; the buffer holds
            // `columns * rows` 32-bit pixels, which is what the header asks GetDIBits for.
            let read = unsafe {
                let desktop = GetDC(null_mut());
                if desktop.is_null() {
                    return Err(failed("GetDC"));
                }
                let memory = CreateCompatibleDC(desktop);
                let bitmap = CreateCompatibleBitmap(desktop, columns, rows);
                let mut read = 0;
                if !memory.is_null() && !bitmap.is_null() {
                    let before = SelectObject(memory, bitmap);
                    SetStretchBltMode(memory, COLORONCOLOR); // take pixels, do not blend them
                    let (width, height) = (area.width as i32, area.height as i32);
                    let copied =
                        StretchBlt(memory, 0, 0, columns, rows, desktop, area.x, area.y, width, height, SRCCOPY);
                    SelectObject(memory, before); // GetDIBits wants the bitmap out of any context
                    if copied != 0 {
                        read = GetDIBits(
                            memory,
                            bitmap,
                            0,
                            rows as u32,
                            pixels.as_mut_ptr().cast(),
                            &mut info,
                            DIB_RGB_COLORS,
                        );
                    }
                }
                if !bitmap.is_null() {
                    DeleteObject(bitmap);
                }
                if !memory.is_null() {
                    DeleteDC(memory);
                }
                ReleaseDC(null_mut(), desktop);
                read
            };
            if read == 0 {
                return Err(failed("copying the desktop"));
            }
            Ok(picture_color(pixels.chunks_exact(4).map(|bgrx| [bgrx[2], bgrx[1], bgrx[0]])))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{chosen, together, Area};

    #[test]
    fn monitors_are_chosen_from_one_and_zero_is_all_of_them() {
        let left = Area { x: 0, y: 0, width: 1920, height: 1080 };
        let right = Area { x: 1920, y: -200, width: 1280, height: 720 };
        let areas = [left, right];
        assert_eq!(chosen(&areas, 1).unwrap(), left);
        assert_eq!(chosen(&areas, 2).unwrap(), right);
        assert_eq!(chosen(&areas, 0).unwrap(), Area { x: 0, y: -200, width: 3200, height: 1280 });
        assert_eq!(together(&[]), None);
        let missing = chosen(&areas, 3).unwrap_err().to_string();
        assert_eq!(missing, "no monitor 3; this machine has 1 to 2");
        assert!(chosen(&areas, -1).is_err());
        assert_eq!(left.step(), 16); // 1080 / 64
    }
}
