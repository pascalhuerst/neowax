mod lookup_table;
mod media_definition;
pub mod pitch;
mod timecoder;

pub use lookup_table::Lut;
pub use media_definition::TimecodeDef;
pub use timecoder::{Timecoder, TimecoderChannel};
pub use pitch::Pitch;

// Constants used across the module
pub(crate) const ZERO_THRESHOLD: i32 = 128 << 16;
pub(crate) const ZERO_RC: f64 = 0.001; // time constant for zero/rumble filter
pub(crate) const REF_PEAKS_AVG: i32 = 48; // in wave cycles
pub(crate) const VALID_BITS: u32 = 24;
pub(crate) const MONITOR_DECAY_EVERY: u32 = 512; // in samples
pub(crate) const TIMECODER_CHANNELS: usize = 2;
