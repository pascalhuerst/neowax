use super::channels::AudioBuffer;
use super::CHANNELS;
use anyhow::Result;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use symphonia::core::audio::AudioBufferRef;
use symphonia::core::codecs::Decoder;
use symphonia::core::formats::{FormatReader, SeekMode, SeekTo, Track};
use symphonia::core::units::TimeStamp;

pub struct RealtimePlayer {
    decoder: Box<dyn Decoder>,
    reader: Box<dyn FormatReader>,
    track: Track,
    position: Arc<AtomicU64>,
    _sample_rate: u32,
    total_samples: u64,
}

impl RealtimePlayer {
    pub fn new(
        decoder: Box<dyn Decoder>,
        reader: Box<dyn FormatReader>,
        track: Track,
        position: Arc<AtomicU64>,
        sample_rate: u32,
        total_samples: u64,
    ) -> Self {
        Self {
            decoder,
            reader,
            track,
            position,
            _sample_rate: sample_rate,
            total_samples,
        }
    }

    pub fn read_samples(&mut self) -> Result<AudioBuffer> {
        let mut buffer = AudioBuffer::new();
        let current_position = self.position.load(Ordering::SeqCst);

        // Check if we've reached the end
        if current_position >= self.total_samples {
            return Ok(buffer);
        }

        // Read the next packet from the format reader
        match self.reader.next_packet() {
            Ok(packet) => {
                // Decode the packet
                match self.decoder.decode(&packet) {
                    Ok(decoded) => {
                        match decoded {
                            AudioBufferRef::F32(audio_buf) => {
                                // Get the interleaved samples
                                let planes = audio_buf.planes();
                                let plane_buf = planes.planes()[0];

                                // Convert to i16 and add to buffer
                                for &value in plane_buf.iter() {
                                    buffer.push((value * i16::MAX as f32) as i16);
                                }

                                // Update position
                                self.position.fetch_add(
                                    plane_buf.len() as u64 / CHANNELS as u64,
                                    Ordering::SeqCst,
                                );
                            }
                            _ => {
                                tracing::warn!("Unsupported audio format");
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to decode audio: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::error!("Failed to read packet: {}", e);
            }
        }

        Ok(buffer)
    }

    pub fn seek(&mut self, position: u64) -> Result<()> {
        // Convert position to timestamp
        let ts = TimeStamp::from(position);

        // Seek in the reader with accurate seeking mode
        self.reader.seek(
            SeekMode::Accurate,
            SeekTo::TimeStamp {
                ts,
                track_id: self.track.id,
            },
        )?;

        // Reset decoder state
        self.decoder.reset();

        // Update position
        self.position.store(position, Ordering::SeqCst);
        Ok(())
    }
}
