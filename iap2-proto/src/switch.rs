use bytes::Bytes;

pub const REQUEST_IAP2_SWITCH_BYTES: Bytes =
    Bytes::from_static(&[0xFF, 0x55, 0x02, 0x00, 0xEE, 0x10]);
