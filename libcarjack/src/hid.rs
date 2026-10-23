//! The accessory's buttons and knobs, which it declares in `/info`'s `hidDevices` as USB HID
//! report descriptors and then sends as `hidSendReport`s. Each descriptor is read for where its
//! input fields sit in a report and what they mean, so any unit's switchgear decodes the same way;
//! what they mean is then mapped onto Android key codes (the touchscreen is handled elsewhere).

use std::collections::HashMap;

/// A key going down or up, as an Android `KeyEvent` key code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub code: u32,
    pub down: bool,
}

/// Android `KeyEvent` codes.
mod key {
    pub const HOME: u32 = 3;
    pub const BACK: u32 = 4;
    pub const ENDCALL: u32 = 6;
    pub const DPAD_UP: u32 = 19;
    pub const DPAD_DOWN: u32 = 20;
    pub const DPAD_LEFT: u32 = 21;
    pub const DPAD_RIGHT: u32 = 22;
    pub const DPAD_CENTER: u32 = 23;
    pub const HEADSETHOOK: u32 = 79;
    pub const MEDIA_PLAY_PAUSE: u32 = 85;
    pub const MEDIA_STOP: u32 = 86;
    pub const MEDIA_NEXT: u32 = 87;
    pub const MEDIA_PREVIOUS: u32 = 88;
    pub const MEDIA_PLAY: u32 = 126;
    pub const MEDIA_PAUSE: u32 = 127;
}

/// The key a (usage page, usage) stands for, if any.
fn key_for(page: u16, usage: u16) -> Option<u32> {
    Some(match (page, usage) {
        // Button page: the first is a knob's press.
        (0x09, 1) => key::DPAD_CENTER,
        // Generic desktop D-pad.
        (0x01, 0x90) => key::DPAD_UP,
        (0x01, 0x91) => key::DPAD_DOWN,
        (0x01, 0x92) => key::DPAD_RIGHT,
        (0x01, 0x93) => key::DPAD_LEFT,
        // Consumer: application control and media transport.
        (0x0c, 0x223) => key::HOME,
        (0x0c, 0x224) => key::BACK,
        (0x0c, 0xb0) => key::MEDIA_PLAY,
        (0x0c, 0xb1) => key::MEDIA_PAUSE,
        (0x0c, 0xb5) => key::MEDIA_NEXT,
        (0x0c, 0xb6) => key::MEDIA_PREVIOUS,
        (0x0c, 0xb7) => key::MEDIA_STOP,
        (0x0c, 0xcd) => key::MEDIA_PLAY_PAUSE,
        // Telephony: answer and hang up.
        (0x0b, 0x20) => key::HEADSETHOOK,
        (0x0b, 0x26) => key::ENDCALL,
        _ => return None,
    })
}

/// One input field of a report.
#[derive(Debug, Clone, PartialEq)]
struct Field {
    page: u16,
    usage: u16,
    bit: usize,
    size: usize,
    relative: bool,
    signed: bool,
}

/// What a device's reports carry, from its descriptor; and what they last held, for edges.
#[derive(Debug, Clone)]
pub struct Device {
    pub name: String,
    fields: Vec<Field>,
    last: Vec<i32>,
}

impl Device {
    /// None for a descriptor with nothing mapped onto keys - the touchscreen, say.
    pub fn new(name: &str, descriptor: &[u8]) -> Option<Device> {
        let fields: Vec<Field> = parse_descriptor(descriptor)
            .into_iter()
            .filter(|f| key_for(f.page, f.usage).is_some() || is_dial(f))
            .collect();
        if fields.is_empty() {
            return None;
        }
        Some(Device {
            name: name.to_string(),
            last: vec![0; fields.len()],
            fields,
        })
    }

    /// The key events in one report: a press or release per button that changed, and a press
    /// and release per detent the dial turned.
    pub fn decode(&mut self, report: &[u8]) -> Vec<KeyEvent> {
        let mut events = Vec::new();
        for (i, field) in self.fields.iter().enumerate() {
            let value = read(report, field);
            if is_dial(field) {
                let code = if value > 0 { key::DPAD_DOWN } else { key::DPAD_UP };
                for _ in 0..value.unsigned_abs().min(16) {
                    events.push(KeyEvent { code, down: true });
                    events.push(KeyEvent { code, down: false });
                }
            } else if let Some(code) = key_for(field.page, field.usage) {
                let (was, is) = (self.last[i] != 0, value != 0);
                if was != is {
                    events.push(KeyEvent { code, down: is });
                }
            }
            self.last[i] = value;
        }
        events
    }
}

/// A rotary control: Generic Desktop Dial or Wheel, relative.
fn is_dial(field: &Field) -> bool {
    field.page == 0x01 && matches!(field.usage, 0x37 | 0x38) && field.relative
}

/// The accessory's switchgear, by the `uuid` its reports carry.
pub fn devices<'a>(
    declared: impl IntoIterator<Item = (&'a str, &'a str, &'a [u8])>,
) -> HashMap<String, Device> {
    declared
        .into_iter()
        .filter_map(|(uuid, name, descriptor)| {
            Device::new(name, descriptor).map(|d| (uuid.to_string(), d))
        })
        .collect()
}

fn read(report: &[u8], field: &Field) -> i32 {
    let mut value: u32 = 0;
    for i in 0..field.size.min(32) {
        let bit = field.bit + i;
        let Some(byte) = report.get(bit / 8) else { break };
        if byte >> (bit % 8) & 1 != 0 {
            value |= 1 << i;
        }
    }
    if field.signed && field.size > 0 && field.size < 32 && value >> (field.size - 1) & 1 != 0 {
        (value | !0u32 << field.size) as i32
    } else {
        value as i32
    }
}

/// The input fields a report descriptor lays out, in report order. Enough of HID's item grammar
/// for buttons, consumer keys and dials: global page/min/size/count, local usages, Input mains.
fn parse_descriptor(descriptor: &[u8]) -> Vec<Field> {
    let mut fields = Vec::new();
    let (mut page, mut logical_min, mut size, mut count) = (0u16, 0i32, 0usize, 0usize);
    let mut usages: Vec<(u16, u16)> = Vec::new();
    let mut usage_range: Option<(u32, u32)> = None;
    let mut bit = 0usize;
    let mut i = 0;
    while i < descriptor.len() {
        let prefix = descriptor[i];
        let len = match prefix & 3 {
            3 => 4,
            n => n as usize,
        };
        let data = descriptor.get(i + 1..i + 1 + len).unwrap_or(&[]);
        i += 1 + len;
        let unsigned = data.iter().rev().fold(0u32, |v, b| v << 8 | *b as u32);
        let signed = match *data {
            [b] => b as i8 as i32,
            [lo, hi] => i16::from_le_bytes([lo, hi]) as i32,
            [_, _, _, _] => unsigned as i32,
            _ => 0,
        };
        match (prefix >> 2 & 3, prefix >> 4) {
            // Global.
            (1, 0) => page = unsigned as u16,
            (1, 1) => logical_min = signed,
            (1, 7) => size = unsigned as usize,
            (1, 9) => count = unsigned as usize,
            // Local: a 4-byte usage carries its own page.
            (2, 0) if len == 4 => usages.push(((unsigned >> 16) as u16, unsigned as u16)),
            (2, 0) => usages.push((page, unsigned as u16)),
            (2, 1) => usage_range = Some((unsigned, usage_range.map_or(unsigned, |r| r.1))),
            (2, 2) => usage_range = Some((usage_range.map_or(unsigned, |r| r.0), unsigned)),
            // Main.
            (0, 8) => {
                let constant = unsigned & 1 != 0;
                let relative = unsigned & 4 != 0;
                if let Some((min, max)) = usage_range.take() {
                    usages.extend((min..=max).map(|u| (page, u as u16)));
                }
                for n in 0..count {
                    if !constant {
                        let (field_page, usage) = usages
                            .get(n)
                            .or(usages.last())
                            .copied()
                            .unwrap_or((page, 0));
                        fields.push(Field {
                            page: field_page,
                            usage,
                            bit,
                            size,
                            relative,
                            signed: logical_min < 0,
                        });
                    }
                    bit += size;
                }
                usages.clear();
            }
            (0, _) => {
                usages.clear();
                usage_range = None;
            }
            _ => {}
        }
    }
    fields
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CFMOTO's "Knob & Buttons": select, AC Back, then a signed dial byte.
    const KNOB: &[u8] = &[
        0x05, 0x01, 0x09, 0x08, 0xa1, 0x01, 0x05, 0x09, 0x09, 0x01, 0x15, 0x00, 0x25, 0x01, 0x75,
        0x01, 0x95, 0x01, 0x81, 0x02, 0x05, 0x0c, 0x0a, 0x24, 0x02, 0x95, 0x01, 0x81, 0x02, 0x95,
        0x06, 0x81, 0x01, 0x05, 0x01, 0x09, 0x37, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x01,
        0x81, 0x06, 0xc0,
    ];
    /// Its "Media Buttons": play, pause, next, previous, play/pause.
    const MEDIA: &[u8] = &[
        0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x15, 0x00, 0x25, 0x01, 0x09, 0xb0, 0x09, 0xb1, 0x09,
        0xb5, 0x09, 0xb6, 0x09, 0xcd, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x03, 0x75, 0x01,
        0x81, 0x01, 0xc0,
    ];
    /// Its touchscreen, which carries no keys.
    const TOUCH: &[u8] = &[
        0x05, 0x0d, 0x09, 0x04, 0xa1, 0x01, 0x05, 0x0d, 0x09, 0x22, 0xa1, 0x02, 0x05, 0x0d, 0x09,
        0x33, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x75, 0x07, 0x95, 0x01,
        0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x15, 0x00, 0x26, 0x20, 0x03, 0x75, 0x10, 0x95, 0x01,
        0x81, 0x02, 0x09, 0x31, 0x15, 0x00, 0x26, 0x00, 0x05, 0x75, 0x10, 0x95, 0x01, 0x81, 0x02,
        0xc0, 0xc0,
    ];

    #[test]
    fn touchscreen_has_no_keys() {
        assert!(Device::new("Touch Screen", TOUCH).is_none());
    }

    #[test]
    fn knob_press_and_back_go_down_and_up() {
        let mut knob = Device::new("Knob & Buttons", KNOB).unwrap();
        assert_eq!(knob.decode(&[0b01, 0]), vec![KeyEvent { code: key::DPAD_CENTER, down: true }]);
        assert_eq!(knob.decode(&[0b00, 0]), vec![KeyEvent { code: key::DPAD_CENTER, down: false }]);
        assert_eq!(knob.decode(&[0b10, 0]), vec![KeyEvent { code: key::BACK, down: true }]);
    }

    #[test]
    fn dial_steps_both_ways() {
        let mut knob = Device::new("Knob & Buttons", KNOB).unwrap();
        let down = knob.decode(&[0, 2]);
        assert_eq!(down.len(), 4);
        assert!(down.iter().all(|e| e.code == key::DPAD_DOWN));
        let up = knob.decode(&[0, 0xff]);
        assert_eq!(
            up,
            vec![
                KeyEvent { code: key::DPAD_UP, down: true },
                KeyEvent { code: key::DPAD_UP, down: false },
            ]
        );
    }

    #[test]
    fn media_keys_by_bit() {
        let mut media = Device::new("Media Buttons", MEDIA).unwrap();
        assert_eq!(media.decode(&[0b100]), vec![KeyEvent { code: key::MEDIA_NEXT, down: true }]);
        assert_eq!(media.decode(&[0]), vec![KeyEvent { code: key::MEDIA_NEXT, down: false }]);
    }
}
