use super::Accent;
use egui::{epaint::Mesh, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use std::time::Duration;

const OVERVIEW_HEIGHT: f32 = 112.0;
const CURSOR_COLOR: Color32 = Color32::from_rgb(255, 255, 255);
const BACKGROUND_COLOR: Color32 = Color32::from_rgb(12, 12, 18);
const GRIDLINE_COLOR: Color32 = Color32::from_rgb(30, 30, 40);

// Frequency band colors, derived from the accent by brightness.
// Bass = darkest, Mid = accent, High = lightest
fn band_color(accent: Accent, low: f32, mid: f32, high: f32) -> (Color32, Color32) {
    let fill_low = accent.scaled_rgb(0.5);
    let fill_mid = accent.scaled_rgb(0.62);
    let fill_high = accent.scaled_rgb(0.7);
    let edge_low = accent.scaled_rgb(0.82);
    let edge_mid = accent.scaled_rgb(1.0);
    let edge_high = accent.bright_rgb();

    let total = low + mid + high;
    if total < 1e-6 {
        return (to_color(fill_mid), to_color(edge_mid));
    }
    let (wl, wm, wh) = (low / total, mid / total, high / total);
    let blend = |l: [f32; 3], m: [f32; 3], h: [f32; 3]| {
        to_color([
            l[0] * wl + m[0] * wm + h[0] * wh,
            l[1] * wl + m[1] * wm + h[1] * wh,
            l[2] * wl + m[2] * wm + h[2] * wh,
        ])
    };
    (
        blend(fill_low, fill_mid, fill_high),
        blend(edge_low, edge_mid, edge_high),
    )
}

fn to_color(rgb: [f32; 3]) -> Color32 {
    Color32::from_rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8)
}

/// Pre-computed min/max at power-of-2 block sizes for fast waveform drawing.
struct WaveformMipmap {
    levels: Vec<Vec<(f32, f32)>>,
}

impl WaveformMipmap {
    fn build(samples: &[f32]) -> Self {
        let mut levels = Vec::new();
        let mut prev: Vec<(f32, f32)> = samples
            .chunks(2)
            .map(|c| {
                let mut mn = c[0];
                let mut mx = c[0];
                for &s in &c[1..] {
                    mn = mn.min(s);
                    mx = mx.max(s);
                }
                (mn, mx)
            })
            .collect();
        levels.push(prev.clone());
        while prev.len() > 1 {
            prev = prev
                .chunks(2)
                .map(|c| {
                    if c.len() == 2 {
                        (c[0].0.min(c[1].0), c[0].1.max(c[1].1))
                    } else {
                        c[0]
                    }
                })
                .collect();
            levels.push(prev.clone());
        }
        WaveformMipmap { levels }
    }

    fn query_range(&self, start: usize, end: usize, samples: &[f32]) -> (f32, f32) {
        if start >= end {
            return (0.0, 0.0);
        }
        let range_len = end - start;
        if range_len < 8 {
            let mut mn = 0.0f32;
            let mut mx = 0.0f32;
            let end = end.min(samples.len());
            for &s in &samples[start..end] {
                mn = mn.min(s);
                mx = mx.max(s);
            }
            return (mn, mx);
        }

        let target_block = range_len / 2;
        let level = if target_block >= 2 {
            (target_block as f64).log2().floor() as usize - 1
        } else {
            0
        };
        let level = level.min(self.levels.len() - 1);
        let mip = &self.levels[level];
        let block_size = 1 << (level + 1);

        let mip_start = start / block_size;
        let mip_end = end.div_ceil(block_size).min(mip.len());

        let mut mn = 0.0f32;
        let mut mx = 0.0f32;
        for block in &mip[mip_start..mip_end] {
            mn = mn.min(block.0);
            mx = mx.max(block.1);
        }
        (mn, mx)
    }
}

#[derive(Debug)]
pub struct WaveformWidget {
    samples: Vec<f32>,
    sample_rate: u32,
    duration: Duration,
    current_position: Duration,
    mipmap: Option<MipmapHolder>,
    band_low: Vec<f32>,
    band_mid: Vec<f32>,
    band_high: Vec<f32>,
    accent: Accent,
}

struct MipmapHolder(WaveformMipmap);
impl std::fmt::Debug for MipmapHolder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MipmapHolder")
            .field("levels", &self.0.levels.len())
            .finish()
    }
}

impl WaveformWidget {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            samples: Vec::new(),
            sample_rate,
            duration: Duration::from_secs(0),
            current_position: Duration::from_secs(0),
            mipmap: None,
            band_low: Vec::new(),
            band_mid: Vec::new(),
            band_high: Vec::new(),
            accent: Accent::default(),
        }
    }

    pub fn set_accent(&mut self, accent: Accent) {
        self.accent = accent;
    }

    pub fn update_samples(
        &mut self,
        samples: Vec<f32>,
        bands: Option<(Vec<f32>, Vec<f32>, Vec<f32>)>,
        sample_rate: u32,
    ) {
        self.sample_rate = sample_rate;
        self.duration = Duration::from_secs_f64(samples.len() as f64 / self.sample_rate as f64);
        self.mipmap = if samples.is_empty() {
            None
        } else {
            Some(MipmapHolder(WaveformMipmap::build(&samples)))
        };
        if let Some((low, mid, high)) = bands {
            self.band_low = low;
            self.band_mid = mid;
            self.band_high = high;
        } else {
            self.band_low.clear();
            self.band_mid.clear();
            self.band_high.clear();
        }
        self.samples = samples;
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn set_position(&mut self, position: Duration) {
        if position > self.duration {
            return;
        }
        self.current_position = position;
    }

    pub fn show(&mut self, ui: &mut Ui) -> Response {
        let available_width = ui.available_width();
        let current_pos = self.current_position.as_secs_f64();
        let duration_secs = self.duration.as_secs_f64();

        // Overview waveform
        let (overview_rect, overview_response) =
            ui.allocate_exact_size(Vec2::new(available_width, OVERVIEW_HEIGHT), Sense::hover());
        self.draw_waveform(ui, &overview_rect, None, true);

        // Position cursor on overview
        if duration_secs > 0.0 {
            let pos_ratio = current_pos / duration_secs;
            let x = overview_rect.left() + overview_rect.width() * pos_ratio as f32;
            ui.painter().line_segment(
                [
                    Pos2::new(x, overview_rect.top()),
                    Pos2::new(x, overview_rect.bottom()),
                ],
                Stroke::new(1.5, CURSOR_COLOR),
            );
        }

        overview_response
    }

    fn draw_waveform(
        &self,
        ui: &mut Ui,
        rect: &Rect,
        view_range: Option<(f64, f64)>,
        is_overview: bool,
    ) {
        let painter = ui.painter();

        painter.rect_filled(*rect, 0.0, BACKGROUND_COLOR);

        let center_y = rect.center().y;
        painter.line_segment(
            [
                Pos2::new(rect.left(), center_y),
                Pos2::new(rect.right(), center_y),
            ],
            Stroke::new(1.0, GRIDLINE_COLOR),
        );

        if !is_overview {
            for frac in [0.25, 0.75] {
                let y = rect.top() + rect.height() * frac;
                painter.line_segment(
                    [Pos2::new(rect.left(), y), Pos2::new(rect.right(), y)],
                    Stroke::new(0.5, GRIDLINE_COLOR),
                );
            }
        }

        if self.samples.is_empty() {
            return;
        }

        let total_samples = self.samples.len();
        let width = rect.width() as f64;

        let (start_sample_f, samples_per_pixel) =
            if let Some((start_secs, duration_secs)) = view_range {
                let start = start_secs * self.sample_rate as f64;
                let total = duration_secs * self.sample_rate as f64;
                (start, total / width)
            } else {
                (0.0, total_samples as f64 / width)
            };

        let half_height = rect.height() / 2.0;

        let use_mipmap = self.mipmap.is_some() && samples_per_pixel >= 8.0;
        let col_count = rect.width() as usize;

        let has_bands = !self.band_low.is_empty();

        let mut columns: Vec<(f32, f32, Color32, Color32)> = Vec::with_capacity(col_count);
        for x in 0..col_count {
            let s_start_f = start_sample_f + x as f64 * samples_per_pixel;
            let s_end_f = s_start_f + samples_per_pixel;
            let s_start = s_start_f as usize;
            let s_end = (s_end_f.ceil() as usize).min(total_samples);

            if s_start >= total_samples {
                break;
            }

            let (min, max) = if use_mipmap {
                self.mipmap
                    .as_ref()
                    .unwrap()
                    .0
                    .query_range(s_start, s_end, &self.samples)
            } else {
                let mut mn = 0.0f32;
                let mut mx = 0.0f32;
                for &sample in &self.samples[s_start..s_end] {
                    mn = mn.min(sample);
                    mx = mx.max(sample);
                }
                (mn, mx)
            };

            let (fill, edge) = if has_bands {
                let band_end = s_end.min(self.band_low.len());
                if s_start < band_end {
                    let mut sum_low = 0.0f32;
                    let mut sum_mid = 0.0f32;
                    let mut sum_high = 0.0f32;
                    for i in s_start..band_end {
                        sum_low += self.band_low[i] * self.band_low[i];
                        sum_mid += self.band_mid[i] * self.band_mid[i];
                        sum_high += self.band_high[i] * self.band_high[i];
                    }
                    let n = (band_end - s_start) as f32;
                    band_color(
                        self.accent,
                        (sum_low / n).sqrt(),
                        (sum_mid / n).sqrt(),
                        (sum_high / n).sqrt(),
                    )
                } else {
                    band_color(self.accent, 0.0, 0.0, 0.0)
                }
            } else {
                band_color(self.accent, 0.0, 0.0, 0.0)
            };

            columns.push((min, max, fill, edge));
        }

        if columns.is_empty() {
            return;
        }

        // Build filled mesh — each column gets its own quad for per-column color
        let mut mesh = Mesh::default();
        mesh.reserve_triangles(columns.len() * 2);
        mesh.reserve_vertices(columns.len() * 4);

        for (x, &(min, max, fill, _)) in columns.iter().enumerate() {
            let px = rect.left() + x as f32;
            let px_next = px + 1.0;
            let y_top = center_y - max * half_height;
            let y_bot = center_y - min * half_height;
            let base = mesh.vertices.len() as u32;
            mesh.colored_vertex(Pos2::new(px, y_top), fill);
            mesh.colored_vertex(Pos2::new(px, y_bot), fill);
            mesh.colored_vertex(Pos2::new(px_next, y_top), fill);
            mesh.colored_vertex(Pos2::new(px_next, y_bot), fill);
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }

        painter.add(Shape::mesh(mesh));

        // Contour lines — draw per-segment to match column color
        for x in 0..columns.len().saturating_sub(1) {
            let (_, max_a, _, edge_a) = columns[x];
            let (_, max_b, _, _) = columns[x + 1];
            let xa = rect.left() + x as f32;
            let xb = xa + 1.0;
            painter.line_segment(
                [
                    Pos2::new(xa, center_y - max_a * half_height),
                    Pos2::new(xb, center_y - max_b * half_height),
                ],
                Stroke::new(1.0, edge_a),
            );
            let (min_a, _, _, _) = columns[x];
            let (min_b, _, _, _) = columns[x + 1];
            painter.line_segment(
                [
                    Pos2::new(xa, center_y - min_a * half_height),
                    Pos2::new(xb, center_y - min_b * half_height),
                ],
                Stroke::new(1.0, edge_a),
            );
        }
    }
}
