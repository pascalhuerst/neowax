use eframe::egui;
use std::f32::consts::PI;
use std::time::Instant;

/// Very gentle drift correction — just enough to prevent long-term phase drift.
/// At 0.02 per frame (60fps), a full-revolution error corrects in ~3 seconds.
const DRIFT_CORRECTION: f32 = 0.02;

/// Represents a spinner UI element that shows vinyl rotation
pub struct Spinner {
    size: f32,
    monitor_data: Option<Vec<u8>>,
    primary_color: egui::Color32,
    secondary_color: egui::Color32,

    /// Angle in radians, driven by integrating pitch. Never jumps.
    angle: f32,
    /// Latest raw vinyl pitch from timecoder (revolutions per second, relative)
    vinyl_pitch: f32,
    /// Latest timecoder position (for gentle drift correction)
    engine_position: f32,
    /// Last frame timestamp
    last_frame_time: Instant,
    /// Whether we have ever received a valid position
    has_position: bool,
    /// Resolution: timecode positions per revolution
    resolution: f32,
}

impl Spinner {
    pub fn new(size: f32) -> Self {
        Self {
            size,
            monitor_data: None,
            primary_color: egui::Color32::from_rgb(60, 140, 255),
            secondary_color: egui::Color32::from_rgb(15, 35, 65),
            angle: 0.0,
            vinyl_pitch: 0.0,
            engine_position: 0.0,
            last_frame_time: Instant::now(),
            has_position: false,
            resolution: 1000.0,
        }
    }

    pub fn update_monitor(&mut self, data: Option<Vec<u8>>) {
        self.monitor_data = data;
    }

    pub fn update_position(
        &mut self,
        position: Option<i32>,
        vinyl_pitch: f64,
        resolution: f64,
    ) {
        self.resolution = resolution as f32;
        self.vinyl_pitch = vinyl_pitch as f32;
        if let Some(pos) = position {
            self.engine_position = pos as f32;
            if !self.has_position {
                // First valid position: snap angle to match
                self.angle = (pos as f32 / self.resolution) * 2.0 * PI;
                self.has_position = true;
            }
        }
    }

    fn advance(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame_time).as_secs_f32();
        self.last_frame_time = now;
        let dt = dt.min(0.05);

        // Integrate pitch to advance angle smoothly
        // vinyl_pitch is in "track-seconds per real-second" at resolution positions/sec
        // One revolution = 2π radians = resolution positions
        // Angular velocity = vinyl_pitch * 2π radians/sec
        self.angle += self.vinyl_pitch * 2.0 * PI * dt;

        // Gentle drift correction toward the timecoder's actual position.
        // Convert engine position to angle and compute the shortest angular error.
        let target_angle = (self.engine_position / self.resolution) * 2.0 * PI;
        let mut error = target_angle - self.angle;

        // Wrap error to [-π, π] so correction takes the short way around
        error = error % (2.0 * PI);
        if error > PI {
            error -= 2.0 * PI;
        } else if error < -PI {
            error += 2.0 * PI;
        }

        self.angle += error * DRIFT_CORRECTION;
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) -> egui::Response {
        self.advance();

        let (rect, response) = ui.allocate_exact_size(
            egui::vec2(self.size, self.size),
            egui::Sense::hover(),
        );

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();

            if let Some(monitor) = &self.monitor_data {
                let size = self.size as usize;

                for y in 0..size {
                    for x in 0..size {
                        let idx = y * size + x;
                        if idx < monitor.len() {
                            let intensity = monitor[idx];
                            let color = if intensity > 0 {
                                let scale = intensity as f32 / 255.0;
                                self.primary_color.linear_multiply(scale)
                            } else {
                                self.secondary_color
                            };

                            let pos = rect.min + egui::vec2(x as f32, y as f32);
                            painter.rect_filled(
                                egui::Rect::from_min_size(pos, egui::vec2(1.0, 1.0)),
                                0.0,
                                color,
                            );
                        }
                    }
                }
            }

            if self.has_position {
                let center = rect.center();
                let radius = self.size / 2.0;

                let end = egui::pos2(
                    center.x + radius * self.angle.cos(),
                    center.y + radius * self.angle.sin(),
                );

                painter.line_segment(
                    [center, end],
                    egui::Stroke::new(2.0, self.primary_color),
                );
            }
        }

        response
    }
}
