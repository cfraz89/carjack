use std::io::{Cursor, Read};

use deku::ctx::Endian;
use deku::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Detect(Detect),
    Iap2(Packet),
}

impl Frame {
    pub fn read(input: &[u8]) -> Result<(Self, usize), DekuError> {
        match Detect::from_bytes((input, 0)) {
            Ok((rest, detect)) => return Ok((Frame::Detect(detect), input.len() - rest.0.len())),
            Err(e @ DekuError::Incomplete(_)) => return Err(e),
            Err(_) => {}
        }
        let (rest, packet) = Packet::from_bytes((input, 0))?;
        Ok((Frame::Iap2(packet), input.len() - rest.0.len()))
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, DekuError> {
        match self {
            Frame::Detect(detect) => detect.to_bytes(),
            Frame::Iap2(packet) => packet.to_bytes(),
        }
    }
}

/// The iAP1 protocol-switch request. Not iAP2 at all: it is the accessory asking the transport to
/// change protocol, and it is nothing but these six bytes.
#[derive(DekuRead, DekuWrite, Debug, Clone, Copy, PartialEq)]
#[deku(magic = b"\xFF\x55\x02\x00\xEE\x10")]
pub struct Detect;

/// (De)serializable iAP2 packet
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(endian = "big")]
pub struct Packet {
    pub header: Header,
    #[deku(
        assert_eq = "header.calculate_checksum()",
        update = "self.header.calculate_checksum()"
    )]
    pub header_checksum: u8,
    #[deku(
        cond = "header.length > 9",
        count = "header.length.saturating_sub(10) as usize"
    )]
    pub payload: Vec<u8>,
    #[deku(
        cond = "!payload.is_empty()",
        assert_eq = "if !payload.is_empty() { Some(payload.calculate_checksum()) } else { None }",
        update = "self.payload.calculate_checksum()"
    )]
    pub payload_checksum: Option<u8>,
}

impl Packet {
    pub fn wire_len(payload_len: usize) -> usize {
        HEADER_SIZE + 1 + payload_len + usize::from(payload_len > 0)
    }

    /// Build a packet, sizing the header's `length` per [`Packet::wire_len`].
    pub fn new(control: ControlByte, seq: u8, ack: u8, session_id: u8, payload: Vec<u8>) -> Self {
        let payload_checksum = (!payload.is_empty()).then(|| payload.calculate_checksum());
        let length = Self::wire_len(payload.len());
        let header = Header {
            length: length as u16,
            control,
            seq,
            ack,
            session_id,
        };
        let header_checksum = header.calculate_checksum();
        Packet {
            header,
            header_checksum,
            payload,
            payload_checksum,
        }
    }
}

pub trait Checksumable {
    fn calculate_checksum(&self) -> u8;
}

#[derive(DekuRead, DekuWrite, DekuSize, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", endian = "endian", magic = b"\xFF\x5A")]
pub struct Header {
    pub length: u16,
    pub control: ControlByte,
    pub seq: u8,
    pub ack: u8,
    pub session_id: u8,
}

const HEADER_SIZE: usize = Header::SIZE_BYTES.unwrap();

impl Checksumable for Header {
    fn calculate_checksum(&self) -> u8 {
        let mut buff = Cursor::new(vec![0; HEADER_SIZE]);
        self.to_writer(&mut Writer::new(&mut buff), Endian::Big)
            .expect("Unable to write control byte");
        buff.set_position(0);
        let sum = buff.bytes().fold(0u8, |a, b| {
            a.wrapping_add(b.expect("Couldn't read byte from header buffer"))
        });
        sum.wrapping_neg()
    }
}

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian, ack: bool, count: usize", endian = "endian")]
pub struct Payload(#[deku(cond = "ack", count = "count")] Vec<u8>);

#[derive(DekuRead, DekuWrite, DekuSize, Debug, Clone, Copy, PartialEq, Default)]
#[deku(ctx = "endian: Endian", endian = "endian")]
pub struct ControlByte {
    #[deku(bits = "1")]
    pub syn: bool, // Link Sync Payload
    #[deku(bits = "1")]
    pub ack: bool, // Acknowledgement Valid
    #[deku(bits = "1")]
    pub eak: bool, // Extended Ack Payload
    #[deku(bits = "1")]
    pub rst: bool, // Link Reset
    #[deku(bits = "1", pad_bits_after = "3")]
    pub sleep: bool, // Device Sleep
}

impl ControlByte {
    pub fn with_ack() -> Self {
        ControlByte {
            ack: true,
            ..Default::default()
        }
    }
}

impl Checksumable for Vec<u8> {
    fn calculate_checksum(&self) -> u8 {
        let sum = self.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        sum.wrapping_neg()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare acknowledgement carries no payload, so it is 9 bytes and its `length` field says
    /// 9. These are the exact bytes the CFMOTO head unit sends us (`cfmoto_8.pcapng` frame
    /// 639) - we have to match, because we previously claimed 10 and sent 9, which cost the
    /// accessory its framing.
    #[test]
    fn empty_packet_is_nine_bytes() {
        let packet = Packet::new(ControlByte::with_ack(), 0xe7, 0xa6, 0x00, Vec::new());
        assert_eq!(packet.header.length, 9);
        assert_eq!(packet.payload_checksum, None);
        assert_eq!(
            packet.to_bytes().unwrap(),
            vec![0xff, 0x5a, 0x00, 0x09, 0x40, 0xe7, 0xa6, 0x00, 0xd1]
        );
    }

    /// With a payload the length also covers the trailing payload checksum. Bytes from
    /// `cfmoto_8.pcapng` frame 622 (our own AuthenticationSucceeded, which the accessory accepted).
    #[test]
    fn payload_packet_length_covers_its_checksum() {
        let packet = Packet::new(
            ControlByte::with_ack(),
            0xa3,
            0xe6,
            0x02,
            vec![0x40, 0x40, 0x00, 0x06, 0xaa, 0x05],
        );
        assert_eq!(packet.header.length, 16);
        assert_eq!(
            packet.to_bytes().unwrap(),
            vec![
                0xff, 0x5a, 0x00, 0x10, 0x40, 0xa3, 0xe6, 0x02, 0xcc, 0x40, 0x40, 0x00, 0x06, 0xaa,
                0x05, 0xcb
            ]
        );
    }

    /// Whatever we emit has to come back through our own decoder unchanged - the empty case
    /// is where an inconsistent `length` would silently desynchronise a stream.
    #[test]
    fn packets_round_trip() {
        for payload in [Vec::new(), vec![0x40, 0x40, 0x00, 0x06, 0xaa, 0x05]] {
            let packet = Packet::new(ControlByte::with_ack(), 0x11, 0x22, 0x02, payload);
            let frame = Frame::Iap2(packet.clone());
            let bytes = frame.to_bytes().unwrap();
            assert_eq!(bytes.len(), packet.header.length as usize);
            let (decoded, consumed) = Frame::read(&bytes).expect("a whole frame");
            assert_eq!(consumed, bytes.len(), "decoder left bytes unconsumed");
            assert_eq!(decoded, frame);
        }
    }

    /// The exact six bytes the CFMOTO head unit repeats once a second - over RFCOMM at the start
    /// of a link, and over the AirPlay tunnel at the handover (`logs/cfmoto-11-08-1855.pcapng`,
    /// event channel t=26.899 onwards). Reading them as a frame is what lets us answer.
    #[test]
    fn detect_round_trips() {
        let bytes = vec![0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10];
        let (frame, consumed) = Frame::read(&bytes).expect("a whole frame");
        assert_eq!(consumed, bytes.len(), "decoder left bytes unconsumed");
        assert_eq!(frame, Frame::Detect(Detect));
        assert_eq!(frame.to_bytes().unwrap(), bytes);
    }

    /// A detect and a packet share their first byte, so only the second tells them apart.
    #[test]
    fn a_detect_is_not_mistaken_for_a_packet() {
        let packet = Packet::new(ControlByte::with_ack(), 0x11, 0x22, 0x02, Vec::new());
        assert_ne!(Frame::Iap2(packet), Frame::Detect(Detect));
        assert!(Frame::read(&[0xFF, 0x56, 0x02, 0x00, 0xEE, 0x10]).is_err());
    }
}
