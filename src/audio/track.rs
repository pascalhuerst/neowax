use super::waveform_data::WaveformData;
use anyhow::Result;
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

const TRACK_CHANNELS: usize = 2;

/// Pre-decoded audio track stored entirely in memory as interleaved stereo i16 PCM.
pub struct Track {
    pcm: Vec<i16>,
    /// Sample rate in Hz
    pub rate: u32,
    /// Length in frames (number of stereo sample pairs)
    pub length: usize,
}

impl Track {
    /// Decode an audio file completely into memory.
    /// If `progress` is provided, it will be called with values from 0.0 to 1.0.
    pub fn from_file(path: &str) -> Result<Arc<Self>> {
        Self::from_file_with_progress(path, &|_| {})
    }

    /// Decode an audio file with progress reporting (0.0 to 1.0).
    pub fn from_file_with_progress(path: &str, progress: &dyn Fn(f32)) -> Result<Arc<Self>> {
        let file_size = std::fs::metadata(path)?.len() as f64;
        let file = Box::new(std::fs::File::open(path)?);
        let mss = MediaSourceStream::new(file, Default::default());

        let probed = symphonia::default::get_probe().format(
            &Hint::new(),
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;
        let track_info = format
            .default_track()
            .ok_or_else(|| anyhow::anyhow!("No audio track found"))?;
        let track_id = track_info.id;
        let rate = track_info.codec_params.sample_rate.unwrap_or(44100);

        let mut decoder = symphonia::default::get_codecs()
            .make(&track_info.codec_params, &DecoderOptions::default())?;

        let mut pcm: Vec<i16> = Vec::new();
        let mut sample_buf: Option<SampleBuffer<f32>> = None;
        let mut num_channels = 0usize;
        let mut bytes_read = 0u64;

        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != track_id {
                continue;
            }

            bytes_read += packet.data.len() as u64;
            // Decode phase is 0.0..0.9, waveform generation is 0.9..1.0
            if file_size > 0.0 {
                progress(((bytes_read as f64 / file_size) * 0.9).min(0.9) as f32);
            }

            match decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let capacity = decoded.capacity();

                    if sample_buf.is_none() {
                        num_channels = spec.channels.count();
                        sample_buf = Some(SampleBuffer::<f32>::new(capacity as u64, spec));
                    }

                    let buf = sample_buf.as_mut().unwrap();
                    buf.copy_interleaved_ref(decoded);
                    let samples = buf.samples();

                    if num_channels >= 2 {
                        for chunk in samples.chunks(num_channels) {
                            pcm.push((chunk[0].clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                            pcm.push((chunk[1].clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
                        }
                    } else {
                        // Mono: duplicate to stereo
                        for &s in samples {
                            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
                            pcm.push(v);
                            pcm.push(v);
                        }
                    }
                }
                Err(_) => continue,
            }
        }

        let length = pcm.len() / TRACK_CHANNELS;

        tracing::info!(
            "Loaded track: {} frames, {:.1}s @ {}Hz",
            length,
            length as f64 / rate as f64,
            rate
        );

        Ok(Arc::new(Self { pcm, rate, length }))
    }

    /// Create a track from raw PCM data (for testing).
    #[cfg(test)]
    pub fn from_pcm(pcm: Vec<i16>, rate: u32) -> Arc<Self> {
        let length = pcm.len() / 2;
        Arc::new(Self { pcm, rate, length })
    }

    /// Summarise the track for drawing its waveform.
    pub fn waveform_data(&self) -> WaveformData {
        WaveformData::from_pcm(&self.pcm, self.rate)
    }

    /// Get stereo sample pair at the given frame index.
    /// Returns [0, 0] if out of bounds.
    #[inline]
    pub fn get_sample(&self, s: isize) -> [i16; 2] {
        if s < 0 || s as usize >= self.length {
            [0, 0]
        } else {
            let idx = s as usize * TRACK_CHANNELS;
            [self.pcm[idx], self.pcm[idx + 1]]
        }
    }
}
