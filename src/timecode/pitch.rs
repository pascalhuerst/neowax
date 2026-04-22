pub(crate) const ALPHA: f64 = 1.0 / 512.0;
pub(crate) const BETA: f64 = ALPHA / 256.0;

#[derive(Debug)]
pub struct Pitch {
    dt: f64,
    x: f64,
    v: f64,
}

impl Pitch {
    pub fn new(dt: f64) -> Self {
        Self { dt, x: 0.0, v: 0.0 }
    }

    pub fn current(&self) -> f64 {
        self.v
    }

    pub fn dt_observation(&mut self, dx: f64) {
        let predicted_x = self.x + self.v * self.dt;
        let predicted_v = self.v;

        let residual_x = dx - predicted_x;

        self.x = predicted_x + residual_x * ALPHA;
        self.v = predicted_v + residual_x * BETA / self.dt;

        self.x -= dx; // relative to previous
    }

    pub fn reset(&mut self) {
        self.x = 0.0;
        self.v = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_converges_to_constant_speed() {
        let mut pitch = Pitch::new(1.0);

        // Feed constant dx=1.0 (speed = 1.0) for many iterations
        for _ in 0..5000 {
            pitch.dt_observation(1.0);
        }

        let v = pitch.current();
        assert!(
            (v - 1.0).abs() < 0.01,
            "should converge to speed 1.0, got {}",
            v
        );
    }

    #[test]
    fn test_converges_to_different_speed() {
        let mut pitch = Pitch::new(1.0);

        for _ in 0..5000 {
            pitch.dt_observation(2.5);
        }

        let v = pitch.current();
        assert!(
            (v - 2.5).abs() < 0.05,
            "should converge to speed 2.5, got {}",
            v
        );
    }

    #[test]
    fn test_reset_clears_state() {
        let mut pitch = Pitch::new(1.0);

        for _ in 0..1000 {
            pitch.dt_observation(1.0);
        }
        assert!(pitch.current() > 0.5);

        pitch.reset();
        assert_eq!(pitch.current(), 0.0);
    }
}
