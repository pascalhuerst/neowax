//! A compact summary of a track for drawing its waveform.
//!
//! The track is reduced to one entry per 5 ms ("hop"): the mono minimum and
//! maximum, and running sums of the signal energy in three frequency bands
//! (bass < 200 Hz, mids, highs > 4 kHz) plus the full signal. That is a few
//! dozen bytes per hop instead of several floats per sample, and it can
//! answer both questions the UI asks: what does the track look like as a
//! whole (smoothed band levels), and what does this moment look like up
//! close (true peaks and RMS).

/// Length of one hop in seconds.
const HOP_SECS: f64 = 0.005;
/// Window for the overview reference level, in seconds.
const OVERVIEW_WINDOW_SECS: f64 = 1.2;

/// Series index for `rms` / `mean_square`.
pub const LOW: usize = 0;
pub const MID: usize = 1;
pub const HIGH: usize = 2;
pub const FULL: usize = 3;

#[derive(Debug, Clone)]
pub struct WaveformData {
    rate: u32,
    hop_frames: usize,
    frames: usize,
    /// Mono minimum / maximum per hop, in -1.0..=1.0
    min: Vec<f32>,
    max: Vec<f32>,
    /// Prefix sums of squared samples per series (len = hops + 1)
    energy: [Vec<f64>; 4],
    /// Typical (97th percentile) smoothed level of each band, for normalising
    overview_ref: [f32; 3],
    /// Typical (99.5th percentile) peak level, for normalising the close-up
    peak_ref: f32,
}

impl WaveformData {
    pub fn from_pcm(pcm: &[i16], rate: u32) -> Self {
        let hop_frames = ((rate as f64 * HOP_SECS) as usize).max(1);
        let frames = pcm.len() / 2;
        let dt = 1.0 / rate as f64;
        // Single-pole filters: alpha = dt / (RC + dt)
        let rc_low = 1.0 / (2.0 * std::f64::consts::PI * 200.0);
        let rc_high = 1.0 / (2.0 * std::f64::consts::PI * 4000.0);
        let alpha_low = dt / (rc_low + dt);
        let alpha_high = dt / (rc_high + dt);

        let hops = frames.div_ceil(hop_frames);
        let mut min = Vec::with_capacity(hops);
        let mut max = Vec::with_capacity(hops);
        let mut energy: [Vec<f64>; 4] = std::array::from_fn(|_| {
            let mut v = Vec::with_capacity(hops + 1);
            v.push(0.0);
            v
        });
        let (mut lp_low, mut lp_high) = (0.0f64, 0.0f64);

        for hop in pcm.chunks(hop_frames * 2) {
            let (mut lo_v, mut hi_v) = (f32::MAX, f32::MIN);
            let mut sums = [0.0f64; 4];
            for pair in hop.as_chunks::<2>().0 {
                let mono = (pair[0] as f64 + pair[1] as f64) / 2.0 / 32768.0;
                lp_low += alpha_low * (mono - lp_low);
                lp_high += alpha_high * (mono - lp_high);
                let band = [lp_low, lp_high - lp_low, mono - lp_high, mono];
                for (sum, value) in sums.iter_mut().zip(band) {
                    *sum += value * value;
                }
                lo_v = lo_v.min(mono as f32);
                hi_v = hi_v.max(mono as f32);
            }
            min.push(lo_v.min(0.0));
            max.push(hi_v.max(0.0));
            for (series, sum) in energy.iter_mut().zip(sums) {
                series.push(series.last().copied().unwrap_or(0.0) + sum);
            }
        }

        let mut data = Self {
            rate,
            hop_frames,
            frames,
            min,
            max,
            energy,
            overview_ref: [1e-3; 3],
            peak_ref: 1e-3,
        };
        data.compute_references();
        data
    }

    fn compute_references(&mut self) {
        let hops = self.hops();
        if hops == 0 {
            return;
        }
        let window = ((OVERVIEW_WINDOW_SECS / HOP_SECS) as usize).clamp(1, hops);
        for band in [LOW, MID, HIGH] {
            let levels: Vec<f32> = (0..=hops - window)
                .step_by((window / 2).max(1))
                .map(|a| self.rms(band, a, a + window))
                .collect();
            self.overview_ref[band] = percentile(levels, 97.0).max(1e-3);
        }
        let peaks: Vec<f32> = self
            .min
            .iter()
            .zip(&self.max)
            .map(|(lo, hi)| lo.abs().max(*hi))
            .collect();
        self.peak_ref = percentile(peaks, 99.5).max(1e-3);
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.rate as f64
    }

    /// Length of one hop in seconds (the time resolution of this data).
    pub fn hop_secs(&self) -> f64 {
        self.hop_frames as f64 / self.rate as f64
    }

    pub fn hops(&self) -> usize {
        self.min.len()
    }

    pub fn overview_ref(&self) -> [f32; 3] {
        self.overview_ref
    }

    pub fn peak_ref(&self) -> f32 {
        self.peak_ref
    }

    /// Lowest and highest sample over hops `a..b` (empty range gives zeros).
    pub fn min_max(&self, a: usize, b: usize) -> (f32, f32) {
        let b = b.min(self.hops());
        if a >= b {
            return (0.0, 0.0);
        }
        let lo = self.min[a..b].iter().copied().fold(0.0, f32::min);
        let hi = self.max[a..b].iter().copied().fold(0.0, f32::max);
        (lo, hi)
    }

    /// Mean squared level of `series` over hops `a..b`.
    pub fn mean_square(&self, series: usize, a: usize, b: usize) -> f64 {
        let b = b.min(self.hops());
        if a >= b {
            return 0.0;
        }
        let sums = &self.energy[series];
        (sums[b] - sums[a]).max(0.0) / ((b - a) * self.hop_frames) as f64
    }

    /// RMS level of `series` over hops `a..b`.
    pub fn rms(&self, series: usize, a: usize, b: usize) -> f32 {
        self.mean_square(series, a, b).sqrt() as f32
    }
}

/// `p`-th percentile (0-100) of `values`; 0.0 if there are none.
fn percentile(mut values: Vec<f32>, p: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    values[((values.len() - 1) as f64 * p / 100.0).round() as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44100;

    /// Stereo sine of `freq` Hz and `amp` (0..1) lasting `secs`.
    fn sine(freq: f64, amp: f64, secs: f64) -> Vec<i16> {
        let n = (RATE as f64 * secs) as usize;
        (0..n)
            .flat_map(|i| {
                let v = (i as f64 / RATE as f64 * freq * std::f64::consts::TAU).sin();
                let s = (v * amp * 32767.0) as i16;
                [s, s]
            })
            .collect()
    }

    #[test]
    fn duration_and_hops_follow_the_track_length() {
        let data = WaveformData::from_pcm(&sine(100.0, 0.5, 3.0), RATE);
        assert!((data.duration_secs() - 3.0).abs() < 1e-6);
        assert_eq!(data.hops(), (3.0f64 / data.hop_secs()).ceil() as usize);
    }

    #[test]
    fn peaks_and_rms_match_a_sine() {
        let data = WaveformData::from_pcm(&sine(440.0, 0.5, 2.0), RATE);
        let (lo, hi) = data.min_max(0, data.hops());
        assert!(
            (hi - 0.5).abs() < 0.01 && (lo + 0.5).abs() < 0.01,
            "{lo} {hi}"
        );
        let rms = data.rms(FULL, 0, data.hops());
        assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.01, "{rms}");
    }

    #[test]
    fn bands_separate_bass_from_treble() {
        let bass = WaveformData::from_pcm(&sine(60.0, 0.5, 2.0), RATE);
        let (n, ms) = (bass.hops(), 1.0);
        assert!(bass.rms(LOW, 0, n) > ms * 3.0 * bass.rms(HIGH, 0, n));

        let treble = WaveformData::from_pcm(&sine(10_000.0, 0.5, 2.0), RATE);
        let n = treble.hops();
        assert!(treble.rms(HIGH, 0, n) > 3.0 * treble.rms(LOW, 0, n));
    }

    #[test]
    fn references_are_positive_and_silence_is_safe() {
        let loud = WaveformData::from_pcm(&sine(100.0, 0.8, 4.0), RATE);
        assert!(loud.overview_ref().iter().all(|&r| r > 0.0));
        assert!((loud.peak_ref() - 0.8).abs() < 0.02);

        let silent = WaveformData::from_pcm(&vec![0i16; RATE as usize * 4], RATE);
        assert!(silent.peak_ref() > 0.0);
        assert_eq!(silent.rms(FULL, 0, silent.hops()), 0.0);

        let empty = WaveformData::from_pcm(&[], RATE);
        assert_eq!(empty.hops(), 0);
        assert_eq!(empty.min_max(0, 10), (0.0, 0.0));
    }

    #[test]
    fn ranges_are_clamped() {
        let data = WaveformData::from_pcm(&sine(100.0, 0.5, 1.0), RATE);
        let n = data.hops();
        assert_eq!(data.rms(FULL, n, n + 100), 0.0);
        assert_eq!(data.rms(FULL, 5, 5), 0.0);
        assert!(data.rms(FULL, 0, n + 100) > 0.0);
    }
}
