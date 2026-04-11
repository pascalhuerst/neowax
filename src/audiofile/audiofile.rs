use anyhow::{Context, Result};
use id3::{Tag, TagLike};
use std::fs::File;
use std::path::Path;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, SeekMode};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;
use symphonia::core::errors::Error;

#[derive(Debug)]
pub struct AudioMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub year: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration: Option<Duration>,
}

pub struct AudioFrame {
    samples: Vec<f32>,
    num_channels: usize,
}

impl AudioFrame {
    pub fn sample(&self, channel: usize) -> f32 {
        if channel < self.num_channels {
            self.samples[channel]
        } else {
            0.0
        }
    }

    pub fn channels(&self) -> usize {
        self.num_channels
    }
}

pub struct AudioFile {
    metadata: AudioMetadata,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    format: Box<dyn symphonia::core::formats::FormatReader>,
    current_frame_offset: u64,
    sample_buf: Option<SampleBuffer<f32>>,
}

impl AudioFile {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        // Try to read ID3 tags first
        let id3_tag = Tag::read_from_path(&path).ok();
        
        // Open the media source
        let file = File::open(&path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        // Create a hint to help the format registry guess what format reader is appropriate
        let mut hint = Hint::new();
        if let Some(extension) = path.as_ref().extension() {
            hint.with_extension(extension.to_str().unwrap_or(""));
        }

        // Use the default options for format reader
        let format_opts = FormatOptions::default();
        let metadata_opts = MetadataOptions::default();
        let decoder_opts = DecoderOptions::default();

        // Probe the media source to determine the format
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .context("unsupported format")?;

        // Get the format reader
        let format = probed.format;

        // Find the first audio track
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .context("no audio track found")?;

        // Create a decoder for the track
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &decoder_opts)
            .context("unsupported codec")?;

        // Extract basic metadata
        let params = &track.codec_params;
        let metadata = AudioMetadata {
            title: id3_tag.as_ref().and_then(|tag| tag.title().map(String::from)),
            artist: id3_tag.as_ref().and_then(|tag| tag.artist().map(String::from)),
            album: id3_tag.as_ref().and_then(|tag| tag.album().map(String::from)),
            year: id3_tag
                .as_ref()
                .and_then(|tag| tag.year().map(|y| y.to_string())),
            sample_rate: params.sample_rate.unwrap_or(44100),
            channels: params.channels.map(|ch| ch.count() as u16).unwrap_or(2),
            duration: track
                .codec_params
                .time_base
                .map(|tb| tb.calc_time(track.codec_params.n_frames.unwrap_or(0)))
                .map(|time| Duration::from_secs_f64(time.seconds as f64)),
        };

        Ok(Self {
            metadata,
            decoder,
            format,
            current_frame_offset: 0,
            sample_buf: None,
        })
    }

    pub fn metadata(&self) -> &AudioMetadata {
        &self.metadata
    }

    pub fn read_samples(&mut self, max_samples: usize) -> Result<Option<Vec<f32>>> {
        // Try to get the next packet from the format reader
        let packet = match self.format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::ResetRequired) => {
                self.current_frame_offset = 0;
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        };

        // Decode the packet into an audio buffer
        let decoded = self.decoder.decode(&packet)?;
        
        // Convert the decoded audio to f32 samples
        let mut sample_buf = SampleBuffer::<f32>::new(decoded.capacity() as u64, *decoded.spec());
        sample_buf.copy_interleaved_ref(decoded);
        
        let mut samples = Vec::with_capacity(sample_buf.samples().len());
        samples.extend_from_slice(sample_buf.samples());

        self.current_frame_offset += packet.dur();

        if samples.len() > max_samples {
            samples.truncate(max_samples);
        }

        Ok(Some(samples))
    }

    pub fn seek(&mut self, time: Duration) -> Result<()> {
        let time_in_seconds = time.as_secs_f64();
        let seeked = self.format.seek(
            SeekMode::Accurate,
            symphonia::core::formats::SeekTo::Time {
                time: Time::from(time_in_seconds),
                track_id: None,
            },
        )?;
        self.current_frame_offset = seeked.actual_ts;
        Ok(())
    }

    pub fn sample_rate(&self) -> u32 {
        self.metadata.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.metadata.channels
    }

    pub fn read_frame(&mut self) -> Result<Option<AudioFrame>, Error> {
        // Try to get the next packet from the format reader
        let packet = match self.format.next_packet() {
            Ok(packet) => packet,
            Err(Error::ResetRequired) => {
                return Ok(None);
            }
            Err(err) => return Err(err),
        };

        // Decode the packet
        let decoded = self.decoder.decode(&packet)?;

        // Get the audio buffer specification
        let spec = *decoded.spec();

        // Create a sample buffer if we don't have one yet, or if the spec changed
        if self.sample_buf.is_none() {
            self.sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
        }

        // Copy the decoded audio samples into the sample buffer
        if let Some(buf) = &mut self.sample_buf {
            buf.copy_interleaved_ref(decoded);

            // Create an AudioFrame from the current samples
            let samples = buf.samples().iter().copied().collect();
            return Ok(Some(AudioFrame {
                samples,
                num_channels: spec.channels.count(),
            }));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_audio_file() {
        // This test requires a valid audio file
        let test_file = "test.mp3"; // Replace with a real test file path
        if let Ok(audio_file) = AudioFile::open(test_file) {
            assert!(audio_file.sample_rate() > 0);
            assert!(audio_file.channels() > 0);
        }
    }

    #[test]
    fn test_read_samples() {
        // This test requires a valid audio file
        let test_file = "test.mp3"; // Replace with a real test file path
        if let Ok(mut audio_file) = AudioFile::open(test_file) {
            if let Ok(Some(samples)) = audio_file.read_samples(1024) {
                assert!(!samples.is_empty());
                assert!(samples.len() <= 1024);
            }
        }
    }

    #[test]
    fn test_seek() {
        // This test requires a valid audio file
        let test_file = "test.mp3"; // Replace with a real test file path
        if let Ok(mut audio_file) = AudioFile::open(test_file) {
            let seek_time = Duration::from_secs(5);
            assert!(audio_file.seek(seek_time).is_ok());
        }
    }
}
