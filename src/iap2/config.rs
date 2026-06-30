// Yoinked from iap2-rs, until I figure out what the values are all for
/// Link layer timing configuration
#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub max_retries: u32,
    pub timeout_ms: u64,
    pub detect_to_syn_delay_ms: u64,
    pub syn_retry_delay_ms: u64,
    pub iap1_probe_timeout_ms: u64,
    pub initial_seq: u8,
}

impl Default for LinkConfig {
    fn default() -> Self {
        LinkConfig {
            max_retries: 30,
            timeout_ms: 1500,
            detect_to_syn_delay_ms: 100,
            syn_retry_delay_ms: 150,
            iap1_probe_timeout_ms: 150,
            initial_seq: 0x9D,
        }
    }
}
