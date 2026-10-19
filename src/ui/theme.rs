use egui::Color32;

/// Selectable accent colors: (name, color). The first entry is the default.
pub const ACCENT_PRESETS: &[(&str, Color32)] = &[
    ("Amber", Color32::from_rgb(240, 180, 60)),
    ("Red", Color32::from_rgb(235, 70, 60)),
    ("Green", Color32::from_rgb(80, 200, 90)),
    ("Cyan", Color32::from_rgb(60, 200, 220)),
    ("Blue", Color32::from_rgb(80, 130, 240)),
    ("Purple", Color32::from_rgb(170, 100, 230)),
    ("Pink", Color32::from_rgb(235, 100, 170)),
    ("White", Color32::from_rgb(225, 225, 225)),
];

/// Opacity (0-255) of the black layer that dims the whole screen to
/// `brightness` percent. 100% (or more) gives 0 = no dimming; the layer never
/// gets fully opaque, so the screen stays readable.
pub fn dim_alpha(brightness: u8) -> u8 {
    let brightness = brightness.clamp(10, 100) as u32;
    ((100 - brightness) * 255 / 100) as u8
}

/// The single user-selected color all highlights are derived from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Accent(Color32);

impl Default for Accent {
    fn default() -> Self {
        Self(ACCENT_PRESETS[0].1)
    }
}

impl Accent {
    pub fn from_preset(index: usize) -> Self {
        Self(ACCENT_PRESETS[index % ACCENT_PRESETS.len()].1)
    }

    pub fn base(&self) -> Color32 {
        self.0
    }

    /// Lighter shade, for the focused / current item.
    pub fn bright(&self) -> Color32 {
        mix(self.0, Color32::WHITE, 0.4)
    }

    /// Almost white with a hint of accent, for selected plain items.
    pub fn light(&self) -> Color32 {
        mix(self.0, Color32::WHITE, 0.65)
    }

    pub fn mid(&self) -> Color32 {
        scale(self.0, 0.85)
    }

    pub fn dim(&self) -> Color32 {
        scale(self.0, 0.72)
    }

    /// Subdued shade, for elements that should stay out of the way.
    pub fn dark(&self) -> Color32 {
        scale(self.0, 0.45)
    }

    /// Very dark shade, for button and inactive backgrounds.
    pub fn tint(&self) -> Color32 {
        scale(self.0, 0.14)
    }

    /// The accent at `factor` of its brightness (0.0 black .. 1.0 the accent itself).
    pub fn shade(&self, factor: f32) -> Color32 {
        scale(self.0, factor)
    }
}

fn scale(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t) as u8;
    Color32::from_rgb(lerp(a.r(), b.r()), lerp(a.g(), b.g()), lerp(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::dim_alpha;

    #[test]
    fn full_brightness_does_not_dim() {
        assert_eq!(dim_alpha(100), 0);
        assert_eq!(dim_alpha(200), 0);
    }

    #[test]
    fn dimming_grows_as_brightness_drops_but_never_blacks_out() {
        assert!(dim_alpha(50) > dim_alpha(75));
        assert!(dim_alpha(10) > dim_alpha(50));
        assert!(dim_alpha(10) < 255);
        // Out-of-range values from a hand-edited file are held at the minimum
        assert_eq!(dim_alpha(0), dim_alpha(10));
    }
}
