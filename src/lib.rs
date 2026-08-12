//! Library surface of the carjack Linux binary. The internal compositor and the H264 encode
//! pipeline are exposed here (rather than living only as binary modules) so integration tests
//! under `tests/` can drive them without an accessory - see `tests/compositor.rs`.
pub mod compositor;
pub mod config;
pub mod display_cache;
pub mod encode;
