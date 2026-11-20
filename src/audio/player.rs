use super::track::Track;
use crate::timecode::Timecoder;
use std::sync::Arc;

/// Snapshot of timecoder state needed by the player for one period.
/// Decouples the player from the Timecoder so they can live on different threads.
pub struct TimecodeState {
    pub pitch: f64,
    pub position: Option<(i32, f64)>,
    pub safe: u32,
    pub resolution: f64,
}

impl TimecodeState {
    pub fn from_timecoder(tc: &Timecoder) -> Self {
        Self {
            pitch: tc.get_pitch(),
            position: tc.get_position(),
            safe: tc.get_safe(),
            resolution: tc.get_resolution(),
        }
    }
}

const PLAYER_CHANNELS: usize = 2;

/// Time taken to reach sync with timecode position
const SYNC_TIME: f64 = 0.5;
/// Don't sync at pitches below this
const SYNC_PITCH: f64 = 0.05;
/// Time constant: sync_pitch decays toward 1.0 when no timecodes available
const SYNC_RC: f64 = 0.05;
/// Jump straight to timecode position if difference exceeds this (seconds)
const SKIP_THRESHOLD: f64 = 1.0 / 8.0;
/// Timecode positions (seconds) this close to the start of the record count as its
/// beginning. A hand can't place the needle to within a few seconds of groove
/// (1 s of timecode is about 0.1 mm), and the first position the decoder can
/// read is already seconds in.
const RECORD_START_ZONE: f64 = 20.0;
/// No timecode for this long means the needle was lifted and put down again
const NEEDLE_LIFT_GAP: f64 = 0.5;
/// A shorter gap still counts when the needle comes back *behind* the track start
/// (slid back through the uncoded groove before the first position)
const SLID_BACK_GAP: f64 = 0.2;
/// Base volume level (leaves headroom for pitch > 1.0)
const VOLUME: f64 = 7.0 / 8.0;

/// LFSR-based dither, returns value between -0.5 and 0.5
fn dither() -> f64 {
    thread_local! {
        static STATE: std::cell::Cell<u32> = const { std::cell::Cell::new(0xbeef_face) };
    }
    STATE.with(|s| {
        let mut x = s.get();
        let bit = (x ^ (x >> 1) ^ (x >> 21) ^ (x >> 31)) & 1;
        x = (x << 1) | bit;
        s.set(x);
        let v = (x & 0x0000_000f) | ((x & 0x000f_0000) >> 12) | ((x & 0x0f00_0000) >> 16);
        v as f64 / 4096.0 - 0.5
    })
}

/// Cubic interpolation of 4 samples at fractional position mu (0..1).
/// Returns the interpolated value between y[1] and y[2].
#[inline]
fn cubic_interpolate(y: [i16; 4], mu: f64) -> f64 {
    let mu2 = mu * mu;
    let a0 = y[3] as i64 - y[2] as i64 - y[0] as i64 + y[1] as i64;
    let a1 = y[0] as i64 - y[1] as i64 - a0;
    let a2 = y[2] as i64 - y[0] as i64;
    let a3 = y[1] as i64;
    mu * mu2 * a0 as f64 + mu2 * a1 as f64 + mu * a2 as f64 + a3 as f64
}

pub struct Player {
    sample_dt: f64,
    track: Option<Arc<Track>>,

    /// Current position in timecode seconds
    pub position: f64,
    /// Target position from timecoder, or INFINITY if unknown
    target_position: f64,
    /// Offset: track start point in timecode coordinates
    pub offset: f64,
    /// Last known position minus target_position (for UI display)
    pub last_difference: f64,
    /// Current pitch from timecoder
    pub pitch: f64,
    /// Pitch compensation to sync with timecode position
    sync_pitch: f64,
    /// Current volume level
    volume: f64,
    /// Calibrate offset on next valid timecode position
    needs_calibration: bool,
    /// How long the timecode has been missing (seconds)
    no_position_secs: f64,
    /// Play the track at normal speed from the start, ignoring the timecode
    free_run: bool,
}

impl Player {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            sample_dt: 1.0 / sample_rate as f64,
            track: None,
            position: 0.0,
            target_position: f64::INFINITY,
            offset: 0.0,
            last_difference: 0.0,
            pitch: 0.0,
            sync_pitch: 1.0,
            volume: 0.0,
            needs_calibration: true,
            no_position_secs: 0.0,
            free_run: false,
        }
    }

    /// Switch between timecode control and free-running playback. A change
    /// restarts from a clean state; the loaded track is kept.
    pub fn set_free_run(&mut self, free_run: bool) {
        if self.free_run == free_run {
            return;
        }
        self.free_run = free_run;
        self.position = 0.0;
        self.offset = 0.0;
        self.pitch = 0.0;
        self.sync_pitch = 1.0;
        self.target_position = f64::INFINITY;
        self.needs_calibration = true;
    }

    pub fn set_track(&mut self, track: Arc<Track>) {
        self.track = Some(track);
        self.position = 0.0;
        self.sync_pitch = 1.0;
        // Calibrate on first valid position while vinyl is moving.
        // This maps the needle's current position to track start,
        // accounting for the physical lead-in on the vinyl.
        self.needs_calibration = true;
    }

    pub fn clear_track(&mut self) {
        self.track = None;
        self.position = 0.0;
        self.offset = 0.0;
        self.volume = 0.0;
        self.needs_calibration = true;
    }

    pub fn get_elapsed(&self) -> f64 {
        self.position - self.offset
    }

    /// Effective pitch: raw timecoder pitch * sync correction.
    /// This is the actual rate at which elapsed advances.
    pub fn get_effective_pitch(&self) -> f64 {
        self.pitch * self.sync_pitch
    }

    pub fn is_active(&self) -> bool {
        self.pitch.abs() > 0.01
    }

    /// Synchronise to the position and speed given by a timecode state snapshot.
    fn sync_to_state(&mut self, state: &TimecodeState) {
        self.pitch = state.pitch;

        match state.position {
            Some((pos, when)) => {
                if pos < 0 || pos as u32 > state.safe {
                    self.target_position = f64::INFINITY;
                } else {
                    let tcpos = pos as f64 / state.resolution;
                    self.target_position = tcpos + self.pitch * when;
                }
            }
            None => {
                self.target_position = f64::INFINITY;
            }
        }
    }

    /// The needle came down (again) in the first seconds of the record: the track
    /// starts from its beginning right where it came down.
    ///
    /// Without this, a start calibrated further in leaves the track time negative
    /// (silence, and no sign of the track coming back), and anything fixed to the
    /// record start would miss the track's beginning, since the first position the
    /// decoder can read is already seconds in, and a hand can't hit an exact spot.
    ///
    /// Only after the needle was lifted (a gap in the timecode), or slid back behind
    /// the track start through the uncoded groove. A dropout of dust length, or a
    /// needle resting on its cue, changes nothing, and neither does dropping further
    /// in, where running up to a cue from before it is intended.
    fn check_record_start(&mut self) {
        if self.needs_calibration || self.target_position >= RECORD_START_ZONE {
            return;
        }
        let lifted = self.no_position_secs >= NEEDLE_LIFT_GAP;
        let slid_back =
            self.no_position_secs >= SLID_BACK_GAP && self.target_position < self.offset;
        if lifted || slid_back {
            tracing::info!(
                "Needle down at {:.2}s in the record's start zone: track starts here (was {:.2}s)",
                self.target_position,
                self.offset
            );
            self.offset = self.target_position;
        }
    }

    /// Adjust playback to converge on the timecode target position
    fn retarget(&mut self) {
        if self.needs_calibration {
            // Only calibrate when vinyl is actually moving, so holding the
            // record still during track load doesn't set a wrong offset.
            if self.pitch.abs() < SYNC_PITCH {
                return;
            }
            self.offset = self.target_position;
            self.position = self.target_position;
            self.needs_calibration = false;
            tracing::info!("Calibrated: timecode offset = {:.2}s", self.offset);
            return;
        }

        let diff = self.position - self.target_position;
        self.last_difference = diff;

        if diff.abs() > SKIP_THRESHOLD {
            // Large deviation (e.g. needle repositioned): jump directly
            self.position = self.target_position;
            tracing::debug!(
                "Seek to {:.2}s (elapsed {:.2}s)",
                self.position,
                self.position - self.offset
            );
        } else if self.pitch.abs() > SYNC_PITCH {
            // Small deviation: bend pitch to compensate
            let denom = diff / SYNC_TIME + self.pitch;
            if denom.abs() > 1e-6 {
                self.sync_pitch = (self.pitch / denom).clamp(0.5, 2.0);
            }
        }
    }

    /// Build a block of PCM audio, resampled from the track using cubic interpolation.
    /// Returns the number of seconds advanced in the source track.
    #[allow(clippy::too_many_arguments)]
    fn build_pcm(
        &self,
        pcm: &mut [i16],
        samples: usize,
        track: &Track,
        position: f64,
        pitch: f64,
        start_vol: f64,
        end_vol: f64,
    ) -> f64 {
        let mut sample = position * track.rate as f64;
        let step = self.sample_dt * pitch * track.rate as f64;
        let mut vol = start_vol;
        let gradient = (end_vol - start_vol) / samples as f64;

        for s in 0..samples {
            // Floor for the 4-sample cubic interpolation window
            let mut sa = sample as isize;
            if sample < 0.0 {
                sa -= 1;
            }
            let f = sample - sa as f64;
            sa -= 1;

            for c in 0..PLAYER_CHANNELS {
                let interp = [
                    track.get_sample(sa)[c],
                    track.get_sample(sa + 1)[c],
                    track.get_sample(sa + 2)[c],
                    track.get_sample(sa + 3)[c],
                ];

                let v = vol * cubic_interpolate(interp, f) + dither();
                pcm[s * PLAYER_CHANNELS + c] = v.clamp(i16::MIN as f64, i16::MAX as f64) as i16;
            }

            sample += step;
            vol += gradient;
        }

        self.sample_dt * pitch * samples as f64
    }

    /// Get a block of PCM audio using a pre-built timecode state snapshot.
    /// Used by the playback thread in the two-thread engine.
    pub fn collect_with_state(&mut self, pcm: &mut [i16], samples: usize, state: &TimecodeState) {
        if self.free_run {
            self.collect_free_run(pcm, samples);
            return;
        }

        let dt = self.sample_dt * samples as f64;

        self.sync_to_state(state);

        if self.target_position.is_finite() {
            self.check_record_start();
            self.retarget();
            self.target_position = f64::INFINITY;
            self.no_position_secs = 0.0;
        } else {
            self.no_position_secs += dt;
            self.sync_pitch += dt / (SYNC_RC + dt) * (1.0 - self.sync_pitch);
        }

        let target_volume = (self.pitch.abs() * VOLUME).min(1.0);
        let pitch = self.pitch * self.sync_pitch;

        let r = match &self.track {
            Some(track) => self.build_pcm(
                pcm,
                samples,
                track,
                self.position - self.offset,
                pitch,
                self.volume,
                target_volume,
            ),
            None => {
                pcm[..samples * PLAYER_CHANNELS].fill(0);
                self.sample_dt * pitch * samples as f64
            }
        };

        self.position += r;
        self.volume = target_volume;
    }

    /// Play the track from the start at normal speed, stopping at its end.
    /// A newly set track starts right away.
    fn collect_free_run(&mut self, pcm: &mut [i16], samples: usize) {
        let end = self.track.as_ref().map(|t| t.length as f64 / t.rate as f64);
        let playing = end.is_some_and(|end| self.position < end);

        self.pitch = if playing { 1.0 } else { 0.0 };
        self.sync_pitch = 1.0;
        let target_volume = if playing { VOLUME } else { 0.0 };

        match &self.track {
            Some(track) => {
                let advanced = self.build_pcm(
                    pcm,
                    samples,
                    track,
                    self.position,
                    self.pitch,
                    self.volume,
                    target_volume,
                );
                self.position += advanced;
            }
            None => pcm[..samples * PLAYER_CHANNELS].fill(0),
        }
        if let Some(end) = end {
            self.position = self.position.min(end);
        }
        self.volume = target_volume;
    }

    /// Get a block of PCM audio data to send to the soundcard.
    /// Convenience wrapper that reads state directly from a Timecoder reference.
    pub fn collect(&mut self, pcm: &mut [i16], samples: usize, timecoder: &Timecoder) {
        let state = TimecodeState::from_timecoder(timecoder);
        self.collect_with_state(pcm, samples, &state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timecode::{TimecodeDef, Timecoder};
    use std::fs::File;
    use std::io::Read;

    /// Create a simple synthetic track: 10 seconds of a 440Hz sine wave at 44100Hz stereo
    fn make_test_track() -> Arc<Track> {
        let rate = 44100u32;
        let length = rate as usize * 10;
        let mut pcm = Vec::with_capacity(length * 2);
        for i in 0..length {
            let t = i as f64 / rate as f64;
            let v = (t * 440.0 * 2.0 * std::f64::consts::PI).sin();
            let s = (v * i16::MAX as f64) as i16;
            pcm.push(s); // left
            pcm.push(s); // right
        }
        Track::from_pcm(pcm, rate)
    }

    fn idle_timecode_state() -> TimecodeState {
        TimecodeState {
            pitch: 0.0,
            position: None,
            safe: 0,
            resolution: 1000.0,
        }
    }

    #[test]
    fn free_run_plays_from_the_start_and_stops_at_the_end() {
        let mut player = Player::new(44100);
        player.set_free_run(true);
        player.set_track(make_test_track()); // 10 s

        let period = 441; // 10 ms
        let mut buf = vec![0i16; period * 2];
        let state = idle_timecode_state();

        // The first second plays right away, with the timecode idle
        let mut peak = 0i16;
        for _ in 0..100 {
            player.collect_with_state(&mut buf, period, &state);
            peak = peak.max(buf.iter().copied().max().unwrap_or(0));
        }
        assert!((player.get_elapsed() - 1.0).abs() < 0.02);
        assert!((player.get_effective_pitch() - 1.0).abs() < 1e-9);
        assert!(peak > 10_000, "expected audible output, peak {peak}");

        // Run past the end: it stops and holds the end position
        for _ in 0..1100 {
            player.collect_with_state(&mut buf, period, &state);
        }
        assert!((player.get_elapsed() - 10.0).abs() < 0.02);
        assert_eq!(player.get_effective_pitch(), 0.0);
    }

    #[test]
    fn free_run_replaces_the_track_and_restarts() {
        let mut player = Player::new(44100);
        player.set_free_run(true);
        player.set_track(make_test_track());

        let period = 441;
        let mut buf = vec![0i16; period * 2];
        let state = idle_timecode_state();
        for _ in 0..300 {
            player.collect_with_state(&mut buf, period, &state);
        }
        assert!(player.get_elapsed() > 2.9);

        // A new track starts from zero immediately
        player.set_track(make_test_track());
        player.collect_with_state(&mut buf, period, &state);
        assert!(player.get_elapsed() < 0.02);
        assert!(player.get_effective_pitch() > 0.9);
    }

    /// A timecode snapshot `secs` into the record, the vinyl moving at normal speed.
    fn moving_at(secs: f64) -> TimecodeState {
        TimecodeState {
            pitch: 1.0,
            position: Some(((secs * 1000.0) as i32, 0.0)),
            safe: u32::MAX,
            resolution: 1000.0,
        }
    }

    fn needle_lifted() -> TimecodeState {
        TimecodeState {
            pitch: 0.0,
            position: None,
            safe: u32::MAX,
            resolution: 1000.0,
        }
    }

    /// Run the player for `secs` (10 ms periods), the needle moving on from record position `from`.
    /// Returns whether anything audible came out.
    fn play_from(player: &mut Player, from: f64, secs: f64) -> bool {
        let period = 441;
        let mut buf = vec![0i16; period * 2];
        let mut audible = false;
        for i in 0..(secs * 100.0) as usize {
            player.collect_with_state(&mut buf, period, &moving_at(from + i as f64 * 0.01));
            audible |= buf.iter().any(|&s| s.abs() > 1000);
        }
        audible
    }

    fn lift_for(player: &mut Player, secs: f64) {
        let mut buf = vec![0i16; 441 * 2];
        for _ in 0..(secs * 100.0) as usize {
            player.collect_with_state(&mut buf, 441, &needle_lifted());
        }
    }

    /// A player whose track was loaded with the needle `record_secs` into the record.
    fn loaded_at(record_secs: f64) -> Player {
        let mut player = Player::new(44100);
        player.set_track(make_test_track());
        play_from(&mut player, record_secs, 0.5);
        player
    }

    #[test]
    fn needle_back_at_the_start_of_the_record_restarts_the_track() {
        // Track start calibrated 30 s into the record
        let mut player = loaded_at(30.0);
        assert!((player.offset - 30.0).abs() < 0.05);

        // Lift the needle and put it down near the beginning of the record
        lift_for(&mut player, 1.0);
        let audible = play_from(&mut player, 4.0, 1.0);

        assert!(audible, "the track must play, not sit in silence");
        assert!(
            (player.offset - 4.0).abs() < 0.05,
            "starts where it came down"
        );
        // One second played since it came down
        assert!(
            (player.get_elapsed() - 1.0).abs() < 0.1,
            "{}",
            player.get_elapsed()
        );
    }

    #[test]
    fn the_track_starts_where_the_needle_came_down_even_when_that_is_later() {
        // First calibrated at 4.0 s; put down again at 6.5 s, which is after that
        // spot. A hand can't land before the first readable position, but the
        // track must still start from its beginning.
        let mut player = loaded_at(4.0);
        lift_for(&mut player, 1.0);
        assert!(play_from(&mut player, 6.5, 1.0));
        assert!((player.offset - 6.5).abs() < 0.05, "{}", player.offset);
        assert!(
            (player.get_elapsed() - 1.0).abs() < 0.1,
            "{}",
            player.get_elapsed()
        );
    }

    #[test]
    fn sliding_back_behind_the_start_through_the_uncoded_groove_restarts_the_track() {
        let mut player = loaded_at(30.0);
        lift_for(&mut player, 0.3); // no timecode for a moment, not a full lift
        assert!(play_from(&mut player, 5.0, 1.0));
        assert!((player.offset - 5.0).abs() < 0.05, "{}", player.offset);
    }

    #[test]
    fn a_dropout_after_the_start_does_not_restart_a_playing_track() {
        // Playing inside the start zone, then a 0.3 s dropout: not a lifted
        // needle, so the track carries on where it was
        let mut player = loaded_at(8.0);
        play_from(&mut player, 8.5, 4.0);
        lift_for(&mut player, 0.3);
        play_from(&mut player, 13.0, 0.5);
        assert!(
            (player.offset - 8.0).abs() < 0.05,
            "offset {}",
            player.offset
        );
    }

    #[test]
    fn dropping_further_in_just_seeks_and_keeps_the_start() {
        let mut player = loaded_at(30.0);
        lift_for(&mut player, 1.0);
        play_from(&mut player, 36.0, 1.0);

        assert!((player.offset - 30.0).abs() < 0.05);
        assert!(
            (player.get_elapsed() - 7.0).abs() < 0.2,
            "{}",
            player.get_elapsed()
        );
    }

    #[test]
    fn dropping_before_the_start_outside_the_start_zone_keeps_the_cue() {
        // Running up to the cue from before it is intended: silence until it is reached
        let mut player = loaded_at(30.0);
        lift_for(&mut player, 1.0);
        let audible = play_from(&mut player, 25.0, 1.0);

        assert!((player.offset - 30.0).abs() < 0.05);
        assert!(!audible);
        assert!(player.get_elapsed() < 0.0);
    }

    #[test]
    fn a_short_dropout_in_the_start_zone_does_not_reset_anything() {
        let mut player = loaded_at(8.0);
        lift_for(&mut player, 0.1); // dust, not a needle placed down
        play_from(&mut player, 7.5, 0.5);
        assert!(
            (player.offset - 8.0).abs() < 0.05,
            "offset {}",
            player.offset
        );
    }

    #[test]
    fn free_run_with_no_track_is_silent() {
        let mut player = Player::new(44100);
        player.set_free_run(true);
        let mut buf = vec![1i16; 441 * 2];
        player.collect_with_state(&mut buf, 441, &idle_timecode_state());
        assert!(buf.iter().all(|&s| s == 0));
        assert_eq!(player.get_effective_pitch(), 0.0);
    }

    #[test]
    fn test_transport_with_timecode() -> Result<(), Box<dyn std::error::Error>> {
        // Load timecode test data
        let mut file = File::open("testdata/serato_cd_1min.raw")?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)?;
        let tc_samples: Vec<i16> = buffer
            .as_chunks::<2>()
            .0
            .iter()
            .map(|chunk| i16::from_le_bytes(*chunk))
            .collect();

        // Create timecoder at 44100Hz (matching the test data)
        let def = TimecodeDef::serato_cd();
        let mut timecoder = Timecoder::new(def, 1.0, 44100, false);

        // Create player at 44100Hz with a test track
        let track = make_test_track();
        let mut player = Player::new(44100);
        player.set_track(track);

        let period = 256;
        let mut playback_buf = vec![0i16; period * 2];
        let total_periods = tc_samples.len() / 2 / period; // stereo frames / period

        let mut got_position = false;
        let mut got_nonzero_pitch = false;
        let mut got_nonzero_output = false;

        for p in 0..total_periods {
            let start = p * period * 2;
            let end = start + period * 2;
            let tc_chunk = &tc_samples[start..end];

            // Feed timecode
            timecoder.submit(tc_chunk);

            // Generate output
            player.collect(&mut playback_buf, period, &timecoder);

            // Check that the player starts tracking
            if timecoder.get_position().is_some() {
                got_position = true;
            }
            if player.pitch.abs() > 0.01 {
                got_nonzero_pitch = true;
            }
            if playback_buf.iter().any(|&s| s != 0) {
                got_nonzero_output = true;
            }
        }

        assert!(
            got_position,
            "Timecoder should find valid positions in the test data"
        );
        assert!(
            got_nonzero_pitch,
            "Player should have non-zero pitch when timecode is playing"
        );
        assert!(
            got_nonzero_output,
            "Player should produce non-silent audio output"
        );

        // Player position should have advanced significantly (close to 60 seconds of timecode)
        let elapsed = player.get_elapsed();
        assert!(
            elapsed > 30.0,
            "Player should have advanced well into the track, got {:.1}s",
            elapsed
        );

        Ok(())
    }

    #[test]
    fn test_cubic_interpolate_boundaries() {
        // At mu=0, result should be y[1]
        assert_eq!(cubic_interpolate([0, 1000, 2000, 3000], 0.0), 1000.0);

        // At mu=1, result should be y[2]
        assert_eq!(cubic_interpolate([0, 1000, 2000, 3000], 1.0), 2000.0);

        // Linear ramp: midpoint should be 1500
        let mid = cubic_interpolate([0, 1000, 2000, 3000], 0.5);
        assert!(
            (mid - 1500.0).abs() < 1.0,
            "midpoint of linear ramp: {}",
            mid
        );

        // Constant signal: should stay constant at any mu
        assert_eq!(cubic_interpolate([500, 500, 500, 500], 0.25), 500.0);
        assert_eq!(cubic_interpolate([500, 500, 500, 500], 0.75), 500.0);
    }

    #[test]
    fn test_sync_pitch_clamp_near_zero_denominator() {
        // Simulate the edge case where diff/SYNC_TIME ≈ -pitch,
        // making the denominator approach zero.
        let mut player = Player::new(44100);
        let track = make_test_track();
        player.set_track(track);
        player.needs_calibration = false;

        // Set up: pitch = 0.1, diff that makes denominator ≈ 0
        // denom = diff/SYNC_TIME + pitch = diff/0.5 + 0.1
        // For denom ≈ 0: diff ≈ -0.05
        player.pitch = 0.1;
        player.position = 10.0;
        player.target_position = 10.05; // diff = -0.05
        player.retarget();

        // sync_pitch should be clamped, not inf/NaN
        assert!(player.sync_pitch.is_finite(), "sync_pitch should be finite");
        assert!(
            player.sync_pitch >= 0.5 && player.sync_pitch <= 2.0,
            "sync_pitch should be clamped, got {}",
            player.sync_pitch
        );
    }
}
