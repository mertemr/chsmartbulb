//! High-level control of one bulb, as in `chsmartbulb.bulb`.
//!
//! The bulb has no dimmer and no power switch on the wire: brightness is the
//! magnitude of the channel values and "off" is all channels at zero. [`Bulb`]
//! keeps the chosen colour and brightness itself and sends the product. The bulb
//! cannot report brightness, so after connecting it is assumed to be 1.0.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use tokio::sync::{oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::color::{Color, OFF, WHITE};
use crate::error::{invalid, Error, Result};
use crate::protocol::{self as p, frame_type, Command, Frame, FrameReader, NativeEffect};
use crate::transport::{Bearer, Connector, Link};

type Waiters = Arc<StdMutex<HashMap<u8, oneshot::Sender<Result<Frame>>>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub version: String,
    pub model: String,
    pub model_id: u16,
    pub device_id: u16,
}

#[derive(Debug, Clone, Copy)]
struct Shown {
    color: Color,
    brightness: f64,
    on: bool,
}

pub struct Bulb {
    connector: Arc<dyn Connector>,
    link: StdMutex<Option<Arc<dyn Link>>>,
    reader: StdMutex<Option<JoinHandle<()>>>,
    pending: Waiters,
    write_lock: Mutex<()>,
    request_lock: Mutex<()>,
    /// The user asked for a connection and has not closed it.
    wanted: AtomicBool,
    auto_reconnect: bool,
    request_timeout: Duration,
    shown: StdMutex<Shown>,
}

fn locked<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Bulb {
    pub fn new(connector: Arc<dyn Connector>) -> Arc<Self> {
        Arc::new(Self {
            connector,
            link: StdMutex::new(None),
            reader: StdMutex::new(None),
            pending: Arc::default(),
            write_lock: Mutex::new(()),
            request_lock: Mutex::new(()),
            wanted: AtomicBool::new(false),
            auto_reconnect: true,
            request_timeout: Duration::from_secs(2),
            shown: StdMutex::new(Shown { color: WHITE, brightness: 1.0, on: false }),
        })
    }

    pub fn describe(&self) -> String {
        self.connector.describe()
    }

    pub fn bearer(&self) -> Bearer {
        self.connector.bearer()
    }

    // --- connection -------------------------------------------------------------------

    pub fn is_connected(&self) -> bool {
        locked(&self.link).as_ref().is_some_and(|link| link.is_open())
    }

    /// Open the link, check the bulb speaks this protocol, and read its light state.
    pub async fn connect(&self) -> Result<()> {
        self.wanted.store(true, Ordering::SeqCst);
        self.open().await?;
        let checked = async {
            let model_id = p::parse_model_id(&self.request(p::query(Command::Identify)).await?)?;
            if model_id != p::MODEL_ID {
                log::warn!("unexpected model id 0x{model_id:04x} (expected 0x{:04x})", p::MODEL_ID);
            }
            self.light_state().await
        };
        let state = match checked.await {
            Ok(state) => state,
            Err(error) => {
                self.disconnect().await;
                return Err(error);
            }
        };
        let mut shown = locked(&self.shown);
        shown.on = state.is_on();
        if state.is_on() {
            shown.color = state.color;
        }
        shown.brightness = 1.0;
        Ok(())
    }

    pub async fn disconnect(&self) {
        self.wanted.store(false, Ordering::SeqCst);
        self.drop_link().await;
    }

    async fn open(&self) -> Result<()> {
        let link = self.connector.connect().await?;
        *locked(&self.link) = Some(link.clone());
        let pending = self.pending.clone();
        let task = tokio::spawn(read_loop(link, pending));
        if let Some(old) = locked(&self.reader).replace(task) {
            old.abort();
        }
        Ok(())
    }

    async fn drop_link(&self) {
        if let Some(task) = locked(&self.reader).take() {
            task.abort();
        }
        let link = locked(&self.link).take();
        if let Some(link) = link {
            link.close().await;
        }
    }

    // --- raw access -------------------------------------------------------------------

    /// Send a frame that expects no answer. Reconnects once if the link has dropped.
    pub async fn send(&self, frame: &Frame) -> Result<()> {
        let data = frame.encode()?;
        let _guard = self.write_lock.lock().await;
        for attempt in 0..2 {
            if !self.is_connected() {
                if !self.wanted.load(Ordering::SeqCst) {
                    return Err(Error::NotConnected("not connected; call connect() first".into()));
                }
                if !self.auto_reconnect {
                    return Err(Error::NotConnected("connection lost and auto_reconnect is off".into()));
                }
                self.drop_link().await;
                self.open().await?;
            }
            let link = locked(&self.link).clone();
            let Some(link) = link else { continue };
            match link.write(&data).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    self.drop_link().await;
                    if attempt == 1 || !self.auto_reconnect {
                        return Err(error);
                    }
                }
            }
        }
        Err(Error::NotConnected("the link could not be brought back".into()))
    }

    /// Send a query and wait for the answer carrying the same command byte.
    pub async fn request(&self, frame: Frame) -> Result<Frame> {
        let _guard = self.request_lock.lock().await;
        let (sender, receiver) = oneshot::channel();
        locked(&self.pending).insert(frame.command, sender);
        let result = async {
            self.send(&frame).await?;
            match tokio::time::timeout(self.request_timeout, receiver).await {
                Ok(Ok(answer)) => answer,
                Ok(Err(_)) => Err(Error::Transport("the link closed before the answer".into())),
                Err(_) => Err(Error::Timeout(format!("no answer to query 0x{:02x}", frame.command))),
            }
        }
        .await;
        locked(&self.pending).remove(&frame.command);
        result
    }

    // --- light ------------------------------------------------------------------------

    /// The colour shown when on, at full scale (before brightness is applied).
    pub fn color(&self) -> Color {
        locked(&self.shown).color
    }

    pub fn brightness(&self) -> f64 {
        locked(&self.shown).brightness
    }

    pub fn is_on(&self) -> bool {
        locked(&self.shown).on
    }

    /// Show `color` scaled by the current brightness. An all-zero colour turns the light off.
    pub async fn set_color(&self, color: Color, fade: bool) -> Result<()> {
        if color.is_off() {
            return self.turn_off(fade).await;
        }
        let scaled = {
            let mut shown = locked(&self.shown);
            shown.color = color;
            shown.on = true;
            color.scaled(shown.brightness)
        };
        self.send(&p::fixed(scaled, fade)).await
    }

    pub async fn set_brightness(&self, level: f64) -> Result<()> {
        if !(0.0..=1.0).contains(&level) {
            return Err(invalid("brightness must be within 0..1"));
        }
        let shown = {
            let mut shown = locked(&self.shown);
            shown.brightness = level;
            *shown
        };
        if shown.on {
            self.send(&p::fixed(shown.color.scaled(level), false)).await?;
        }
        Ok(())
    }

    pub async fn turn_on(&self, fade: bool) -> Result<()> {
        let scaled = {
            let mut shown = locked(&self.shown);
            shown.on = true;
            shown.color.scaled(shown.brightness)
        };
        self.send(&p::fixed(scaled, fade)).await
    }

    pub async fn turn_off(&self, fade: bool) -> Result<()> {
        locked(&self.shown).on = false;
        self.send(&p::fixed(OFF, fade)).await
    }

    /// Start one of the bulb's built-in effects.
    ///
    /// `speed` is a level from 0 (fastest) to 15 (slowest). `color` is used only
    /// by effects that take one; any light command ends the effect.
    pub async fn set_native_effect(&self, effect: NativeEffect, color: Option<Color>, speed: u8) -> Result<()> {
        let speed = p::speed_byte(effect, speed)?;
        let shown = {
            let mut shown = locked(&self.shown);
            if let Some(color) = color.filter(|c| !c.is_off()) {
                shown.color = color;
            }
            shown.on = true;
            if effect.uses_color() {
                shown.color.scaled(shown.brightness)
            } else {
                OFF
            }
        };
        self.send(&p::light(shown, effect, speed, false)).await
    }

    pub async fn light_state(&self) -> Result<p::LightState> {
        p::parse_light_state(&self.request(p::query(Command::LightState)).await?)
    }

    // --- device -----------------------------------------------------------------------

    pub async fn info(&self) -> Result<DeviceInfo> {
        let name = p::parse_name(&self.request(p::query(Command::Name)).await?)?;
        let hardware = p::parse_hardware(&self.request(p::query(Command::Hardware)).await?)?;
        let model_id = p::parse_model_id(&self.request(p::query(Command::Identify)).await?)?;
        Ok(DeviceInfo {
            name: name.name,
            version: name.version,
            model: hardware.model,
            model_id,
            device_id: hardware.device_id,
        })
    }

    /// Schedule entries stored in the bulb.
    pub async fn timers(&self) -> Result<Vec<p::Timer>> {
        p::parse_timers(&self.request(p::query(Command::Timers)).await?)
    }

    /// Switch a stored timer on or off, keeping its name, time and days.
    ///
    /// The bulb does not acknowledge the write, so the timer is read back and
    /// the call fails if the change did not take.
    pub async fn set_timer_enabled(&self, index: u8, enabled: bool) -> Result<p::Timer> {
        let timer = self.timers().await?.into_iter().find(|t| t.index == index);
        let timer = timer.ok_or_else(|| invalid(format!("the bulb has no timer with index {index}")))?;
        self.send(&p::write_timer(&timer, enabled)).await?;
        let updated = self.timers().await?.into_iter().find(|t| t.index == index);
        match updated {
            Some(updated) if updated.enabled == enabled => Ok(updated),
            _ => Err(Error::Protocol(format!("the bulb did not apply the change to timer {index}"))),
        }
    }

    /// The 24 status bytes the vendor app polls every second; meaning unknown.
    pub async fn status_raw(&self) -> Result<Vec<u8>> {
        let frame = self.request(p::query(Command::Status)).await?;
        if frame.body.len() < 8 {
            return Err(Error::Protocol(format!("status answer too short: {}", p::hex(&frame.body))));
        }
        Ok(frame.body[8..].to_vec())
    }
}

async fn read_loop(link: Arc<dyn Link>, pending: Waiters) {
    let mut reader = FrameReader::new();
    loop {
        match link.read().await {
            Ok(data) => {
                for frame in reader.feed(&data) {
                    let waiter =
                        if frame.kind == frame_type::ANSWER { locked(&pending).remove(&frame.command) } else { None };
                    match waiter {
                        Some(waiter) => {
                            let _ = waiter.send(Ok(frame));
                        }
                        None => log::debug!(
                            "unsolicited frame {:02x} {:02x} {}",
                            frame.kind,
                            frame.command,
                            p::hex(&frame.body)
                        ),
                    }
                }
            }
            Err(error) => {
                log::info!("connection lost: {error}");
                link.close().await;
                for (_, waiter) in locked(&pending).drain() {
                    let _ = waiter.send(Err(error.clone()));
                }
                return;
            }
        }
    }
}
