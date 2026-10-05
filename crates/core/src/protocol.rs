//! Wire format of the CHSmartBulb / BL08A control protocol (no I/O in this module).
//!
//! Every message is one frame:
//!
//! ```text
//! 01 fe 00 00 | type | command | length (u16 LE, whole frame) | body
//! ```
//!
//! The same frames travel over SPP/RFCOMM and over the BLE characteristics.
//! See `docs/protocol.md` for how each field was verified.

use crate::color::Color;
use crate::error::{invalid, Error, Result};

pub const MAGIC: [u8; 4] = [0x01, 0xfe, 0x00, 0x00];
pub const HEADER_LEN: usize = 8;

pub const RFCOMM_CHANNEL: u8 = 2;
/// The Serial Port service the bulb offers on Bluetooth Classic.
pub const SPP_UUID: &str = "00001101-0000-1000-8000-00805f9b34fb";
pub const BLE_WRITE_UUID: &str = "00008877-0000-1000-8000-00805f9b34fb";
pub const BLE_NOTIFY_UUID: &str = "00008888-0000-1000-8000-00805f9b34fb";
/// Company id in the bulb's LE manufacturer data (CUBE Technologies).
pub const BLE_COMPANY_ID: u16 = 0x03ee;
/// Names the bulb advertises; older units use the second.
pub const ADVERTISED_NAMES: [&str; 2] = ["SmartBulb Bluetooth", "Chsmartbulb"];

/// Sent by the vendor app right after connecting. The bulb works without it.
pub const HELLO: &[u8] = b"01234567";

/// Reply body prefix to [`Command::Identify`] on this protocol dialect.
pub const MODEL_ID: u16 = 0x0c47;

const ARGS_QUERY: [u8; 8] = [0, 0, 0, 0x80, 0, 0, 0, 0x80];
const ARGS_ZERO: [u8; 8] = [0; 8];
const ARGS_ALL: [u8; 8] = [0xff, 0xff, 0xff, 0xff, 0, 0, 0, 0x80];

pub mod frame_type {
    /// 'A', device -> host
    pub const ANSWER: u8 = 0x41;
    /// 'Q', host -> device, answered with the same command byte
    pub const QUERY: u8 = 0x51;
    /// 'S', host -> device, not acknowledged
    pub const SET: u8 = 0x53;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Command {
    /// As a query; as a set the same byte sets the clock.
    Status = 0x00,
    Identify = 0x02,
    Timers = 0x30,
    Ringtones = 0x31,
    Alarms = 0x32,
    Name = 0x80,
    Hardware = 0x81,
    LightState = 0x82,
    Light = 0x83,
}

impl Command {
    fn query_args(self) -> Option<&'static [u8; 8]> {
        match self {
            Command::Status | Command::Identify | Command::Timers | Command::Ringtones => Some(&ARGS_QUERY),
            Command::Alarms => Some(&ARGS_ALL),
            Command::Name | Command::Hardware | Command::LightState => Some(&ARGS_ZERO),
            Command::Light => None,
        }
    }
}

/// Effect byte of the light command. Unknown values make the bulb ignore the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NativeEffect {
    Fixed = 0x50,
    /// Sound reactive; dark while silent, ignores the colour.
    Music = 0x51,
    Breathing = 0x52,
    Rainbow = 0x53,
    Flash = 0x54,
    Heartbeat = 0x56,
    Automatic = 0x58,
    Candle = 0x5a,
    Ocean = 0x5c,
    Natural = 0x5d,
    Sunset = 0x5e,
    Passion = 0x5f,
    RgbCut = 0x61,
}

impl NativeEffect {
    pub const ALL: [NativeEffect; 13] = [
        NativeEffect::Fixed,
        NativeEffect::Music,
        NativeEffect::Breathing,
        NativeEffect::Rainbow,
        NativeEffect::Flash,
        NativeEffect::Heartbeat,
        NativeEffect::Automatic,
        NativeEffect::Candle,
        NativeEffect::Ocean,
        NativeEffect::Natural,
        NativeEffect::Sunset,
        NativeEffect::Passion,
        NativeEffect::RgbCut,
    ];

    /// The lower case name the service and its clients use.
    pub fn name(self) -> &'static str {
        match self {
            NativeEffect::Fixed => "fixed",
            NativeEffect::Music => "music",
            NativeEffect::Breathing => "breathing",
            NativeEffect::Rainbow => "rainbow",
            NativeEffect::Flash => "flash",
            NativeEffect::Heartbeat => "heartbeat",
            NativeEffect::Automatic => "automatic",
            NativeEffect::Candle => "candle",
            NativeEffect::Ocean => "ocean",
            NativeEffect::Natural => "natural",
            NativeEffect::Sunset => "sunset",
            NativeEffect::Passion => "passion",
            NativeEffect::RgbCut => "rgb_cut",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let name = name.to_lowercase();
        Self::ALL.into_iter().find(|effect| effect.name() == name)
    }

    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|effect| *effect as u8 == byte)
    }

    pub fn uses_color(self) -> bool {
        matches!(
            self,
            NativeEffect::Fixed
                | NativeEffect::Breathing
                | NativeEffect::Flash
                | NativeEffect::Heartbeat
                | NativeEffect::Candle
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub command: u8,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(kind: u8, command: u8, body: impl Into<Vec<u8>>) -> Self {
        Self { kind, command, body: body.into() }
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let length = HEADER_LEN + self.body.len();
        if length > 0xffff {
            return Err(Error::Protocol("frame body too long".into()));
        }
        let mut data = Vec::with_capacity(length);
        data.extend_from_slice(&MAGIC);
        data.push(self.kind);
        data.push(self.command);
        data.extend_from_slice(&(length as u16).to_le_bytes());
        data.extend_from_slice(&self.body);
        Ok(data)
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() < HEADER_LEN || data[..4] != MAGIC {
            return Err(Error::Protocol(format!("not a frame: {}", hex(data))));
        }
        let length = u16::from_le_bytes([data[6], data[7]]) as usize;
        if length != data.len() {
            return Err(Error::Protocol(format!("frame length field says {length}, got {} bytes", data.len())));
        }
        Ok(Self::new(data[4], data[5], &data[HEADER_LEN..]))
    }
}

/// Reassembles frames from arbitrary chunks (RFCOMM reads or BLE notifications).
#[derive(Debug, Default)]
pub struct FrameReader {
    buffer: Vec<u8>,
}

impl FrameReader {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Frame> {
        self.buffer.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            let Some(start) = self.buffer.windows(MAGIC.len()).position(|window| window == MAGIC) else {
                // keep a possible partial magic at the tail, drop the rest
                let keep = MAGIC.len() - 1;
                let drop = self.buffer.len().saturating_sub(keep);
                self.buffer.drain(..drop);
                return frames;
            };
            self.buffer.drain(..start);
            if self.buffer.len() < HEADER_LEN {
                return frames;
            }
            let length = u16::from_le_bytes([self.buffer[6], self.buffer[7]]) as usize;
            if length < HEADER_LEN {
                self.buffer.drain(..MAGIC.len()); // corrupt header, resync on the next magic
                continue;
            }
            if self.buffer.len() < length {
                return frames;
            }
            frames.push(Frame::new(self.buffer[4], self.buffer[5], &self.buffer[HEADER_LEN..length]));
            self.buffer.drain(..length);
        }
    }
}

// --- builders ---------------------------------------------------------------------------

/// A query frame with the argument bytes the vendor app uses for that command.
pub fn query(command: Command) -> Frame {
    let args = command.query_args().copied().unwrap_or(ARGS_ZERO);
    Frame::new(frame_type::QUERY, command as u8, args)
}

/// The light command. Channel values are the brightness; there is no separate dimmer.
pub fn light(color: Color, effect: NativeEffect, speed: u8, fade: bool) -> Frame {
    // body: GG BB RR speed effect WW YY fade  (YY drives no LED on this model)
    let body = [color.g, color.b, color.r, speed, effect as u8, color.w, 0, u8::from(fade)];
    Frame::new(frame_type::SET, Command::Light as u8, body)
}

/// Shorthand for a fixed colour.
pub fn fixed(color: Color, fade: bool) -> Frame {
    light(color, NativeEffect::Fixed, 0, fade)
}

/// Speed byte for `level` 0 (fastest) .. 15 (slowest), packed the way the vendor app does.
pub fn speed_byte(effect: NativeEffect, level: u8) -> Result<u8> {
    if level > 15 {
        return Err(invalid("speed level must be 0..15"));
    }
    Ok(match effect {
        NativeEffect::Fixed => 0,
        NativeEffect::Music => level * 2,  // app range 0..30
        NativeEffect::Candle => level * 4, // app range 0..60
        NativeEffect::Heartbeat => level << 4,
        _ => (level << 4) | (level >> 2),
    })
}

/// Calendar time for [`set_clock`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Clock-set frame as sent by the vendor app on connect (not exercised on a device here).
pub fn set_clock(when: ClockTime) -> Frame {
    let mut body = vec![0, 0, 0, 0, 0, 0, 0, 0x80];
    body.extend_from_slice(&when.year.to_le_bytes());
    body.extend_from_slice(&[when.month, when.day, when.hour, when.minute, when.second, 0]);
    Frame::new(frame_type::SET, Command::Status as u8, body)
}

// --- parsers ----------------------------------------------------------------------------

fn expect(frame: &Frame, command: Command, min_body: usize) -> Result<()> {
    if frame.kind != frame_type::ANSWER || frame.command != command as u8 {
        return Err(Error::Protocol(format!(
            "expected answer 0x{:02x}, got {:02x} {:02x}",
            command as u8, frame.kind, frame.command
        )));
    }
    if frame.body.len() < min_body {
        return Err(Error::Protocol(format!("answer 0x{:02x} too short: {}", command as u8, hex(&frame.body))));
    }
    Ok(())
}

fn cstring(raw: &[u8]) -> String {
    let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}

/// What the bulb reports for its light.
///
/// The bulb rescales the channels so the largest is 255, so this tells the
/// colour mix and whether the light is on, but not the brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightState {
    pub color: Color,
    pub yellow: u8,
    pub flags: u8,
}

impl LightState {
    pub fn is_on(&self) -> bool {
        !self.color.is_off()
    }
}

pub fn parse_light_state(frame: &Frame) -> Result<LightState> {
    expect(frame, Command::LightState, 8)?;
    let b = &frame.body;
    Ok(LightState { color: Color::rgbw(b[2], b[0], b[1], b[5]), yellow: b[6], flags: b[7] })
}

pub fn parse_model_id(frame: &Frame) -> Result<u16> {
    expect(frame, Command::Identify, 2)?;
    Ok(u16::from_le_bytes([frame.body[0], frame.body[1]]))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceName {
    pub version: String,
    pub name: String,
}

pub fn parse_name(frame: &Frame) -> Result<DeviceName> {
    expect(frame, Command::Name, 17)?;
    Ok(DeviceName { version: cstring(&frame.body[8..15]), name: cstring(&frame.body[16..]) })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hardware {
    pub device_id: u16,
    pub model: String,
}

pub fn parse_hardware(frame: &Frame) -> Result<Hardware> {
    expect(frame, Command::Hardware, 8)?;
    let body = &frame.body;
    let model: Vec<u8> = body[4..8].iter().rev().copied().collect();
    Ok(Hardware {
        device_id: u16::from_le_bytes([body[0], body[1]]),
        model: String::from_utf8_lossy(&model).into_owned(),
    })
}

const TIMER_RECORD: usize = 44;
const TIMER_NAME: usize = 32;

/// A stored schedule entry, as read from the bulb.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timer {
    pub index: u8,
    pub name: String,
    pub enabled: bool,
    /// Bit mask, 0x7f = every day.
    pub days: u8,
    pub hour: u8,
    pub minute: u8,
    pub raw: Vec<u8>,
}

/// Rewrite a stored timer with only its enabled flag changed.
pub fn write_timer(timer: &Timer, enabled: bool) -> Frame {
    let raw_name = &timer.raw[..TIMER_NAME];
    let end = raw_name.iter().position(|&byte| byte == 0).unwrap_or(TIMER_NAME);
    let mut name = raw_name[..end].to_vec();
    name.resize(TIMER_NAME, 0);
    let flag = u8::from(enabled);
    let mut body = Vec::with_capacity(52);
    body.extend_from_slice(&u32::from(timer.index).to_le_bytes());
    body.extend_from_slice(&u32::from(flag).to_le_bytes());
    body.extend_from_slice(&name);
    body.extend_from_slice(&[timer.index, 0, flag, timer.days, timer.hour, timer.minute]);
    body.extend_from_slice(&timer.raw[TIMER_NAME + 6..]);
    Frame::new(frame_type::SET, Command::Timers as u8, body)
}

pub fn parse_timers(frame: &Frame) -> Result<Vec<Timer>> {
    expect(frame, Command::Timers, 8)?;
    let body = &frame.body;
    let count = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
    let records = &body[8..];
    if records.len() < count * TIMER_RECORD {
        return Err(Error::Protocol(format!("timer list truncated: {count} entries in {} bytes", records.len())));
    }
    Ok(records
        .chunks_exact(TIMER_RECORD)
        .take(count)
        .map(|raw| {
            let tail = &raw[TIMER_NAME..];
            Timer {
                index: tail[0],
                name: cstring(&raw[..TIMER_NAME]),
                enabled: tail[2] != 0,
                days: tail[3],
                hour: tail[4],
                minute: tail[5],
                raw: raw.to_vec(),
            }
        })
        .collect())
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn unhex(text: &str) -> Result<Vec<u8>> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if !text.len().is_multiple_of(2) {
        return Err(invalid("odd number of hex digits"));
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| invalid(format!("not hex: {text:?}"))))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::color::{BLUE, GREEN, RED, WHITE};

    pub const NAME_ANSWER: &str =
        "01fe0000418050000000000000000000312e302e00000003536d61727442756c6220426c7565746f6f7468\
        00000000000000000000000000000000000000000000000000000000000000000000000000";
    pub const HARDWARE_ANSWER: &str = "01fe0000418116007c58000034304c42010100001a00";
    pub const TIMERS_ANSWER: &str = "01fe0000413068000200000000000000\
        706f776572206f666600010000000000120000001300000014000000150000000601017f0614000301000000\
        706f776572206f6e0000010000000000120000001300000014000000150000000501007f0a23000301000000";

    fn encoded(frame: Frame) -> String {
        hex(&frame.encode().unwrap())
    }

    #[test]
    fn query_frames_match_the_vendor_app() {
        assert_eq!(encoded(query(Command::Identify)), "01fe0000510210000000008000000080");
        assert_eq!(encoded(query(Command::LightState)), "01fe0000518210000000000000000000");
        assert_eq!(encoded(query(Command::Status)), "01fe0000510010000000008000000080");
        assert_eq!(encoded(query(Command::Alarms)), "01fe000051321000ffffffff00000080");
    }

    #[test]
    fn light_frame_orders_channels_green_blue_red() {
        assert_eq!(encoded(fixed(RED, false)), "01fe0000538310000000ff0050000000");
        assert_eq!(encoded(fixed(GREEN, false)), "01fe000053831000ff00000050000000");
        assert_eq!(encoded(fixed(BLUE, false)), "01fe00005383100000ff000050000000");
        assert_eq!(encoded(fixed(WHITE, false)), "01fe0000538310000000000050ff0000");
    }

    #[test]
    fn light_frame_fade_effect_and_speed() {
        let frame = light(Color::rgb(1, 2, 3), NativeEffect::Breathing, 0x82, true);
        assert_eq!(hex(&frame.body), "0203018252000001");
    }

    #[test]
    fn speed_byte_uses_the_vendor_packing() {
        let bytes: Vec<u8> = [0, 4, 8, 15].iter().map(|&i| speed_byte(NativeEffect::Breathing, i).unwrap()).collect();
        assert_eq!(bytes, [0x00, 0x41, 0x82, 0xf3]);
        assert_eq!(speed_byte(NativeEffect::Heartbeat, 8).unwrap(), 0x80);
        assert_eq!(speed_byte(NativeEffect::Fixed, 8).unwrap(), 0);
        assert!(speed_byte(NativeEffect::Flash, 16).is_err());
    }

    #[test]
    fn set_clock_matches_capture() {
        let when = ClockTime { year: 2017, month: 11, day: 11, hour: 17, minute: 46, second: 10 };
        assert_eq!(encoded(set_clock(when)), "01fe0000530018000000000000000080e1070b0b112e0a00");
    }

    #[test]
    fn frame_round_trip_and_length_check() {
        let frame = Frame::new(frame_type::ANSWER, 0x82, (0..8).collect::<Vec<u8>>());
        let mut data = frame.encode().unwrap();
        assert_eq!(Frame::decode(&data).unwrap(), frame);
        data.push(0);
        assert!(Frame::decode(&data).is_err());
        assert!(Frame::decode(b"01234567").is_err());
    }

    #[test]
    fn reader_reassembles_split_and_concatenated_frames() {
        let mut stream = unhex(TIMERS_ANSWER).unwrap();
        stream.extend(unhex(HARDWARE_ANSWER).unwrap());
        let mut reader = FrameReader::new();
        let frames: Vec<Frame> = stream.chunks(7).flat_map(|chunk| reader.feed(chunk)).collect();
        let commands: Vec<u8> = frames.iter().map(|f| f.command).collect();
        assert_eq!(commands, [Command::Timers as u8, Command::Hardware as u8]);
        assert_eq!(hex(&frames[0].encode().unwrap()), TIMERS_ANSWER);
    }

    #[test]
    fn reader_resyncs_after_garbage() {
        let mut data = b"\x00junk\x01\xfe".to_vec();
        data.extend(unhex(HARDWARE_ANSWER).unwrap());
        let frames = FrameReader::new().feed(&data);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].command, Command::Hardware as u8);
    }

    #[test]
    fn parse_answers() {
        let state =
            parse_light_state(&Frame::decode(&unhex("01fe000041821000b66e260000ff0000").unwrap()).unwrap()).unwrap();
        assert_eq!(state.color, Color::rgbw(0x26, 0xb6, 0x6e, 0xff));
        assert!(state.is_on());
        let model = parse_model_id(&Frame::decode(&unhex("01fe000041021000470c000000000000").unwrap()).unwrap());
        assert_eq!(model.unwrap(), MODEL_ID);
        let name = parse_name(&Frame::decode(&unhex(NAME_ANSWER).unwrap()).unwrap()).unwrap();
        assert_eq!((name.name.as_str(), name.version.as_str()), ("SmartBulb Bluetooth", "1.0."));
        let hardware = parse_hardware(&Frame::decode(&unhex(HARDWARE_ANSWER).unwrap()).unwrap()).unwrap();
        assert_eq!((hardware.model.as_str(), hardware.device_id), ("BL04", 0x587c));
        let wrong = Frame::decode(&unhex(HARDWARE_ANSWER).unwrap()).unwrap();
        assert!(parse_light_state(&wrong).is_err());
    }

    #[test]
    fn timers_parse_and_rewrite() {
        let timers = parse_timers(&Frame::decode(&unhex(TIMERS_ANSWER).unwrap()).unwrap()).unwrap();
        let (off, on) = (&timers[0], &timers[1]);
        assert_eq!(
            (off.name.as_str(), off.index, off.enabled, off.days, off.hour, off.minute),
            ("power off", 6, true, 0x7f, 6, 20)
        );
        assert_eq!((on.name.as_str(), on.index, on.enabled, on.hour, on.minute), ("power on", 5, false, 10, 35));
        let frame = write_timer(off, false);
        assert_eq!(frame.body.len(), 52);
        assert_eq!(&frame.body[..8], &[6, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(&frame.body[40..46], &[6, 0, 0, 0x7f, 6, 20]);
    }

    #[test]
    fn native_names_round_trip() {
        for effect in NativeEffect::ALL {
            assert_eq!(NativeEffect::from_name(effect.name()), Some(effect));
            assert_eq!(NativeEffect::from_byte(effect as u8), Some(effect));
        }
    }
}
