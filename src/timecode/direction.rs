/// Pitch below which the record counts as running backwards.
const REVERSE_PITCH: f64 = -0.5;
/// Seconds of sustained reverse playback before it is reported.
const TRIGGER_SECS: f32 = 3.0;
/// The report clears once the accumulated time falls below this.
const CLEAR_SECS: f32 = 0.5;

/// Detects a timecode that keeps running backwards at roughly normal speed,
/// which is what swapped left/right input channels look like. Short reverse
/// moves (scratching, a quick rewind) never reach the trigger time.
///
/// The decoder reports the reversed pitch even when the swapped signal never
/// yields a valid position, so only the pitch is used. Without a carrier the
/// pitch is 0, so silence cannot trigger it.
#[derive(Debug, Default)]
pub struct ReverseDetector {
    secs: f32,
    detected: bool,
}

impl ReverseDetector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one UI frame. Returns whether sustained reverse playback is detected.
    pub fn update(&mut self, vinyl_pitch: f64, dt: f32) -> bool {
        let reversed = vinyl_pitch < REVERSE_PITCH;
        if reversed {
            self.secs += dt;
        } else {
            // Drain faster than it fills so a glitch can't build up a false alarm
            self.secs = (self.secs - 2.0 * dt).max(0.0);
        }
        if self.secs >= TRIGGER_SECS {
            self.detected = true;
        } else if self.secs <= CLEAR_SECS {
            self.detected = false;
        }
        self.detected
    }

    pub fn detected(&self) -> bool {
        self.detected
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(det: &mut ReverseDetector, pitch: f64, secs: f32) -> bool {
        let mut out = det.detected();
        for _ in 0..(secs * 60.0) as usize {
            out = det.update(pitch, 1.0 / 60.0);
        }
        out
    }

    #[test]
    fn sustained_reverse_is_detected() {
        let mut det = ReverseDetector::new();
        assert!(!run(&mut det, -1.0, 2.0));
        assert!(run(&mut det, -1.0, 2.0));
    }

    #[test]
    fn forward_and_short_reverse_are_ignored() {
        let mut det = ReverseDetector::new();
        assert!(!run(&mut det, 1.0, 10.0));
        // A scratch: 1s backwards, 1s forwards, repeated
        for _ in 0..10 {
            assert!(!run(&mut det, -1.0, 1.0));
            assert!(!run(&mut det, 1.0, 1.0));
        }
    }

    #[test]
    fn silence_is_ignored() {
        let mut det = ReverseDetector::new();
        assert!(!run(&mut det, 0.0, 10.0));
    }

    #[test]
    fn clears_after_direction_recovers() {
        let mut det = ReverseDetector::new();
        assert!(run(&mut det, -1.0, 4.0));
        assert!(!run(&mut det, 1.0, 3.0));
    }
}
