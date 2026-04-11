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

        loop {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(_) => break,
            };

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
                        sample_buf =
                            Some(SampleBuffer::<f32>::new(capacity as u64, spec));
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

    /// Extract mono waveform data as f32 for UI display.
    /// Mixes stereo to mono and normalizes to -1.0..1.0.
    pub fn waveform_data(&self) -> Vec<f32> {
        self.pcm
            .chunks(2)
            .map(|pair| {
                let mono = (pair[0] as f32 + pair[1] as f32) / 2.0;
                mono / i16::MAX as f32
            })
            .collect()
    }

    /// Compute per-sample frequency band energies for colored waveform display.
    /// Returns (low, mid, high) vectors of absolute amplitude per mono sample.
    /// Low = bass (<200Hz), Mid = 200-4000Hz, High = treble (>4000Hz).
    pub fn frequency_bands(&self) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let rate = self.rate as f64;
        let dt = 1.0 / rate;

        // Single-pole IIR filter coefficients: alpha = dt / (RC + dt)
        let rc_low = 1.0 / (2.0 * std::f64::consts::PI * 200.0);
        let rc_high = 1.0 / (2.0 * std::f64::consts::PI * 4000.0);
        let alpha_low = dt / (rc_low + dt);
        let alpha_high = dt / (rc_high + dt);

        let len = self.length;
        let mut low = Vec::with_capacity(len);
        let mut mid = Vec::with_capacity(len);
        let mut high = Vec::with_capacity(len);

        let mut lp_low: f64 = 0.0; // low-pass at 200 Hz
        let mut lp_high: f64 = 0.0; // low-pass at 4000 Hz

        for pair in self.pcm.chunks(2) {
            let mono = (pair[0] as f64 + pair[1] as f64) / 2.0 / i16::MAX as f64;

            lp_low += alpha_low * (mono - lp_low);
            lp_high += alpha_high * (mono - lp_high);

            let lo = lp_low;
            let hi = mono - lp_high;
            let mi = lp_high - lp_low;

            low.push(lo.abs() as f32);
            mid.push(mi.abs() as f32);
            high.push(hi.abs() as f32);
        }

        (low, mid, high)
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
