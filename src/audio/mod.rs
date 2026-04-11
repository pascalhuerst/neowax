mod audio_devices;
mod buffer;
mod channels;
mod engine;
mod player;
pub mod realtime;
mod realtime_player;
mod track;

pub use audio_devices::*;
pub use buffer::*;
pub use channels::{
    create_audio_channels, AudioBuffer, AudioRingBufferConsumer, AudioRingBufferProducer,
    InputChannel, OutputChannel, FRAMES_PER_BUFFER, MAX_AUDIO_BUFFERS, SAMPLES_PER_BUFFER,
};
pub use engine::*;
pub use player::Player;
pub use realtime_player::*;
pub use track::Track;

pub const CHANNELS: usize = 2;
