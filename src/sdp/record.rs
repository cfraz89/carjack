use deku::prelude::*;

// ---------------------------------------------------------------------------
// Helpers for variable-length payloads (size_desc 5 / 6 / 7)
// ---------------------------------------------------------------------------

/// One-byte length prefix, then `len` bytes of data.
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "_: deku::ctx::Endian")]
pub struct VarLen8 {
    pub len: u8,
    #[deku(count = "len")]
    pub data: Vec<u8>,
}

/// Two-byte (big-endian) length prefix, then `len` bytes of data.
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(endian = "big", ctx = "_: deku::ctx::Endian")]
pub struct VarLen16 {
    pub len: u16,
    #[deku(count = "len")]
    pub data: Vec<u8>,
}

/// Four-byte (big-endian) length prefix, then `len` bytes of data.
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(endian = "big", ctx = "_: deku::ctx::Endian")]
pub struct VarLen32 {
    pub len: u32,
    #[deku(count = "len")]
    pub data: Vec<u8>,
}

// ---------------------------------------------------------------------------
// DataElement
// ---------------------------------------------------------------------------

/// An SDP Data Element decoded from its wire tag byte.
///
/// The tag byte packs `type_id (5 bits) | size_desc (3 bits)`.
/// Each valid `(type_id, size_desc)` combination has its own variant
/// so that the `data` field is typed appropriately.
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(id_type = "u8", endian = "big")]
pub enum DataElement {

    // -- Nil ----------------------------------------------------------------
    #[deku(id = "0b00000_000")]
    Nil,

    // -- Unsigned integers --------------------------------------------------
    #[deku(id = "0b00001_000")]
    U8(u8),
    #[deku(id = "0b00001_001")]
    U16(u16),
    #[deku(id = "0b00001_010")]
    U32(u32),
    #[deku(id = "0b00001_011")]
    U64(u64),
    #[deku(id = "0b00001_100")]
    U128(u128),

    // -- Signed integers ----------------------------------------------------
    #[deku(id = "0b00010_000")]
    S8(i8),
    #[deku(id = "0b00010_001")]
    S16(i16),
    #[deku(id = "0b00010_010")]
    S32(i32),
    #[deku(id = "0b00010_011")]
    S64(i64),
    // -- UUIDs --------------------------------------------------------------
    #[deku(id = "0b00011_001")]
    UUID16(u16),
    #[deku(id = "0b00011_010")]
    UUID32(u32),
    #[deku(id = "0b00011_100")]
    UUID128([u8; 16]),

    // -- Text strings (variable length) ------------------------------------
    #[deku(id = "0b00100_101")]
    String1(VarLen8),
    #[deku(id = "0b00100_110")]
    String2(VarLen16),
    #[deku(id = "0b00100_111")]
    String4(VarLen32),

    // -- Boolean ------------------------------------------------------------
    #[deku(id = "0b00101_000")]
    Bool(bool),

    // -- Sequences ----------------------------------------------------------
    #[deku(id = "0b00110_101")]
    Seq1(VarLen8),
    #[deku(id = "0b00110_110")]
    Seq2(VarLen16),
    #[deku(id = "0b00110_111")]
    Seq4(VarLen32),

    // -- Alternatives -------------------------------------------------------
    #[deku(id = "0b00111_101")]
    Alt1(VarLen8),
    #[deku(id = "0b00111_110")]
    Alt2(VarLen16),
    #[deku(id = "0b00111_111")]
    Alt4(VarLen32),

    // -- URLs ---------------------------------------------------------------
    #[deku(id = "0b01000_101")]
    Url1(VarLen8),
    #[deku(id = "0b01000_110")]
    Url2(VarLen16),
    #[deku(id = "0b01000_111")]
    Url4(VarLen32),
}

// ---------------------------------------------------------------------------
// DataElement helper methods
// ---------------------------------------------------------------------------

/// Pick the smallest size descriptor for a given data byte-length.
fn pick_size_desc(len: usize) -> u8 {
    match len {
        0 => 0,
        1 => 0,
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        n if n <= 0xFF => 5,
        n if n <= 0xFFFF => 6,
        n if n <= 0xFFFF_FFFF => 7,
        _ => panic!("SDP data element too large ({len} bytes)"),
    }
}

impl DataElement {
    // -- raw data access for variable-length variants -----------------------

    fn var_data(&self) -> Option<&[u8]> {
        match self {
            DataElement::String1(v)
            | DataElement::Url1(v)
            | DataElement::Seq1(v)
            | DataElement::Alt1(v) => Some(&v.data),
            DataElement::String2(v)
            | DataElement::Url2(v)
            | DataElement::Seq2(v)
            | DataElement::Alt2(v) => Some(&v.data),
            DataElement::String4(v)
            | DataElement::Url4(v)
            | DataElement::Seq4(v)
            | DataElement::Alt4(v) => Some(&v.data),
            _ => None,
        }
    }

    // -- typed accessors ----------------------------------------------------

    /// Interpret the element as a `u8` (must be `U8`).
    pub fn as_u8(&self) -> Option<u8> {
        match self {
            DataElement::U8(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret the element as a `u16` (must be `U16`).
    pub fn as_u16(&self) -> Option<u16> {
        match self {
            DataElement::U16(v) => Some(*v),
            _ => None,
        }
    }

    /// Interpret the element as a UUID (UUID16 / UUID32 / UUID128).
    pub fn as_uuid(&self) -> Option<uuid::Uuid> {
        match self {
            DataElement::UUID16(id) => Some(uuid::Uuid::from_u128(*id as u128)),
            DataElement::UUID32(id) => Some(uuid::Uuid::from_u128(*id as u128)),
            DataElement::UUID128(bytes) => Some(uuid::Uuid::from_bytes(*bytes)),
            _ => None,
        }
    }

    /// Parse the raw data of a SEQ / ALT variant into its child elements.
    pub fn parse_seq_data(&self) -> Result<Vec<DataElement>, DekuError> {
        let data = self.var_data().unwrap_or(&[]);
        if data.is_empty() {
            return Ok(vec![]);
        }
        let mut elements = Vec::new();
        let mut remaining = data;
        while !remaining.is_empty() {
            let ((rest, _), el) = DataElement::from_bytes((remaining, 0))?;
            elements.push(el);
            remaining = rest;
        }
        Ok(elements)
    }

    // -- constructors -------------------------------------------------------

    pub fn nil() -> Self {
        DataElement::Nil
    }

    pub fn u8(v: u8) -> Self {
        DataElement::U8(v)
    }

    pub fn u16(v: u16) -> Self {
        DataElement::U16(v)
    }

    pub fn uuid16(v: u16) -> Self {
        DataElement::UUID16(v)
    }

    pub fn uuid128(v: [u8; 16]) -> Self {
        DataElement::UUID128(v)
    }

    pub fn text(v: Vec<u8>) -> Self {
        let len = v.len();
        match pick_size_desc(len) {
            5 => DataElement::String1(VarLen8 { len: len as u8, data: v }),
            6 => DataElement::String2(VarLen16 { len: len as u16, data: v }),
            7 => DataElement::String4(VarLen32 { len: len as u32, data: v }),
            _ => unreachable!(),
        }
    }

    /// Build a SEQ from child elements.
    pub fn seq(elements: Vec<DataElement>) -> Self {
        let mut data = Vec::new();
        for el in elements {
            data.extend(el.to_bytes().unwrap());
        }
        let len = data.len();
        match pick_size_desc(len) {
            5 => DataElement::Seq1(VarLen8 { len: len as u8, data }),
            6 => DataElement::Seq2(VarLen16 { len: len as u16, data }),
            7 => DataElement::Seq4(VarLen32 { len: len as u32, data }),
            _ => unreachable!(),
        }
    }
}

// ---------------------------------------------------------------------------
// Attribute
// ---------------------------------------------------------------------------

/// An SDP attribute (attribute ID + value).
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub id: DataElement,
    pub value: DataElement,
}

// ---------------------------------------------------------------------------
// SdpRecord
// ---------------------------------------------------------------------------

/// A parsed SDP record.
///
/// Wire format: a `DataElement::Seq` containing interleaved
/// attribute-ID / attribute-value pairs.
#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
pub struct SdpRecord(pub DataElement);

impl SdpRecord {
    /// Iterate over the attributes stored in the inner Seq.
    fn parse_attributes(&self) -> Result<Vec<Attribute>, DekuError> {
        let elements = self.0.parse_seq_data()?;
        let mut attrs = Vec::new();
        let mut iter = elements.into_iter();
        while let Some(id_de) = iter.next() {
            let value = iter
                .next()
                .ok_or(DekuError::Parse("attribute value missing after ID".into()))?;
            attrs.push(Attribute {
                id: id_de,
                value,
            });
        }
        Ok(attrs)
    }

    /// Find the first attribute with the given ID.
    pub fn find_attribute(&self, id: u16) -> Option<Attribute> {
        self.parse_attributes()
            .ok()?
            .into_iter()
            .find(|a| a.id.as_u16() == Some(id))
    }

    pub fn service_class_ids(&self) -> Vec<uuid::Uuid> {
        self.find_attribute(0x0001)
            .into_iter()
            .flat_map(|attr| attr.value.parse_seq_data().unwrap_or_default())
            .filter_map(|de| de.as_uuid())
            .collect()
    }

    /// Extract the RFCOMM channel number from the ProtocolDescriptorList
    /// (attribute ID 0x0004) or AdditionalProtocolDescriptorLists (0x000D).
    pub fn rfcomm_channel(&self) -> Option<u16> {
        let attr = self
            .find_attribute(0x0004)
            .or_else(|| self.find_attribute(0x000D))?;
        let protos = attr.value.parse_seq_data().ok()?;
        for proto in &protos {
            let stack = proto.parse_seq_data().ok()?;
            for (i, item) in stack.iter().enumerate() {
                if item.as_uuid() == Some(uuid::Uuid::from_u128(0x0003)) {
                    if let Some(next) = stack.get(i + 1) {
                        return next.as_u8().map(|v| v as u16).or_else(|| next.as_u16());
                    }
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nil() {
        let data = [0b00000_000];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::Nil);
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uint8() {
        let data = [0b00001_000, 0x42];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::U8(0x42));
        assert_eq!(de.as_u8(), Some(0x42));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uint16() {
        let data = [0b00001_001, 0x01, 0x02];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::U16(0x0102));
        assert_eq!(de.as_u16(), Some(0x0102));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_uuid16() {
        let data = [0b00011_001, 0x01, 0x00];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::UUID16(0x0100));
        assert_eq!(de.as_uuid(), Some(uuid::Uuid::from_u128(0x0100)));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_bool_true() {
        let data = [0b00101_000, 0x01];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::Bool(true));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_bool_false() {
        let data = [0b00101_000, 0x00];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::Bool(false));
        assert!(rest.is_empty());
    }

    #[test]
    fn test_text() {
        let data = [0b00100_101, 0x03, b'f', b'o', b'o'];
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(
            de,
            DataElement::String1(VarLen8 {
                len: 3,
                data: b"foo".to_vec()
            })
        );
        assert_eq!(de.var_data(), Some(&b"foo"[..]));
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
        let ((rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(
            de,
            DataElement::Seq1(VarLen8 {
                len: 4,
                data: vec![0b00001_000, 0x01, 0b00001_000, 0x02]
            })
        );
        let elements = de.parse_seq_data().unwrap();
        assert_eq!(elements.len(), 2);
        assert_eq!(elements[0].as_u8(), Some(1));
        assert_eq!(elements[1].as_u8(), Some(2));
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

        let ((_rest, _), seq) = DataElement::from_bytes((&record_data, 0)).unwrap();
        let record = SdpRecord(seq);
        let attrs = record.parse_attributes().unwrap();
        assert_eq!(attrs.len(), 2);
        assert_eq!(attrs[0].id.as_u16(), Some(0x0001));
        assert_eq!(record.rfcomm_channel(), Some(5));
    }

    #[test]
    fn test_roundtrip_uint16() {
        let original = DataElement::U16(0x1234);
        let bytes = original.to_bytes().unwrap();
        let ((_rest, _), parsed) = DataElement::from_bytes((&bytes, 0)).unwrap();
        assert_eq!(parsed, DataElement::U16(0x1234));
        assert_eq!(parsed.as_u16(), Some(0x1234));
    }

    #[test]
    fn test_seq_roundtrip() {
        let inner = DataElement::seq(vec![
            DataElement::u8(1),
            DataElement::U16(0x0200),
            DataElement::text(b"hello".to_vec()),
        ]);
        let bytes = inner.to_bytes().unwrap();
        let ((_rest, _), parsed) = DataElement::from_bytes((&bytes, 0)).unwrap();
        let elements = parsed.parse_seq_data().unwrap();
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0], DataElement::U8(1));
        assert_eq!(elements[1], DataElement::U16(0x0200));
        assert_eq!(
            elements[2],
            DataElement::String1(VarLen8 {
                len: 5,
                data: b"hello".to_vec()
            })
        );
    }

    #[test]
    fn test_uuid128_parse() {
        let uuid_bytes: [u8; 16] = [
            0x02, 0x03, 0x03, 0x02, 0x1d, 0x19, 0x41, 0x5f, 0x86, 0xf2, 0x22, 0xa2, 0x10, 0x6a,
            0x0a, 0x77,
        ];
        let data = [
            0b00011_100, // UUID type, size_desc=4 (16 bytes)
            uuid_bytes[0], uuid_bytes[1], uuid_bytes[2], uuid_bytes[3],
            uuid_bytes[4], uuid_bytes[5], uuid_bytes[6], uuid_bytes[7],
            uuid_bytes[8], uuid_bytes[9], uuid_bytes[10], uuid_bytes[11],
            uuid_bytes[12], uuid_bytes[13], uuid_bytes[14], uuid_bytes[15],
        ];
        let ((_rest, _), de) = DataElement::from_bytes((&data, 0)).unwrap();
        assert_eq!(de, DataElement::UUID128(uuid_bytes));
        assert_eq!(de.as_uuid(), Some(uuid::Uuid::from_bytes(uuid_bytes)));
    }

    #[test]
    fn test_service_class_ids_with_uuid128() {
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

        let ((_rest, _), seq) = DataElement::from_bytes((&record_data, 0)).unwrap();
        let record = SdpRecord(seq);
        let ids = record.service_class_ids();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], uuid::Uuid::from_bytes(uuid_bytes));
    }
}
