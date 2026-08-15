use serde::Serialize;

/// The SDP attribute ids in the record.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
#[repr(u16)]
pub enum SdpAttributeId {
    ServiceClassIdList = 0x0001,
    ProtocolDescriptorList = 0x0004,
    BrowseGroupList = 0x0005,
    LanguageBaseAttributeIdList = 0x0006,
    ServiceAvailability = 0x0008,
    BluetoothProfileDescriptorList = 0x0009,
    ServiceName = 0x0100,
}

impl serde::Serialize for SdpAttributeId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{:#06x}", *self as u16))
    }
}

/// The language in the `LanguageBaseAttributeIdList` attribute: ISO 639-2 `"en"`.
pub const LANGUAGE_EN: u16 = 0x656e;
/// The character encoding there: the IANA charset number for UTF-8.
pub const ENCODING_UTF8: u16 = 0x006a;

/// The `ServiceAvailability` value when the service is up: 0xFF = 100%.
pub const FULLY_AVAILABLE: u8 = 0xff;

/// Serial Port profile v1.2, in `BluetoothProfileDescriptorList` as `(major, minor)`.
pub const SERIAL_PORT_VERSION_1_2: u16 = 0x0102;

/// A value in an SDP record: a scalar carrying its bytes in a `value` attribute, or a `sequence`
/// of more values. quick-xml writes each variant's name as the element tag and inlines `$value`
/// content without wrappers.
#[derive(Serialize)]
pub enum SdpValue {
    #[serde(rename = "uuid")]
    Uuid {
        #[serde(rename = "@value")]
        value: String,
    },
    #[serde(rename = "uint8")]
    Uint8 {
        #[serde(rename = "@value")]
        value: String,
    },
    #[serde(rename = "uint16")]
    Uint16 {
        #[serde(rename = "@value")]
        value: String,
    },
    #[serde(rename = "text")]
    Text {
        #[serde(rename = "@value")]
        value: String,
    },
    #[serde(rename = "sequence")]
    Sequence {
        #[serde(rename = "$value")]
        children: Vec<SdpValue>,
    },
}

impl From<btuuid::BluetoothUuid16> for SdpValue {
    fn from(value: btuuid::BluetoothUuid16) -> Self {
        SdpValue::Uuid {
            value: format!("{:#06x}", value.to_u16()),
        }
    }
}

impl From<uuid::Uuid> for SdpValue {
    fn from(value: uuid::Uuid) -> Self {
        SdpValue::Uuid {
            value: value.to_string(),
        }
    }
}

impl From<u8> for SdpValue {
    fn from(value: u8) -> Self {
        SdpValue::Uint8 {
            value: format!("{value:#04x}"),
        }
    }
}

impl From<u16> for SdpValue {
    fn from(value: u16) -> Self {
        SdpValue::Uint16 {
            value: format!("{value:#06x}"),
        }
    }
}

impl From<&str> for SdpValue {
    fn from(value: &str) -> Self {
        SdpValue::Text {
            value: value.into(),
        }
    }
}

/// A tuple is a `<sequence>`: each element converts to its kind, so a mixed `(uuid, uint8)` is
/// allowed in a way a `vec![...]` cannot be.
impl<A: Into<SdpValue>> From<(A,)> for SdpValue {
    fn from(value: (A,)) -> Self {
        SdpValue::Sequence {
            children: vec![value.0.into()],
        }
    }
}

impl<A: Into<SdpValue>, B: Into<SdpValue>> From<(A, B)> for SdpValue {
    fn from(value: (A, B)) -> Self {
        SdpValue::Sequence {
            children: vec![value.0.into(), value.1.into()],
        }
    }
}

impl<A: Into<SdpValue>, B: Into<SdpValue>, C: Into<SdpValue>> From<(A, B, C)> for SdpValue {
    fn from(value: (A, B, C)) -> Self {
        SdpValue::Sequence {
            children: vec![value.0.into(), value.1.into(), value.2.into()],
        }
    }
}

/// One SDP attribute: an `id` with a single value.
#[derive(Serialize)]
#[serde(rename = "attribute")]
pub struct SdpAttribute {
    #[serde(rename = "@id")]
    id: SdpAttributeId,
    #[serde(rename = "$value")]
    value: SdpValue,
}

pub fn attribute(id: SdpAttributeId, value: impl Into<SdpValue>) -> SdpAttribute {
    SdpAttribute {
        id,
        value: value.into(),
    }
}

/// The full SDP service record, serialized with `record` as its root tag.
#[derive(Serialize)]
#[serde(rename = "record")]
pub struct SdpRecord {
    pub attribute: Vec<SdpAttribute>,
}

impl SdpRecord {
    pub fn to_xml(&self) -> String {
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n");
        let mut ser = quick_xml::se::Serializer::new(&mut xml);
        ser.indent(' ', 2);
        self.serialize(ser)
            .expect("XML Serialization should never fail");
        xml
    }
}
