use super::media_definition::{self, TimecodeDef};
use super::pitch::Pitch;
use super::{
    MONITOR_DECAY_EVERY, REF_PEAKS_AVG, TIMECODER_CHANNELS, VALID_BITS, ZERO_RC, ZERO_THRESHOLD,
};

#[derive(Debug)]
pub struct TimecoderChannel {
    positive: bool,
    swapped: bool,
    zero: i32,
    crossing_ticker: u32,
}

impl TimecoderChannel {
    fn new() -> Self {
        Self {
            positive: false,
            swapped: false,
            zero: 0,
            crossing_ticker: 0,
        }
    }

    fn detect_zero_crossing(&mut self, v: i32, threshold: i32, zero_alpha: f64) {
        self.crossing_ticker += 1;
        self.swapped = false;

        if v > self.zero + threshold && !self.positive {
            self.swapped = true;
            self.positive = true;
            self.crossing_ticker = 0;
        } else if v < self.zero - threshold && self.positive {
            self.swapped = true;
            self.positive = false;
            self.crossing_ticker = 0;
        }

        self.zero += (zero_alpha * (v - self.zero) as f64) as i32;
    }
}

#[derive(Debug)]
pub struct Timecoder {
    def: TimecodeDef,
    speed: f64,
    dt: f64,
    zero_alpha: f64,
    threshold: i32,
    forwards: bool,
    primary: TimecoderChannel,
    secondary: TimecoderChannel,
    pitch: Pitch,
    ref_level: i32,
    bitstream: u32,
    timecode: u32,
    valid_counter: u32,
    timecode_ticker: u32,
    monitor: Option<Vec<u8>>,
    mon_size: i32,
    mon_counter: i32,
}

impl Timecoder {
    pub fn new(def: TimecodeDef, speed: f64, sample_rate: u32, phono: bool) -> Self {
        let dt = 1.0 / sample_rate as f64;
        let zero_alpha = dt / (ZERO_RC + dt);
        let mut threshold = ZERO_THRESHOLD;

        if phono {
            threshold >>= 5; // approx -36dB
        }

        Self {
            def,
            speed,
            dt,
            zero_alpha,
            threshold,
            forwards: true,
            primary: TimecoderChannel::new(),
            secondary: TimecoderChannel::new(),
            pitch: Pitch::new(dt),
            ref_level: i32::MAX,
            bitstream: 0,
            timecode: 0,
            valid_counter: 0,
            timecode_ticker: 0,
            monitor: None,
            mon_size: 0,
            mon_counter: 0,
        }
    }

    /// Reset tracking state without rebuilding the LUT.
    pub fn reset(&mut self) {
        self.forwards = true;
        self.primary = TimecoderChannel::new();
        self.secondary = TimecoderChannel::new();
        self.pitch.reset();
        self.ref_level = i32::MAX;
        self.bitstream = 0;
        self.timecode = 0;
        self.valid_counter = 0;
        self.timecode_ticker = 0;
    }

    pub fn monitor_init(&mut self, size: i32) -> Result<(), std::io::Error> {
        self.mon_size = size;
        let buffer_size = (size * size) as usize;
        let mut monitor = vec![0u8; buffer_size];
        monitor.fill(0);
        self.monitor = Some(monitor);
        self.mon_counter = 0;
        Ok(())
    }

    pub fn monitor_clear(&mut self) {
        self.monitor = None;
    }

    pub fn get_monitor(&self) -> Option<Vec<u8>> {
        self.monitor.clone()
    }

    pub fn get_valid_counter(&self) -> u32 {
        self.valid_counter
    }

    pub fn get_bitstream(&self) -> u32 {
        self.bitstream
    }

    pub fn get_timecode(&self) -> u32 {
        self.timecode
    }

    pub fn has_position(&self) -> bool {
        self.def.lut_lookup(self.bitstream).is_some()
    }

    pub fn get_position(&self) -> Option<(i32, f64)> {
        if self.valid_counter <= VALID_BITS {
            return None;
        }

        match self.def.lut_lookup(self.bitstream) {
            Some(pos) => Some((pos, self.timecode_ticker as f64 * self.dt)),
            None => None,
        }
    }

    pub fn get_pitch(&self) -> f64 {
        self.pitch.current() / self.speed
    }

    pub fn get_safe(&self) -> u32 {
        self.def.safe
    }

    pub fn get_resolution(&self) -> f64 {
        self.def.resolution as f64 * self.speed
    }

    pub fn revs_per_sec(&self) -> f64 {
        (33.0 + 1.0 / 3.0) * self.speed / 60.0
    }

    fn update_monitor(&mut self, x: i32, y: i32) {
        if let Some(monitor) = self.monitor.as_mut() {
            self.mon_counter += 1;

            if self.mon_counter % MONITOR_DECAY_EVERY as i32 == 0 {
                for p in monitor.iter_mut() {
                    if *p > 0 {
                        *p = ((*p as u16 * 7) / 8) as u8;
                    }
                }
            }

            let size = self.mon_size;
            let ref_level = self.ref_level;

            // Prevent division by zero and handle small ref_level values
            if ref_level <= 0 {
                return;
            }

            // Scale down the input values first to prevent overflow
            let x_scaled = (x / 8) as i64;
            let y_scaled = (y / 8) as i64;
            let size_scaled = size as i64;
            let ref_scaled = ref_level as i64;

            // Calculate pixel positions with scaled values
            let px = (size_scaled / 2 + (x_scaled * size_scaled) / ref_scaled) as i32;
            let py = (size_scaled / 2 + (y_scaled * size_scaled) / ref_scaled) as i32;

            if px >= 0 && px < size && py >= 0 && py < size {
                monitor[(py * size + px) as usize] = 0xff; // white
            }
        }
    }

    pub fn submit(&mut self, pcm: &[i16]) {
        for chunk in pcm.chunks(TIMECODER_CHANNELS) {
            if chunk.len() < 2 {
                break;
            }

            let left = (chunk[0] as i32) << 16;
            let right = (chunk[1] as i32) << 16;

            let (primary, secondary) = if self.def.flags & media_definition::SWITCH_PRIMARY != 0 {
                (left, right)
            } else {
                (right, left)
            };

            self.process_sample(primary, secondary);
            self.update_monitor(left, right);
        }
    }

    fn process_sample(&mut self, primary: i32, secondary: i32) {
        self.primary
            .detect_zero_crossing(primary, self.threshold, self.zero_alpha);
        self.secondary
            .detect_zero_crossing(secondary, self.threshold, self.zero_alpha);

        if self.primary.swapped || self.secondary.swapped {
            let forwards = if self.primary.swapped {
                self.primary.positive != self.secondary.positive
            } else {
                self.primary.positive == self.secondary.positive
            };

            let forwards = if self.def.flags & media_definition::SWITCH_PHASE != 0 {
                !forwards
            } else {
                forwards
            };

            if forwards != self.forwards {
                self.forwards = forwards;
                self.valid_counter = 0;
            }
        }

        if !self.primary.swapped && !self.secondary.swapped {
            self.pitch.dt_observation(0.0);
        } else {
            let mut dx = 1.0 / self.def.resolution as f64 / 4.0;
            if !self.forwards {
                dx = -dx;
            }
            self.pitch.dt_observation(dx);
        }

        if self.secondary.swapped
            && self.primary.positive == (self.def.flags & media_definition::SWITCH_POLARITY == 0)
        {
            let m = (primary / 2 - self.primary.zero / 2).abs();
            self.process_bitstream(m);
        }

        self.timecode_ticker += 1;
    }

    fn process_bitstream(&mut self, m: i32) {
        let b = m > self.ref_level;

        if self.forwards {
            self.timecode = self.def.fwd(self.timecode);
            self.bitstream = (self.bitstream >> 1) + ((b as u32) << (self.def.bits - 1));
        } else {
            let mask = (1 << self.def.bits) - 1;
            self.timecode = self.def.rev(self.timecode);
            self.bitstream = ((self.bitstream << 1) & mask) + (b as u32);
        }

        if self.timecode == self.bitstream {
            self.valid_counter += 1;
        } else {
            self.timecode = self.bitstream;
            self.valid_counter = 0;
        }

        self.timecode_ticker = 0;
        self.ref_level -= self.ref_level / REF_PEAKS_AVG;
        self.ref_level += m / REF_PEAKS_AVG;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Read;

    #[test]
    fn test_serato_cd() -> Result<(), Box<dyn std::error::Error>> {
        let def = TimecodeDef::serato_cd();
        let mut tc = Timecoder::new(def, 1.0, 44100, false);

        let mut file = File::open("testdata/serato_cd_1min.raw")?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;

        let samples: Vec<i16> = buffer
            .chunks_exact(2)
            .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();

        tc.submit(&samples);

        if let Some((pos, _when)) = tc.get_position() {
            assert!(pos >= 0);
        } else {
            panic!("No valid position");
        }

        Ok(())
    }
}
