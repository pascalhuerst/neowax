//! The log viewer (Settings > Debug): the most recent lines the app logged
//! during this run.
//!
//! Scroll with the encoder (or drag on the touch screen); a click goes back.

use super::*;
use neowax::logbuf::LogBuffer;

/// Pixels the view moves per encoder tick.
const SCROLL_STEP: f32 = 42.0;

pub struct LogsState {
    lines: Vec<String>,
    scroll_y: f32,
}

impl LogsState {
    pub fn new() -> Self {
        Self {
            lines: Vec::new(),
            scroll_y: 0.0,
        }
    }

    /// Take a fresh copy of the log and jump to its newest line.
    fn reload(&mut self) {
        self.lines = LogBuffer::global().snapshot();
        self.scroll_y = f32::MAX;
    }
}

impl NeowaxApp {
    pub(super) fn open_logs(&mut self) {
        self.logs.reload();
        self.view = View::Logs;
    }

    /// Encoder scroll in the viewer.
    pub(super) fn scroll_logs(&mut self, up: bool, down: bool) {
        if up {
            self.logs.scroll_y = (self.logs.scroll_y.min(1.0e7) - SCROLL_STEP).max(0.0);
        }
        if down {
            self.logs.scroll_y += SCROLL_STEP;
        }
    }

    pub(super) fn draw_logs_view(&mut self, ui: &mut egui::Ui) {
        let dim = Color32::from_rgb(180, 170, 150);
        let (mut close, mut refresh) = (false, false);

        ui.horizontal(|ui| {
            let title = |text: &str| RichText::new(text).color(self.accent.bright()).size(16.0);
            close = ui.selectable_label(true, title("[Back]")).clicked();
            refresh = ui.selectable_label(false, title("[Refresh]")).clicked();
            ui.label(title("Log of this run"));
        });
        ui.add_space(4.0);

        if self.logs.lines.is_empty() {
            ui.label(RichText::new("No log lines yet.").color(dim).size(14.0));
        } else {
            let output = egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .vertical_scroll_offset(self.logs.scroll_y)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    for line in &self.logs.lines {
                        let color = if line.contains("ERROR") {
                            CLIP_COLOR
                        } else if line.contains("WARN") {
                            self.accent.base()
                        } else {
                            dim
                        };
                        ui.label(RichText::new(line).monospace().size(12.0).color(color));
                    }
                });
            // Keep our offset in step with touch dragging and with clamping
            self.logs.scroll_y = output.state.offset.y;
        }

        if refresh {
            self.logs.reload();
        }
        if close {
            self.view = View::Settings;
        }
    }
}
