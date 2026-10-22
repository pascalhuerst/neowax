use super::Accent;
use crate::audio::{WaveformData, FULL, HIGH, LOW, MID};
use egui::{epaint::Mesh, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use std::time::Duration;

const CURSOR_COLOR: Color32 = Color32::WHITE;
const BACKGROUND_COLOR: Color32 = Color32::from_rgb(12, 12, 18);
const GRIDLINE_COLOR: Color32 = Color32::from_rgb(30, 30, 40);

/// Share of the half height the waveform may fill, leaving a little air.
const MAX_FILL: f32 = 0.95;
/// Smoothing window of the overview: longer than a beat, so the beat doesn't
/// show up as stripes and what remains is the structure (breakdowns, drops).
const OVERVIEW_SMOOTH_SECS: f64 = 1.2;
/// Contrast curve of the overview band layers (above 1 favours the loud parts).
const OVERVIEW_GAMMA: f32 = 1.8;
/// Contrast curve of the close-up (below 1 lifts quieter detail slightly).
const ZOOM_GAMMA: f32 = 0.9;
/// How much taller the bright RMS core is drawn than the raw RMS.
const CORE_GAIN: f32 = 1.5;
/// Height of the overview strip above the close-up, as a share of the total.
const STRIP_SHARE: f32 = 0.3;
const STRIP_MIN: f32 = 36.0;
const STRIP_MAX: f32 = 80.0;
const STRIP_GAP: f32 = 3.0;

/// How the track is shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WaveformMode {
    /// The whole track across the full width.
    Overview,
    /// A small overview strip on top, and below it a close-up of
    /// `window_secs` seconds that scrolls past a fixed playhead in the middle.
    Zoomed { window_secs: f32 },
}

pub struct WaveformWidget {
    data: Option<WaveformData>,
    position: Duration,
    accent: Accent,
}

impl Default for WaveformWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl WaveformWidget {
    pub fn new() -> Self {
        Self {
            data: None,
            position: Duration::ZERO,
            accent: Accent::default(),
        }
    }

    pub fn set_accent(&mut self, accent: Accent) {
        self.accent = accent;
    }

    pub fn set_data(&mut self, data: WaveformData) {
        self.data = Some(data);
        self.position = Duration::ZERO;
    }

    pub fn clear(&mut self) {
        self.data = None;
        self.position = Duration::ZERO;
    }

    pub fn has_data(&self) -> bool {
        self.data.is_some()
    }

    pub fn set_position(&mut self, position: Duration) {
        self.position = position;
    }

    /// Draw the waveform, `height` pixels tall and as wide as the available space.
    pub fn show(&mut self, ui: &mut Ui, height: f32, mode: WaveformMode) -> Response {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
        let painter = ui.painter();

        match mode {
            WaveformMode::Overview => {
                self.draw_overview(painter, rect);
                self.draw_overview_cursor(painter, rect, None);
            }
            WaveformMode::Zoomed { window_secs } => {
                let strip = (height * STRIP_SHARE).clamp(STRIP_MIN, STRIP_MAX);
                let strip_rect = Rect::from_min_size(rect.min, Vec2::new(rect.width(), strip));
                let zoom_rect = Rect::from_min_max(
                    Pos2::new(rect.left(), strip_rect.bottom() + STRIP_GAP),
                    rect.max,
                );
                self.draw_overview(painter, strip_rect);
                self.draw_overview_cursor(painter, strip_rect, Some(window_secs as f64));
                self.draw_zoom(painter, zoom_rect, window_secs as f64);
            }
        }
        response
    }

    fn draw_background(painter: &egui::Painter, rect: Rect) {
        painter.rect_filled(rect, 0.0, BACKGROUND_COLOR);
        painter.line_segment(
            [
                Pos2::new(rect.left(), rect.center().y),
                Pos2::new(rect.right(), rect.center().y),
            ],
            Stroke::new(1.0_f32, GRIDLINE_COLOR),
        );
    }

    /// The playhead on the overview, plus (when zoomed) the span the close-up shows.
    fn draw_overview_cursor(&self, painter: &egui::Painter, rect: Rect, window: Option<f64>) {
        let Some(data) = &self.data else {
            return;
        };
        let duration = data.duration_secs();
        if duration <= 0.0 {
            return;
        }
        let at = |secs: f64| rect.left() + rect.width() * (secs / duration).clamp(0.0, 1.0) as f32;
        let now = self.position.as_secs_f64();

        if let Some(window) = window {
            let span = Rect::from_x_y_ranges(
                at(now - window / 2.0)..=at(now + window / 2.0),
                rect.y_range(),
            );
            painter.rect_filled(span, 0.0, Color32::from_white_alpha(22));
        }
        let x = at(now);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.5_f32, CURSOR_COLOR),
        );
    }

    /// The whole track: three smoothed band layers (bass darkest and behind,
    /// highs lightest), all in shades of the accent color.
    fn draw_overview(&self, painter: &egui::Painter, rect: Rect) {
        Self::draw_background(painter, rect);
        let Some(data) = &self.data else {
            return;
        };
        let (hops, hop_secs) = (data.hops(), data.hop_secs());
        let columns = rect.width() as usize;
        if hops == 0 || columns == 0 {
            return;
        }
        let duration = data.duration_secs();
        let smooth = OVERVIEW_SMOOTH_SECS.max(duration / columns as f64);
        let refs = data.overview_ref();
        let colors = [self.accent.dark(), self.accent.base(), self.accent.light()];

        let mut mesh = Mesh::default();
        let center_y = rect.center().y;
        let half = rect.height() / 2.0 * MAX_FILL;
        for x in 0..columns {
            let t = (x as f64 + 0.5) / columns as f64 * duration;
            let a = ((t - smooth / 2.0) / hop_secs).floor().max(0.0) as usize;
            let b = (((t + smooth / 2.0) / hop_secs).ceil() as usize).clamp(a + 1, hops);
            let mut layers = [LOW, MID, HIGH].map(|band| {
                let level = (data.rms(band, a, b) / refs[band]).clamp(0.0, 1.0);
                (level.powf(OVERVIEW_GAMMA), colors[band])
            });
            // Tallest first, so the smaller layers stay visible in front of it
            layers.sort_by(|p, q| q.0.total_cmp(&p.0));
            let left = rect.left() + x as f32;
            for (level, color) in layers {
                add_quad(
                    &mut mesh,
                    left,
                    center_y - level * half,
                    center_y + level * half,
                    color,
                );
            }
        }
        painter.add(Shape::mesh(mesh));
    }

    /// A close-up of `window_secs` around the playhead. Columns sit on a fixed
    /// time grid and the whole picture slides past the centre line, so it
    /// scrolls smoothly instead of shimmering. Each column shows the true
    /// peaks in a shade that follows the bass/treble balance, with the RMS
    /// as a brighter core.
    fn draw_zoom(&self, painter: &egui::Painter, rect: Rect, window_secs: f64) {
        Self::draw_background(painter, rect);
        let Some(data) = &self.data else {
            return;
        };
        let (hops, hop_secs) = (data.hops() as isize, data.hop_secs());
        let columns = rect.width() as usize;
        if hops == 0 || columns == 0 {
            return;
        }
        // Hops per column: the window, rounded to a whole number of hops
        let per_col = ((window_secs / columns as f64 / hop_secs).round() as isize).max(1);
        let playhead = self.position.as_secs_f64() / hop_secs; // in hops
        let first = (playhead / per_col as f64 - columns as f64 / 2.0).floor() as isize;
        let peak_ref = data.peak_ref();

        let mut mesh = Mesh::default();
        let center = rect.center();
        let half = rect.height() / 2.0 * MAX_FILL;
        let level = |v: f32| (v / peak_ref).clamp(0.0, 1.0).powf(ZOOM_GAMMA);
        for i in 0..=(columns + 1) as isize {
            let a = (first + i) * per_col;
            let b = a + per_col;
            if b <= 0 || a >= hops {
                continue;
            }
            let (a, b) = (a.max(0) as usize, b.min(hops) as usize);
            let x = center.x + (((first + i) * per_col) as f64 - playhead) as f32 / per_col as f32;
            if x < rect.left() - 1.0 || x > rect.right() {
                continue;
            }

            let (lowest, highest) = data.min_max(a, b);
            let rms = data.rms(FULL, a, b);
            let (low, mid, high) = (
                data.mean_square(LOW, a, b),
                data.mean_square(MID, a, b),
                data.mean_square(HIGH, a, b),
            );
            // 0 = all bass .. 1 = all treble
            let balance = ((mid * 0.5 + high) / (low + mid + high).max(1e-12)) as f32;
            let outer = self.accent.shade(0.5 + 0.5 * balance);

            add_quad(
                &mut mesh,
                x,
                center.y - level(highest) * half,
                center.y + level(-lowest) * half,
                outer,
            );
            let core = level(rms * CORE_GAIN) * half;
            add_quad(
                &mut mesh,
                x,
                center.y - core,
                center.y + core,
                self.accent.bright(),
            );
        }
        painter.add(Shape::mesh(mesh));

        painter.line_segment(
            [
                Pos2::new(center.x, rect.top()),
                Pos2::new(center.x, rect.bottom()),
            ],
            Stroke::new(1.5_f32, CURSOR_COLOR),
        );
    }
}

/// A one pixel wide solid quad from `top` to `bottom` at `left`.
fn add_quad(mesh: &mut Mesh, left: f32, top: f32, bottom: f32, color: Color32) {
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(Pos2::new(left, top), color);
    mesh.colored_vertex(Pos2::new(left, bottom), color);
    mesh.colored_vertex(Pos2::new(left + 1.0, top), color);
    mesh.colored_vertex(Pos2::new(left + 1.0, bottom), color);
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base + 1, base + 3, base + 2);
}
