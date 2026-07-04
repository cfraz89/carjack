use std::iter;

use deku::prelude::*;
use zbus::zvariant::dbus::StructSeqSerializer::Seq;

pub mod typ {
    pub const NIL: u8 = 0;
    pub const UINT: u8 = 1;
    pub const SINT: u8 = 2;
    pub const UUID: u8 = 3;
    pub const TEXT: u8 = 4;
    pub const BOOL: u8 = 5;
    pub const SEQ: u8 = 6;
    pub const ALT: u8 = 7;
    pub const URL: u8 = 8;
}

/// SDP Data Element header byte.
///
/// Bit layout: `[7:3] type_id (5 bits)` | `[2:0] size_desc (3 bits)`
///
/// size_desc interpretation:
///   0-4   → data length = 2^size_desc bytes (1, 2, 4, 8, 16)
///   5     → 1 additional uint8 byte gives the data length
///   6     → 2 additional uint16 bytes give the data length
///   7     → 4 additional uint32 bytes give the data length
#[derive(DekuRead, Debug, Clone, PartialEq)]
pub struct DataElementHeader {
    #[deku(bits = "5")]
    pub type_id: u8,
    #[deku(bits = "3")]
    pub size_desc: u8,
}

/// A parsed SDP Data Element value.
#[derive(Debug, Clone, PartialEq)]
pub enum DataElement {
    Nil,
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    Uuid16(u16),
    Uuid32(u32),
    Uuid128([u8; 16]),
    Text(Vec<u8>),
    Bool(bool),
    Seq(Vec<DataElement>),
    Alt(Vec<DataElement>),
    Url(Vec<u8>),
}

/// An SDP attribute (attribute ID + value).
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub id: u16,
    pub value: DataElement,
}

/// A parsed SDP record containing a sequence of attributes.
#[derive(Debug, Clone, PartialEq)]
pub struct SdpRecord {
    pub attributes: Vec<Attribute>,
}

// ---------------------------------------------------------------------------
// Size resolution helpers
// ---------------------------------------------------------------------------

/// Resolve the data length from a size descriptor.
///
/// Returns `(size_header_bytes, data_length)` where `size_header_bytes` is
/// how many bytes of the length prefix were consumed (0 for fixed sizes).
fn resolve_size(size_desc: u8, input: &[u8]) -> Result<(usize, usize), DekuError> {
    match size_desc {
        0..=4 => Ok((0, 1 << size_desc)),
        5 => {
            let len = *input.first().ok_or(DekuError::Parse(
                "size_desc 5: missing 1-byte length field".into(),
            ))?;
            Ok((1, len as usize))
        }
        6 => {
            if input.len() < 2 {
                return Err(DekuError::Parse(
                    "size_desc 6: missing 2-byte length field".into(),
                ));
            }
            let len = u16::from_be_bytes([input[0], input[1]]);
            Ok((2, len as usize))
        }
        7 => {
            if input.len() < 4 {
                return Err(DekuError::Parse(
                    "size_desc 7: missing 4-byte length field".into(),
                ));
            }
            let len = u32::from_be_bytes([input[0], input[1], input[2], input[3]]);
            Ok((4, len as usize))
        }
        _ => Err(DekuError::Parse("invalid size descriptor".into())),
    }
}

/// Pick the smallest size descriptor for a given data byte-length.
///
/// Returns `(size_desc, header_bytes)` where `header_bytes` is the optional
/// length-prefix (empty for fixed sizes).
fn pick_size_desc(data_len: usize) -> (u8, Vec<u8>) {
    match data_len {
        1 => (0, vec![]),
        2 => (1, vec![]),
        4 => (2, vec![]),
        8 => (3, vec![]),
        16 => (4, vec![]),
        n if n <= 0xFF => (5, vec![n as u8]),
        n if n <= 0xFFFF => (6, (n as u16).to_be_bytes().to_vec()),
        n if n <= 0xFFFF_FFFF => (7, (n as u32).to_be_bytes().to_vec()),
        _ => panic!("SDP data element too large ({data_len} bytes)"),
    }
}

// ---------------------------------------------------------------------------
// Value byte readers
// ---------------------------------------------------------------------------

fn read_uint(data: &[u8]) -> Result<DataElement, DekuError> {
    match data.len() {
        1 => Ok(DataElement::U8(data[0])),
        2 => Ok(DataElement::U16(u16::from_be_bytes([data[0], data[1]]))),
        4 => Ok(DataElement::U32(u32::from_be_bytes([
            data[0], data[1], data[2], data[3],
        ]))),
        8 => Ok(DataElement::U64(u64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]))),
        16 => {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(data);
            Ok(DataElement::U128(u128::from_be_bytes(arr)))
        }
        n if n < 16 => {
            let mut padded = [0u8; 16];
            padded[16 - n..].copy_from_slice(data);
            Ok(DataElement::U128(u128::from_be_bytes(padded)))
        }
        _ => Err(DekuError::Parse("unsigned integer exceeds 128 bits".into())),
    }
}

fn read_sint(data: &[u8]) -> Result<DataElement, DekuError> {
    match data.len() {
        1 => Ok(DataElement::I8(data[0] as i8)),
        2 => Ok(DataElement::I16(i16::from_be_bytes([data[0], data[1]]))),
        4 => Ok(DataElement::I32(i32::from_be_bytes([
            data[0], data[1], data[2], data[3],
        ]))),
        8 => Ok(DataElement::I64(i64::from_be_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]))),
        16 => {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(data);
            Ok(DataElement::I128(i128::from_be_bytes(arr)))
        }
        _ => Err(DekuError::Parse("signed integer exceeds 128 bits".into())),
    }
}

fn read_uuid(data: &[u8]) -> Result<DataElement, DekuError> {
    match data.len() {
        2 => Ok(DataElement::Uuid16(u16::from_be_bytes([data[0], data[1]]))),
        4 => Ok(DataElement::Uuid32(u32::from_be_bytes([
            data[0], data[1], data[2], data[3],
        ]))),
        16 => {
            let mut arr = [0u8; 16];
            arr.copy_from_slice(data);
            Ok(DataElement::Uuid128(arr))
        }
        _ => Err(DekuError::Parse("unsupported UUID width".into())),
    }
}

fn parse_seq(data: &[u8]) -> Result<Vec<DataElement>, DekuError> {
    let mut elements = Vec::new();
    let mut remaining = data;
    while !remaining.is_empty() {
        let (rest, element) = DataElement::from_bytes(remaining)?;
        elements.push(element);
        remaining = rest;
    }
    Ok(elements)
}

fn write_seq(elements: &[DataElement]) -> Vec<u8> {
    let mut buf = Vec::new();
    for el in elements {
        buf.extend(el.to_bytes());
    }
    buf
}

// ---------------------------------------------------------------------------
// DataElement
// ---------------------------------------------------------------------------

impl DataElement {
    /// Parse a `DataElement` from the front of `input`, returning the
    /// remaining slice and the parsed element.
    pub fn from_bytes(input: &[u8]) -> Result<(&[u8], Self), DekuError> {
        let (rest, header) = DataElementHeader::from_bytes((input, 0))
            .map_err(|_| DekuError::Parse("failed to read data element header".into()))?;
        let rest = rest.0;

        let min_extra = match header.size_desc {
            5 => 1,
            6 => 2,
            7 => 4,
            _ => 0,
        };
        if rest.len() < min_extra {
            return Err(DekuError::Parse("input too short".into()));
        }

        let (size_len, data_len) = resolve_size(header.size_desc, rest)?;

        if rest.len() < size_len + data_len {
            return Err(DekuError::Parse("input too short for data".into()));
        }

        let data = &rest[size_len..size_len + data_len];
        let remaining = &rest[size_len + data_len..];

        let value = match header.type_id {
            0 => DataElement::Nil,
            1 => read_uint(data)?,
            2 => read_sint(data)?,
            3 => read_uuid(data)?,
            4 => DataElement::Text(data.to_vec()),
            5 => DataElement::Bool(data.first().map_or(false, |&b| b != 0)),
            6 => DataElement::Seq(parse_seq(data)?),
            7 => DataElement::Alt(parse_seq(data)?),
            8 => DataElement::Url(data.to_vec()),
            t => {
                return Err(DekuError::Parse(
                    format!("unknown data element type {t}").into(),
                ));
            }
        };

        Ok((remaining, value))
    }

    /// Serialize this `DataElement` into bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            DataElement::Nil => vec![0b00000_000],
            _ => {
                let type_id = self.type_id();
                let raw_data = self.raw_data();
                let data_len = raw_data.len();
                let (size_desc, prefix) = pick_size_desc(data_len);
                let header_byte: u8 = (type_id << 3) | size_desc;
                [vec![header_byte], prefix, raw_data].concat()
            }
        }
    }

    fn type_id(&self) -> u8 {
        match self {
            DataElement::Nil => 0,
            DataElement::U8(_)
            | DataElement::U16(_)
            | DataElement::U32(_)
            | DataElement::U64(_)
            | DataElement::U128(_) => 1,
            DataElement::I8(_)
            | DataElement::I16(_)
            | DataElement::I32(_)
            | DataElement::I64(_)
            | DataElement::I128(_) => 2,
            DataElement::Uuid16(_) | DataElement::Uuid32(_) | DataElement::Uuid128(_) => 3,
            DataElement::Text(_) => 4,
            DataElement::Bool(_) => 5,
            DataElement::Seq(_) => 6,
            DataElement::Alt(_) => 7,
            DataElement::Url(_) => 8,
        }
    }

    fn raw_data(&self) -> Vec<u8> {
        match self {
            DataElement::Nil => vec![],
            DataElement::U8(v) => v.to_be_bytes().to_vec(),
            DataElement::U16(v) => v.to_be_bytes().to_vec(),
            DataElement::U32(v) => v.to_be_bytes().to_vec(),
            DataElement::U64(v) => v.to_be_bytes().to_vec(),
            DataElement::U128(v) => v.to_be_bytes().to_vec(),
            DataElement::I8(v) => v.to_be_bytes().to_vec(),
            DataElement::I16(v) => v.to_be_bytes().to_vec(),
            DataElement::I32(v) => v.to_be_bytes().to_vec(),
            DataElement::I64(v) => v.to_be_bytes().to_vec(),
            DataElement::I128(v) => v.to_be_bytes().to_vec(),
            DataElement::Uuid16(v) => v.to_be_bytes().to_vec(),
            DataElement::Uuid32(v) => v.to_be_bytes().to_vec(),
            DataElement::Uuid128(v) => v.to_vec(),
            DataElement::Text(v) => v.clone(),
            DataElement::Bool(v) => vec![*v as u8],
            DataElement::Seq(v) => write_seq(v),
            DataElement::Alt(v) => write_seq(v),
            DataElement::Url(v) => v.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Attribute
// ---------------------------------------------------------------------------

impl Attribute {
    /// Parse a single attribute (UINT16 ID + DataElement value).
    pub fn from_bytes(input: &[u8]) -> Result<(&[u8], Self), DekuError> {
        let (rest, id_de) = DataElement::from_bytes(input)?;
        let id = match id_de {
            DataElement::U16(id) => id,
            _ => {
                return Err(DekuError::Parse(
                    "expected UINT16 data element for attribute ID".into(),
                ));
            }
        };
        let (rest, value) = DataElement::from_bytes(rest)?;
        Ok((rest, Attribute { id, value }))
    }

    /// Serialize this attribute into bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = DataElement::U16(self.id).to_bytes();
        buf.extend(self.value.to_bytes());
        buf
    }
}

// ---------------------------------------------------------------------------
// SdpRecord
// ---------------------------------------------------------------------------

impl SdpRecord {
    /// Parse an SDP record from a byte slice.
    ///
    /// The top-level element must be a `DataElement::Seq` containing interleaved
    /// attribute-ID / attribute-value pairs.
    pub fn from_bytes(input: &[u8]) -> Result<Self, DekuError> {
        let (_rest, top) = DataElement::from_bytes(input)?;
        let elements = match top {
            DataElement::Seq(e) => e,
            _ => {
                return Err(DekuError::Parse(
                    "SDP record must be a DataElementSequence".into(),
                ));
            }
        };

        let mut attributes = Vec::new();
        let mut iter = elements.into_iter();
        while let Some(id_de) = iter.next() {
            let id = match id_de {
                DataElement::U16(id) => id,
                other => {
                    return Err(DekuError::Parse(
                        format!("expected UINT16 attribute ID, got {other:?}").into(),
                    ));
                }
            };
            let value = iter
                .next()
                .ok_or(DekuError::Parse("attribute value missing after ID".into()))?;
            attributes.push(Attribute { id, value });
        }

        Ok(SdpRecord { attributes })
    }

    /// Serialize this record into bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut attr_bytes = Vec::new();
        for attr in &self.attributes {
            attr_bytes.extend(attr.to_bytes());
        }
        let (sd, prefix) = pick_size_desc(attr_bytes.len());
        let header_byte: u8 = (typ::SEQ << 3) | sd;
        [vec![header_byte], prefix, attr_bytes].concat()
    }

    /// Find the first attribute with the given ID.
    pub fn find_attribute(&self, id: u16) -> Option<&Attribute> {
        self.attributes.iter().find(|a| a.id == id)
    }

    pub fn service_class_ids(&self) -> Vec<uuid::Uuid> {
        self.find_attribute(0x0001)
            .into_iter()
            .flat_map(|attr| match &attr.value {
                DataElement::Seq(seq) => seq.iter(),
                _ => [].iter(),
            })
            .filter_map(|de| match de {
                DataElement::Uuid16(id) => Some(uuid::Uuid::from_u128(*id as u128)),
                DataElement::Uuid32(id) => Some(uuid::Uuid::from_u128(*id as u128)),
                DataElement::Uuid128(bytes) => Some(uuid::Uuid::from_bytes(*bytes)),
                _ => None,
            })
            .collect()
    }

    /// Extract the RFCOMM channel number from the ProtocolDescriptorList
    /// (attribute ID 0x0004) or AdditionalProtocolDescriptorLists (0x000D).
    pub fn rfcomm_channel(&self) -> Option<u16> {
        let DataElement::Seq(ref protos) = self
            .find_attribute(0x0004)
            .or_else(|| self.find_attribute(0x000D))?
            .value
        else {
            return None;
        };
        for proto in protos {
            let stack = match proto {
                DataElement::Seq(s) => s,
                _ => continue,
            };
            for (i, item) in stack.iter().enumerate() {
                if matches!(item, DataElement::Uuid16(0x0003)) {
                    if let Some(next) = stack.get(i + 1) {
                        return match next {
                            DataElement::U8(ch) => Some(*ch as u16),
                            DataElement::U16(ch) => Some(*ch),
                            _ => None,
                        };
                    }
                }
            }
        }
        return None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nil() {
        let data = [0b00000_000];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Nil);
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uint8() {
        let data = [0b00001_000, 0x42];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::U8(0x42));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uint16() {
        let data = [0b00001_001, 0x01, 0x02];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::U16(0x0102));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uuid16() {
        let data = [0b00011_001, 0x01, 0x00];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Uuid16(0x0100));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_bool_true() {
        let data = [0b00101_000, 0x01];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Bool(true));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_bool_false() {
        let data = [0b00101_000, 0x00];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Bool(false));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_text() {
        let data = [0b00100_101, 0x03, b'f', b'o', b'o'];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Text(b"foo".to_vec()));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_seq_of_uint8() {
        let data = [
            0b00110_101, // SEQ, variable uint8 length
            0x04,        // length = 4
            0b00001_000,
            0x01, // UINT8(1)
            0b00001_000,
            0x02, // UINT8(2)
        ];
        let (rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(
            de,
            DataElement::Seq(vec![DataElement::U8(1), DataElement::U8(2)])
        );
        assert!(rest.is_empty());
    }

    #[test]
    fn test_sdp_record_rfcomm() {
        let record_data = [
            0b00110_101, // SEQ, uint8 length
            0x18,        // length = 24
            // ServiceClassIDList (0x0001)
            0b00001_001,
            0x00,
            0x01, // UINT16(0x0001)
            0b00110_101,
            0x03, // SEQ, 3 bytes
            0b00011_001,
            0xFF,
            0xFE, // UUID16(0xFFFE)
            // ProtocolDescriptorList (0x0004)
            0b00001_001,
            0x00,
            0x04, // UINT16(0x0004)
            0b00110_101,
            0x0C, // SEQ, 12 bytes
            0b00110_101,
            0x0A, // SEQ, 10 bytes
            0b00011_001,
            0x01,
            0x00, // UUID16(L2CAP)
            0b00011_001,
            0x00,
            0x03, // UUID16(RFCOMM)
            0b00001_000,
            0x05, // UINT8(5) = RFCOMM channel
        ];

        let record = SdpRecord::from_bytes(&record_data).unwrap();
        assert_eq!(record.attributes.len(), 2);
        assert_eq!(record.attributes[0].id, 0x0001);
        assert_eq!(record.rfcomm_channel(), Some(5));
    }

    #[test]
    fn test_roundtrip_uint16() {
        let original = DataElement::U16(0x1234);
        let bytes = original.to_bytes();
        let (_rest, parsed) = DataElement::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_seq_roundtrip() {
        let original = DataElement::Seq(vec![
            DataElement::U8(1),
            DataElement::U16(0x0200),
            DataElement::Text(b"hello".to_vec()),
        ]);
        let bytes = original.to_bytes();
        let (_rest, parsed) = DataElement::from_bytes(&bytes).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn test_uuid128_parse() {
        let uuid_bytes: [u8; 16] = [
            0x02, 0x03, 0x03, 0x02, 0x1d, 0x19, 0x41, 0x5f, 0x86, 0xf2, 0x22, 0xa2, 0x10, 0x6a,
            0x0a, 0x77,
        ];
        let data = [
            0b00011_100, // UUID type, size_desc=4 (16 bytes)
            uuid_bytes[0],
            uuid_bytes[1],
            uuid_bytes[2],
            uuid_bytes[3],
            uuid_bytes[4],
            uuid_bytes[5],
            uuid_bytes[6],
            uuid_bytes[7],
            uuid_bytes[8],
            uuid_bytes[9],
            uuid_bytes[10],
            uuid_bytes[11],
            uuid_bytes[12],
            uuid_bytes[13],
            uuid_bytes[14],
            uuid_bytes[15],
        ];
        let (_rest, de) = DataElement::from_bytes(&data).unwrap();
        assert_eq!(de, DataElement::Uuid128(uuid_bytes));
    }

    #[test]
    fn test_service_class_ids_with_uuid128() {
        // Build a minimal SDP record with ServiceClassIDList (0x0001)
        // containing a 128-bit UUID.
        // Top-level SEQ, variable uint8 length
        //   UINT16(0x0001) = attribute ID
        //   SEQ, variable uint8 length
        //     UUID128(02 03 03 02 1d 19 41 5f 86 f2 22 a2 10 6a 0a 77)
        let uuid_bytes: [u8; 16] = [
            0x02, 0x03, 0x03, 0x02, 0x1d, 0x19, 0x41, 0x5f, 0x86, 0xf2, 0x22, 0xa2, 0x10, 0x6a,
            0x0a, 0x77,
        ];
        let mut record_data = vec![
            0b00110_101, // SEQ, variable uint8 length
            0x16,        // total inner length = 22
            0b00001_001,
            0x00,
            0x01, // UINT16(0x0001) — ServiceClassIDList
            0b00110_101,
            0x11,        // SEQ, 17 bytes: UUID header + 16 bytes
            0b00011_100, // UUID type, size_desc=4 (16 bytes)
        ];
        record_data.extend_from_slice(&uuid_bytes);

        let parsed = SdpRecord::from_bytes(&record_data).unwrap();
        let ids = parsed.service_class_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], uuid::Uuid::from_bytes(uuid_bytes));
    }
}
