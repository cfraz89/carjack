use std::io::{Cursor, Read};

use deku::ctx::Endian;
use deku::prelude::*;

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
    /// How many bytes a packet carrying `payload_len` bytes of payload occupies on the wire.
    ///
    /// This counts the *whole* packet: the 8-byte header, its checksum, the payload, and - only
    /// when there is a payload - the payload checksum. The empty case is the one worth spelling
    /// out: an empty packet is 9 bytes, not 10, because there is no payload checksum to carry.
    /// That is not a cosmetic off-by-one. The accessory trusts `length` to know how many bytes
    /// this packet occupies, so a keepalive claiming 10 while writing 9 makes it take the first
    /// byte of the *next* packet as the missing one and lose framing for the rest of the session
    /// - it then stops acknowledging anything, retransmits its last message until it gives up,
    /// and drops the link (`cfmoto_8.pcapng`, t=30.068 onwards).
    ///
    /// Exposed separately from `new` so the negotiated `max_length` can be checked against a
    /// payload before a packet is built for it.
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
    /// `cfmoto_8.pcapng` frame 622 (our own AuthenticationSucceeded, which the car accepted).
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
            let bytes = packet.to_bytes().unwrap();
            assert_eq!(bytes.len(), packet.header.length as usize);
            let (rest, decoded) = Packet::from_bytes((&bytes, 0)).unwrap();
            assert_eq!(rest.0.len(), 0, "decoder left bytes unconsumed");
            assert_eq!(decoded, packet);
        }
    }
}
