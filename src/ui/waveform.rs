use egui::{epaint::Mesh, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2};
use std::time::Duration;

const OVERVIEW_HEIGHT: f32 = 112.0;
const WAVEFORM_FILL: Color32 = Color32::from_rgb(30, 80, 160);
const WAVEFORM_EDGE: Color32 = Color32::from_rgb(80, 170, 255);
const CURSOR_COLOR: Color32 = Color32::from_rgb(255, 255, 255);
const BACKGROUND_COLOR: Color32 = Color32::from_rgb(12, 12, 18);
const GRIDLINE_COLOR: Color32 = Color32::from_rgb(30, 30, 40);

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
        let mip_end = ((end + block_size - 1) / block_size).min(mip.len());

        let mut mn = 0.0f32;
        let mut mx = 0.0f32;
        for i in mip_start..mip_end {
            mn = mn.min(mip[i].0);
            mx = mx.max(mip[i].1);
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
        }
    }

    pub fn update_samples(
        &mut self,
        samples: Vec<f32>,
        bands: Option<(Vec<f32>, Vec<f32>, Vec<f32>)>,
        sample_rate: u32,
    ) {
        self.sample_rate = sample_rate;
        self.duration =
            Duration::from_secs_f64(samples.len() as f64 / self.sample_rate as f64);
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
        let (overview_rect, overview_response) = ui.allocate_exact_size(
            Vec2::new(available_width, OVERVIEW_HEIGHT),
            Sense::hover(),
        );
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

        let (start_sample_f, samples_per_pixel) = if let Some((start_secs, duration_secs)) =
            view_range
        {
            let start = start_secs * self.sample_rate as f64;
            let total = duration_secs * self.sample_rate as f64;
            (start, total / width)
        } else {
            (0.0, total_samples as f64 / width)
        };

        let half_height = rect.height() / 2.0;

        let use_mipmap = self.mipmap.is_some() && samples_per_pixel >= 8.0;
        let col_count = rect.width() as usize;

        let mut columns: Vec<(f32, f32)> = Vec::with_capacity(col_count);
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

            columns.push((min, max));
        }

        if columns.is_empty() {
            return;
        }

        // Build filled mesh
        let mut mesh = Mesh::default();
        mesh.reserve_triangles(columns.len() * 2);
        mesh.reserve_vertices(columns.len() * 2);

        for (x, &(min, max)) in columns.iter().enumerate() {
            let px = rect.left() + x as f32;
            let y_top = center_y - max * half_height;
            let y_bot = center_y - min * half_height;
            mesh.colored_vertex(Pos2::new(px, y_top), WAVEFORM_FILL);
            mesh.colored_vertex(Pos2::new(px, y_bot), WAVEFORM_FILL);
        }

        for x in 0..columns.len() - 1 {
            let i = (x * 2) as u32;
            mesh.add_triangle(i, i + 1, i + 2);
            mesh.add_triangle(i + 1, i + 3, i + 2);
        }

        painter.add(Shape::mesh(mesh));

        // Contour lines
        let top_points: Vec<Pos2> = columns
            .iter()
            .enumerate()
            .map(|(x, &(_, max))| Pos2::new(rect.left() + x as f32, center_y - max * half_height))
            .collect();
        let bot_points: Vec<Pos2> = columns
            .iter()
            .enumerate()
            .map(|(x, &(min, _))| Pos2::new(rect.left() + x as f32, center_y - min * half_height))
            .collect();

        painter.add(Shape::line(top_points, Stroke::new(1.0, WAVEFORM_EDGE)));
        painter.add(Shape::line(bot_points, Stroke::new(1.0, WAVEFORM_EDGE)));
    }
}
