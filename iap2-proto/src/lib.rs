use std::io::{Cursor, Read};

use deku::ctx::Endian;
use deku::prelude::*;

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
        cond = "header.control.ack",
        count = "header.length.saturating_sub(10) as usize"
    )]
    pub payload: Vec<u8>,
    #[deku(
        cond = "!payload.is_empty()",
        assert_eq = "if !payload.is_empty() { Some(payload.calculate_checksum()) } else { None }"
    )]
    pub payload_checksum: Option<u8>,
}

trait Checksumable {
    fn calculate_checksum(&self) -> u8;
}

pub const IAP2_HANDSHAKE: [u8; 6] = [0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10];

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
        let sum = buff.bytes().fold(0u8, |a, b| {
            a.wrapping_add(b.expect("Couldn't read byt from header buffer"))
        });
        sum.wrapping_neg()
    }
}

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian, ack: bool, count: usize", endian = "endian")]
pub struct Payload(#[deku(cond = "ack", count = "count")] Vec<u8>);

#[derive(DekuRead, DekuWrite, DekuSize, Debug, Clone, PartialEq, Default)]
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

impl Checksumable for Vec<u8> {
    fn calculate_checksum(&self) -> u8 {
        let sum = self.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        sum.wrapping_neg()
    }
}
