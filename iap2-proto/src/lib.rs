use std::io::{Cursor, Read};

use deku::ctx::Endian;
use deku::prelude::*;

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(endian = "big")]
pub struct Packet {
    pub header: Header,
    #[deku(assert_eq = "header.checksum()", update = "self.header.checksum()")]
    pub header_checksum: u8,
    #[deku(
        cond = "header.control.ack",
        count = "header.length.saturating_sub(10) as usize"
    )]
    pub payload: Vec<u8>,
    #[deku(
        cond = "!payload.is_empty()",
        assert_eq = "if !payload.is_empty() { Some(gen_payload_checksum(payload)) } else { None }"
    )]
    pub payload_checksum: Option<u8>,
}

impl Packet {
    pub fn parse_from_bytes(input: &[u8]) -> Result<((&[u8], usize), Packet), DekuError> {
        Packet::from_bytes((input, 0))
    }
}

fn gen_payload_checksum(payload: &Vec<u8>) -> u8 {
    let sum = payload.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    sum.wrapping_neg()
}

#[derive(DekuRead, DekuWrite, DekuSize, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", endian = "endian", magic = b"\xFF\x5A")]
pub struct Header {
    pub length: u16,
    pub control: ControlByte,
    pub seq: u8,
    #[deku(cond = "control.ack")]
    pub ack: u8,
    #[deku(cond = "control.ack")]
    pub session_id: u8,
}

const HEADER_SIZE: usize = Header::SIZE_BYTES.unwrap();

impl Header {
    fn checksum(self: &Self) -> u8 {
        let mut buff = Cursor::new(vec![0; HEADER_SIZE]);
        self.to_writer(&mut Writer::new(&mut buff), Endian::Big)
            .expect("Unable to write control byte");
        let sum = buff.bytes().fold(0u8, |a, b| {
            a.wrapping_add(b.expect("Couldn't read byt from header buffer"))
        });
        sum.wrapping_neg()
    }
}

#[derive(DekuRead, DekuWrite, DekuSize, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", endian = "endian")]
pub struct ControlByte {
    #[deku(bits = "1")]
    pub syn: bool,
    #[deku(bits = "1")]
    pub ack: bool,
    #[deku(bits = "1")]
    pub eak: bool,
    #[deku(bits = "1")]
    pub rst: bool,
    #[deku(bits = "1")]
    pub sleep: bool,
    #[deku(bits = "3")]
    pub _unused: u8,
}
