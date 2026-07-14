use std::time::Duration;

// Yoinked from iap2-rs, until I figure out what the values are all for
/// Link layer timing configuration
#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub max_retries: u32,
    pub timeout: Duration,
    pub detect_to_syn_delay: Duration,
    pub syn_retry_delay: Duration,
    pub initial_seq: u8,
}

impl Default for LinkConfig {
    fn default() -> Self {
        LinkConfig {
            max_retries: 30,
            timeout: Duration::from_millis(1500),
            detect_to_syn_delay: Duration::from_millis(100),
            syn_retry_delay: Duration::from_millis(150),
            initial_seq: 0x9D,
        }
    }
}
