#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SessionType {
    Control = 0,
    FileTransfer = 1,
    ExternalAccessory = 2,
}

impl TryFrom<u8> for SessionType {
    type Error = u8;
    fn try_from(v: u8) -> Result<Self, u8> {
        match v {
            0 => Ok(SessionType::Control),
            1 => Ok(SessionType::FileTransfer),
            2 => Ok(SessionType::ExternalAccessory),
            _ => Err(v),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: u8,
    pub session_type: SessionType,
    pub version: u8,
}

#[derive(Debug, Clone, Default)]
pub struct SessionTable {
    pub sessions: Vec<SessionEntry>,
}

impl SessionTable {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
        }
    }

    pub fn add(&mut self, entry: SessionEntry) {
        self.sessions.retain(|s| s.id != entry.id);
        self.sessions.push(entry);
    }

    pub fn get(&self, id: u8) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn find_type(&self, session_type: SessionType) -> Option<&SessionEntry> {
        self.sessions.iter().find(|s| s.session_type == session_type)
    }
}

#[derive(Debug, Clone)]
pub struct SynPayload {
    pub version: u8,
    pub max_outstanding: u8,
    pub max_length: u16,
    pub retx_timeout: u16,
    pub cumack_timeout: u16,
    pub max_retx: u8,
    pub max_cumack: u8,
    pub sessions: Vec<SessionEntry>,
}

impl SynPayload {
    pub fn new() -> Self {
        SynPayload {
            version: 1,
            max_outstanding: 3,
            max_length: 4096,
            retx_timeout: 200,
            cumack_timeout: 100,
            max_retx: 5,
            max_cumack: 1,
            sessions: Vec::new(),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(10 + self.sessions.len() * 3);
        buf.push(self.version);
        buf.push(self.max_outstanding);
        buf.extend_from_slice(&self.max_length.to_be_bytes());
        buf.extend_from_slice(&self.retx_timeout.to_be_bytes());
        buf.extend_from_slice(&self.cumack_timeout.to_be_bytes());
        buf.push(self.max_retx);
        buf.push(self.max_cumack);
        for sess in &self.sessions {
            buf.push(sess.id);
            buf.push(sess.session_type as u8);
            buf.push(sess.version);
        }
        buf
    }

    pub fn from_bytes(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < 10 {
            return Err("SYN payload too short");
        }
        let version = data[0];
        let max_outstanding = data[1];
        let max_length = u16::from_be_bytes([data[2], data[3]]);
        let retx_timeout = u16::from_be_bytes([data[4], data[5]]);
        let cumack_timeout = u16::from_be_bytes([data[6], data[7]]);
        let max_retx = data[8];
        let max_cumack = data[9];
        let mut sessions = Vec::new();
        let mut off = 10;
        while off + 3 <= data.len() {
            let id = data[off];
            let raw_type = data[off + 1];
            let version = data[off + 2];
            let session_type = SessionType::try_from(raw_type)
                .unwrap_or(SessionType::ExternalAccessory);
            sessions.push(SessionEntry { id, session_type, version });
            off += 3;
        }
        Ok(SynPayload {
            version,
            max_outstanding,
            max_length,
            retx_timeout,
            cumack_timeout,
            max_retx,
            max_cumack,
            sessions,
        })
    }
}
