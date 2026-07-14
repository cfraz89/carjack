use bytes::{BufMut, Bytes, BytesMut};

pub struct EaFrame {
    pub transfer_id: u16,
    pub data: Bytes,
}

impl EaFrame {
    pub fn new(transfer_id: u16, data: Bytes) -> Self {
        EaFrame { transfer_id, data }
    }

    pub fn to_bytes(&self) -> Bytes {
        let mut buf = BytesMut::with_capacity(2 + self.data.len());
        buf.put_u16(self.transfer_id);
        buf.put_slice(&self.data);
        buf.freeze()
    }

    pub fn from_bytes(data: &[u8]) -> Result<(Self, &[u8]), &'static str> {
        if data.is_empty() {
            return Err("EA frame empty");
        }
        let transfer_id = if data.len() >= 2 {
            u16::from_be_bytes([data[0], data[1]])
        } else {
            0x0000
        };
        let offset = if data.len() >= 2 { 2 } else { 0 };
        let rest = Bytes::copy_from_slice(&data[offset..]);
        Ok((EaFrame { transfer_id, data: rest }, &[]))
    }
}
