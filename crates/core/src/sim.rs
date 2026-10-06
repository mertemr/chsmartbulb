//! A simulated bulb in memory, modelled on behaviour observed on the real one.
//!
//! The tests drive the service against it, and the app offers it as a demo so the
//! interface can be tried without a bulb. It mirrors `tests/fakes.py`.

use std::sync::{Arc, Mutex};

use crate::error::{Error, Result};
use crate::protocol::{self as p, frame_type, Command, Frame, FrameReader, NativeEffect};
use crate::transport::{Bearer, ChannelLink, Connector, Link, LinkFeed, Writer};
use async_trait::async_trait;

pub const NAME_ANSWER: &str = "01fe0000418050000000000000000000312e302e00000003536d61727442756c6220426c7565746f6f7468\
    00000000000000000000000000000000000000000000000000000000000000000000000000";
pub const HARDWARE_ANSWER: &str = "01fe0000418116007c58000034304c42010100001a00";
pub const TIMERS_ANSWER: &str = "01fe0000413068000200000000000000\
    706f776572206f666600010000000000120000001300000014000000150000000601017f0614000301000000\
    706f776572206f6e0000010000000000120000001300000014000000150000000501007f0a23000301000000";

#[derive(Default)]
pub struct SimState {
    pub opened: usize,
    pub fail_open: bool,
    pub light_bodies: Vec<Vec<u8>>,
    /// g, b, r, w, y as last written
    pub channels: [u8; 5],
    pub timers_answer: Vec<u8>,
    /// Split answers into pieces of this size, like BLE notifications.
    pub chunk: Option<usize>,
    feed: Option<LinkFeed>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastLight {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub w: u8,
    pub speed: u8,
    pub effect: u8,
    pub fade: u8,
}

#[derive(Clone)]
pub struct SimulatedBulb {
    pub state: Arc<Mutex<SimState>>,
}

impl Default for SimulatedBulb {
    fn default() -> Self {
        Self::new()
    }
}

impl SimulatedBulb {
    pub fn new() -> Self {
        let state = SimState { timers_answer: p::unhex(TIMERS_ANSWER).unwrap(), ..SimState::default() };
        Self { state: Arc::new(Mutex::new(state)) }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut SimState) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    pub fn last_light(&self) -> LastLight {
        self.with(|s| {
            let b = s.light_bodies.last().expect("a light command was sent").clone();
            LastLight { g: b[0], b: b[1], r: b[2], speed: b[3], effect: b[4], w: b[5], fade: b[7] }
        })
    }

    pub fn sent(&self) -> usize {
        self.with(|s| s.light_bodies.len())
    }

    /// Simulate the bulb going out of range.
    pub fn drop_link(&self) {
        if let Some(feed) = self.with(|s| s.feed.take()) {
            feed.closed();
        }
    }

    pub fn connector(&self) -> Arc<dyn Connector> {
        Arc::new(self.clone())
    }
}

struct SimWriter {
    state: Arc<Mutex<SimState>>,
    reader: Mutex<FrameReader>,
    feed: LinkFeed,
}

fn answer(state: &SimState, feed: &LinkFeed, data: Vec<u8>) {
    let size = state.chunk.unwrap_or(data.len());
    for piece in data.chunks(size) {
        feed.push(piece.to_vec());
    }
}

fn reply(command: u8, body: Vec<u8>) -> Vec<u8> {
    Frame::new(frame_type::ANSWER, command, body).encode().unwrap()
}

#[async_trait]
impl Writer for SimWriter {
    async fn write(&self, data: &[u8]) -> Result<()> {
        let frames = self.reader.lock().unwrap().feed(data);
        let mut state = self.state.lock().unwrap();
        for frame in frames {
            if frame.kind == frame_type::SET && frame.command == Command::Light as u8 {
                let b = &frame.body;
                if NativeEffect::from_byte(b[4]).is_none() {
                    continue; // the real bulb ignores frames with an unknown effect byte
                }
                state.light_bodies.push(b.clone());
                state.channels = [b[0], b[1], b[2], b[5], b[6]];
            } else if frame.kind == frame_type::SET && frame.command == Command::Timers as u8 {
                // stored record = name[32] + tail, where the bulb reports tail[1] as 01
                let mut record = frame.body[8..].to_vec();
                record[33] = 1;
                let index = record[32];
                let offset =
                    (16..state.timers_answer.len()).step_by(44).find(|o| state.timers_answer[o + 32] == index).unwrap();
                state.timers_answer[offset + 32..offset + 44].copy_from_slice(&record[32..44]);
            } else if frame.kind == frame_type::QUERY {
                let data = match frame.command {
                    c if c == Command::Identify as u8 => reply(c, p::unhex("470c000000000000").unwrap()),
                    c if c == Command::LightState as u8 => {
                        // the real bulb rescales so that the largest channel reads 255
                        let top = *state.channels.iter().max().unwrap() as u32;
                        let s: Vec<u8> = state
                            .channels
                            .iter()
                            .map(|&c| (c as u32 * 255).checked_div(top).unwrap_or(0) as u8)
                            .collect();
                        reply(c, vec![s[0], s[1], s[2], 0, 0, s[3], s[4], 0])
                    }
                    c if c == Command::Name as u8 => p::unhex(NAME_ANSWER).unwrap(),
                    c if c == Command::Hardware as u8 => p::unhex(HARDWARE_ANSWER).unwrap(),
                    c if c == Command::Timers as u8 => state.timers_answer.clone(),
                    _ => continue,
                };
                answer(&state, &self.feed, data);
            }
        }
        Ok(())
    }

    async fn close(&self) {
        self.feed.closed();
    }
}

#[async_trait]
impl Connector for SimulatedBulb {
    async fn connect(&self) -> Result<Arc<dyn Link>> {
        let mut state = self.state.lock().unwrap();
        if state.fail_open {
            return Err(Error::ConnectionFailed("simulated: host is down".into()));
        }
        state.opened += 1;
        let (link, feed) = ChannelLink::new_with(|feed| {
            Box::new(SimWriter { state: self.state.clone(), reader: Mutex::new(FrameReader::new()), feed })
        });
        state.feed = Some(feed);
        Ok(link)
    }

    fn describe(&self) -> String {
        "the simulated bulb".into()
    }

    fn bearer(&self) -> Bearer {
        Bearer::Spp
    }
}
