use clap::Parser;
use eframe::egui::{self, Color32, Frame, Margin, RichText, Rounding};
use neowax::audio::{DeckState, Engine, Track, TrackCommand};
use neowax::library::{self, FlatEntry, FlatEntryKind, LibraryNode};
use neowax::sysinfo;
use neowax::timecode::TimecodeDef;
use neowax::ui::{Accent, Spinner, WaveformWidget, ACCENT_PRESETS};
use neowax::update::{self, DebInfo};
use std::cmp::Ordering as VersionOrdering;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};

const BORDER: f32 = 8.0;
const SPACER: f32 = 6.0;
/// Per-frame falloff of the peak meters (~20 dB/s at 60 fps)
const METER_DECAY: f32 = 0.96;
/// Width reserved for the "In" / "Out" meter labels
const METER_LABEL_WIDTH: f32 = 32.0;
/// Bottom of the meter scale in dB
const METER_FLOOR_DB: f32 = -48.0;

#[derive(Parser, Debug)]
#[command(author, version, about = "neowax - vinyl timecode DJ player")]
struct Args {
    /// ALSA device for both input and output (e.g. "plughw:sndrpihifiberry")
    #[arg(long)]
    alsa: Option<String>,

    /// ALSA input device (overrides --alsa for input)
    #[arg(long)]
    input_device: Option<String>,

    /// ALSA output device (overrides --alsa for output)
    #[arg(long)]
    output_device: Option<String>,

    /// Sample rate in Hz
    #[arg(short, long, default_value_t = 48000)]
    sample_rate: u32,

    /// Timecode format
    #[arg(short, long, default_value = "serato_2a")]
    timecode: String,

    /// Directory to scan for audio files
    #[arg(long, alias = "crate")]
    library: PathBuf,

    /// Window geometry WxH (e.g. "800x480")
    #[arg(long)]
    geometry: Option<String>,

    /// No window decorations (title bar, borders)
    #[arg(long)]
    no_decor: bool,

    /// Vinyl speed multiplier
    #[arg(long, default_value_t = 1.0)]
    speed: f64,

    /// Phono-level input
    #[arg(long)]
    phono: bool,

    /// ALSA playback buffer size in frames (default: 256, ~5.3ms at 48kHz)
    #[arg(long)]
    buffer: Option<u32>,

    /// ALSA capture buffer size in frames (default: 1024, ~21ms at 48kHz)
    #[arg(long)]
    capture_buffer: Option<u32>,

    /// CPU core to pin the capture thread to (default: 3)
    #[arg(long, default_value_t = 3)]
    capture_cpu: usize,

    /// CPU core to pin the playback thread to (default: 2)
    #[arg(long, default_value_t = 2)]
    playback_cpu: usize,

    /// Parse arguments and exit (used to validate a restart after an update)
    #[arg(long, hide = true)]
    check_args: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Deck,
    Library,
    Settings,
    Update,
}

/// Library list selection sentinels, ordered as displayed above the tree.
const SEL_BACK: usize = usize::MAX - 2;
const SEL_SETTINGS: usize = usize::MAX - 1;
const SEL_RESCAN: usize = usize::MAX;

#[derive(PartialEq, Clone, Copy)]
enum SettingsRow {
    Back,
    Pitch,
    Accent,
    Update,
}

struct SettingsState {
    /// Index into the currently visible rows
    selected: usize,
    pitch_open: bool,
    /// A value row is being edited: the encoder changes the value, click confirms
    editing: bool,
    /// View to return to on [Back]
    return_to: View,
}

enum UpdatePhase {
    /// Waiting for the user to pick Install or Cancel
    Confirm,
    Installing(mpsc::Receiver<Result<(), String>>),
    Failed(String),
}

struct UpdateState {
    info: DebInfo,
    installed: String,
    ordering: VersionOrdering,
    /// 0 = Install, 1 = Cancel
    selected: usize,
    phase: UpdatePhase,
}

struct LoadedTrack {
    track: Arc<Track>,
    waveform: Vec<f32>,
    bands: (Vec<f32>, Vec<f32>, Vec<f32>),
    name: String,
    path: PathBuf,
    duration: f64,
}

struct NeowaxApp {
    view: View,

    // Engine state
    deck_state: Arc<DeckState>,
    track_sender: mpsc::Sender<TrackCommand>,

    // Track loading
    load_request_tx: mpsc::Sender<(PathBuf, String)>,
    loaded_rx: mpsc::Receiver<LoadedTrack>,
    loading: bool,
    load_progress: Arc<AtomicU32>,

    // Library
    library_dir: PathBuf,
    library_tree: Vec<LibraryNode>,
    library_flat: Vec<FlatEntry>,
    selected_index: usize,
    current_track_path: Option<PathBuf>,

    // UI widgets
    spinner: Spinner,
    waveform: WaveformWidget,

    // Current track
    track_name: String,
    track_path: Option<PathBuf>,
    track_duration: f64,

    // Smoothed signal quality (0.0 to 1.0)
    signal_quality: f32,

    // Timecode resolution (positions per revolution, accounting for speed)
    timecode_resolution: f64,

    // Track name scroll state (marquee for long names)
    title_scroll_offset: f32,

    // Pitch history for calibration plot (ring buffer)
    pitch_history: VecDeque<f32>,

    // Peak meters, held and decayed per frame: [in L, in R, out L, out R]
    meters: [f32; 4],

    // Settings panel
    settings: SettingsState,
    accent_index: usize,
    accent: Accent,
    os_version: String,
    /// Best neowax .deb found while scanning the library
    available_update: Option<DebInfo>,

    // Status flash message
    status_message: String,
    status_timer: f64,

    // Self-update installer state (opened from the settings panel)
    update: Option<UpdateState>,
    restart_requested: Arc<AtomicBool>,
}

impl NeowaxApp {
    fn new(
        deck_state: Arc<DeckState>,
        track_sender: mpsc::Sender<TrackCommand>,
        library_dir: PathBuf,
        scan: library::Scan,
        sample_rate: u32,
        timecode_resolution: f64,
        restart_requested: Arc<AtomicBool>,
    ) -> Self {
        let library_tree = scan.tree;
        let library_flat = library::flatten_tree(&library_tree);
        let (load_request_tx, load_request_rx) = mpsc::channel::<(PathBuf, String)>();
        let (loaded_tx, loaded_rx) = mpsc::channel::<LoadedTrack>();
        let load_progress = Arc::new(AtomicU32::new(0));
        let load_progress_writer = load_progress.clone();

        std::thread::spawn(move || {
            while let Ok((path, name)) = load_request_rx.recv() {
                let path_str = path.to_string_lossy().to_string();
                load_progress_writer.store(0, Ordering::Relaxed);
                let progress_ref = &load_progress_writer;
                match Track::from_file_with_progress(&path_str, &|p| {
                    progress_ref.store((p * 1000.0) as u32, Ordering::Relaxed);
                }) {
                    Ok(track) => {
                        load_progress_writer.store(900, Ordering::Relaxed);
                        let waveform = track.waveform_data();
                        load_progress_writer.store(950, Ordering::Relaxed);
                        let bands = track.frequency_bands();
                        load_progress_writer.store(1000, Ordering::Relaxed);
                        let duration = track.length as f64 / track.rate as f64;
                        loaded_tx
                            .send(LoadedTrack {
                                track,
                                bands,
                                waveform,
                                name,
                                path: path.clone(),
                                duration,
                            })
                            .ok();
                    }
                    Err(e) => {
                        tracing::error!("Failed to load track: {}", e);
                        load_progress_writer.store(0, Ordering::Relaxed);
                    }
                }
            }
        });

        Self {
            view: View::Library,
            deck_state,
            track_sender,
            load_request_tx,
            loaded_rx,
            loading: false,
            load_progress,
            library_dir,
            library_tree,
            library_flat,
            selected_index: 0,
            current_track_path: None,
            spinner: Spinner::new(128.0),
            waveform: WaveformWidget::new(sample_rate),
            track_name: String::new(),
            track_path: None,
            track_duration: 0.0,
            signal_quality: 0.0,
            title_scroll_offset: 0.0,
            pitch_history: VecDeque::with_capacity(600),
            timecode_resolution,
            meters: [0.0; 4],
            settings: SettingsState {
                selected: 0,
                pitch_open: false,
                editing: false,
                return_to: View::Deck,
            },
            accent_index: 0,
            accent: Accent::default(),
            os_version: sysinfo::os_version(),
            available_update: update::find_best(&scan.packages),
            status_message: String::new(),
            status_timer: 0.0,
            update: None,
            restart_requested,
        }
    }

    fn is_deck_active(&self) -> bool {
        self.deck_state.get_pitch().abs() > 0.01
    }

    fn rescan_library(&mut self) {
        // Touch the directory to trigger systemd automount if needed
        let _ = std::fs::read_dir(&self.library_dir);
        let scan = library::scan_directory(&self.library_dir);
        self.library_tree = scan.tree;
        self.available_update = update::find_best(&scan.packages);
        self.library_flat = library::flatten_tree(&self.library_tree);
        self.selected_index = 0;
        tracing::info!("Rescanned library: {} entries", self.library_flat.len());
    }

    fn clear_deck(&mut self) {
        self.track_sender.send(TrackCommand::Clear).ok();
        self.track_name.clear();
        self.track_path = None;
        self.track_duration = 0.0;
        self.current_track_path = None;
        self.waveform
            .update_samples(Vec::new(), None, self.waveform.sample_rate());
    }

    fn load_selected_track(&mut self) {
        let entry = match self.library_flat.get(self.selected_index) {
            Some(e) => match &e.kind {
                FlatEntryKind::Track { path, display_name } => (path.clone(), display_name.clone()),
                _ => return,
            },
            None => return,
        };
        self.clear_deck();
        self.current_track_path = Some(entry.0.clone());
        self.track_name = format!("Loading {}...", entry.1);
        self.loading = true;
        self.load_request_tx.send(entry).ok();
    }

    fn flash(&mut self, msg: impl Into<String>) {
        self.status_message = msg.into();
        self.status_timer = 3.0;
    }

    /// Open the update view for a neowax package found in the library.
    fn begin_update(&mut self, info: DebInfo) {
        let installed = update::installed_version();
        let ordering =
            update::compare_versions(&info.version, &installed).unwrap_or(VersionOrdering::Equal);
        self.update = Some(UpdateState {
            info,
            installed,
            ordering,
            // Default to Cancel when the package is not newer
            selected: if ordering == VersionOrdering::Greater {
                0
            } else {
                1
            },
            phase: UpdatePhase::Confirm,
        });
        self.view = View::Update;
    }

    fn start_install(&mut self) {
        let Some(state) = self.update.as_mut() else {
            return;
        };
        let path = state.info.path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = update::install(&path).and_then(|()| update::check_restart_args());
            tx.send(result.map_err(|e| e.to_string())).ok();
        });
        state.phase = UpdatePhase::Installing(rx);
    }

    fn check_update_progress(&mut self, ctx: &egui::Context) {
        let Some(state) = self.update.as_mut() else {
            return;
        };
        let UpdatePhase::Installing(rx) = &state.phase else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(())) => {
                tracing::info!("Update installed, restarting");
                self.restart_requested.store(true, Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            Ok(Err(e)) => {
                tracing::error!("Update failed: {}", e);
                state.phase = UpdatePhase::Failed(e);
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                state.phase = UpdatePhase::Failed("installer thread died".to_string());
            }
        }
    }

    fn draw_update_view(&self, ui: &mut egui::Ui) {
        let Some(state) = &self.update else {
            return;
        };
        let amber = self.accent.base();
        let dim = Color32::from_rgb(180, 170, 150);

        ui.label(
            RichText::new("Update neowax")
                .color(self.accent.bright())
                .size(20.0),
        );
        ui.add_space(SPACER);
        ui.label(
            RichText::new(format!("Installed: {}", state.installed))
                .color(dim)
                .size(18.0),
        );
        ui.label(
            RichText::new(format!("Package:   {}", state.info.version))
                .color(amber)
                .size(18.0),
        );
        match state.ordering {
            VersionOrdering::Greater => {}
            VersionOrdering::Equal => {
                ui.label(
                    RichText::new("Same version as installed")
                        .color(Color32::from_rgb(200, 80, 30))
                        .size(16.0),
                );
            }
            VersionOrdering::Less => {
                ui.label(
                    RichText::new("Older than installed version!")
                        .color(Color32::from_rgb(200, 80, 30))
                        .size(16.0),
                );
            }
        }
        ui.add_space(SPACER * 2.0);

        match &state.phase {
            UpdatePhase::Confirm => {
                for (i, label) in ["Install", "Cancel"].iter().enumerate() {
                    let selected = state.selected == i;
                    let _ = ui.selectable_label(
                        selected,
                        RichText::new(*label)
                            .color(if selected {
                                self.accent.bright()
                            } else {
                                self.accent.mid()
                            })
                            .size(20.0),
                    );
                }
            }
            UpdatePhase::Installing(_) => {
                ui.label(RichText::new("Installing...").color(amber).size(20.0));
            }
            UpdatePhase::Failed(msg) => {
                ui.label(
                    RichText::new(format!("Install failed: {msg}"))
                        .color(Color32::from_rgb(200, 80, 30))
                        .size(16.0),
                );
                ui.add_space(SPACER);
                ui.label(RichText::new("Press to go back").color(dim).size(16.0));
            }
        }
    }

    fn check_loaded_track(&mut self) {
        if let Ok(loaded) = self.loaded_rx.try_recv() {
            self.waveform
                .update_samples(loaded.waveform, Some(loaded.bands), loaded.track.rate);
            self.track_sender
                .send(TrackCommand::Load(loaded.track))
                .ok();
            self.track_name = loaded.name;
            self.title_scroll_offset = 0.0;
            self.track_path = Some(loaded.path.clone());
            self.track_duration = loaded.duration;
            self.current_track_path = Some(loaded.path);
            self.loading = false;
            self.view = View::Deck;
        }
    }

    fn update_from_engine(&mut self) {
        let peaks = self.deck_state.take_peaks();
        for (meter, peak) in self.meters.iter_mut().zip(peaks) {
            *meter = peak.max(*meter * METER_DECAY);
        }

        // If the track file disappeared (USB removed), clear the deck
        if let Some(path) = &self.track_path {
            if !path.exists() {
                tracing::info!("Track file removed, clearing deck");
                self.clear_deck();
                self.view = View::Library;
                self.rescan_library();
                return;
            }
        }

        if let Ok(mon) = self.deck_state.monitor.try_lock() {
            if !mon.is_empty() {
                self.spinner.update_monitor(Some(mon.clone()));
            }
        }
        self.spinner.update_position(
            self.deck_state.get_timecode_pos(),
            self.deck_state.get_vinyl_pitch(),
            self.timecode_resolution,
        );

        // Smooth signal quality: fast attack, slow decay
        let valid = self.deck_state.valid_counter.load(Ordering::Relaxed);
        let raw_quality = (valid as f32 / 24.0).min(1.0);
        if raw_quality > self.signal_quality {
            self.signal_quality += (raw_quality - self.signal_quality) * 0.3;
        } else {
            self.signal_quality += (raw_quality - self.signal_quality) * 0.05;
        }

        let elapsed = self.deck_state.get_elapsed();
        if elapsed.is_finite() && elapsed >= 0.0 {
            self.waveform
                .set_position(std::time::Duration::from_secs_f64(elapsed));
        }

        // Record pitch history (~10s window at 60fps)
        let vinyl_pitch = self.deck_state.get_vinyl_pitch() as f32;
        if vinyl_pitch.is_finite() {
            self.pitch_history.push_back(vinyl_pitch);
            if self.pitch_history.len() > 600 {
                self.pitch_history.pop_front();
            }
        }
    }

    fn format_time(secs: f64) -> String {
        if !secs.is_finite() || secs < 0.0 {
            return "--:--".to_string();
        }
        let m = (secs / 60.0) as u32;
        let s = secs % 60.0;
        format!("{}:{:04.1}", m, s)
    }

    fn draw_deck_view(&mut self, ui: &mut egui::Ui) {
        let elapsed = self.deck_state.get_elapsed();
        let remaining = self.track_duration - elapsed;

        // Row 1: Spinner + signal bar + track name
        ui.horizontal(|ui| {
            self.spinner.ui(ui);

            // Signal quality bar (vertical, next to spinner)
            let bar_width = 6.0;
            let bar_height = 128.0; // match spinner size
            let (bar_rect, _) =
                ui.allocate_exact_size(egui::vec2(bar_width, bar_height), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(bar_rect, 0.0, Color32::from_rgb(20, 20, 20));
            let fill_height = bar_height * self.signal_quality;
            if fill_height > 0.0 {
                let color = if self.signal_quality > 0.9 {
                    self.accent.base()
                } else if self.signal_quality > 0.5 {
                    self.accent.mid()
                } else {
                    Color32::from_rgb(160, 50, 20)
                };
                let fill_rect = egui::Rect::from_min_max(
                    egui::pos2(bar_rect.left(), bar_rect.bottom() - fill_height),
                    bar_rect.max,
                );
                painter.rect_filled(fill_rect, 0.0, color);
            }

            ui.add_space(SPACER);

            // Track name with marquee scroll for long titles
            let text = if self.track_name.is_empty() {
                "No track loaded"
            } else {
                &self.track_name
            };
            let available = ui.available_width();
            let galley = ui.painter().layout_no_wrap(
                text.to_string(),
                egui::FontId::proportional(36.0),
                Color32::WHITE,
            );
            let text_width = galley.size().x;

            if text_width <= available {
                // Fits — just show it, reset scroll
                self.title_scroll_offset = 0.0;
                ui.label(RichText::new(text).color(Color32::WHITE).size(36.0));
            } else {
                // Marquee: scroll back and forth
                let overflow = text_width - available;
                // Scroll speed: 30 pixels/sec, ping-pong
                let cycle = (overflow / 30.0) * 2.0 + 2.0; // seconds for full cycle + pauses
                let t = self.title_scroll_offset % cycle;
                let pause = 1.0; // 1 second pause at each end
                let scroll = if t < pause {
                    0.0
                } else if t < pause + overflow / 30.0 {
                    (t - pause) * 30.0
                } else if t < pause * 2.0 + overflow / 30.0 {
                    overflow
                } else {
                    overflow - (t - pause * 2.0 - overflow / 30.0) * 30.0
                }
                .clamp(0.0, overflow);

                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(available, galley.size().y),
                    egui::Sense::hover(),
                );
                ui.painter()
                    .with_clip_rect(rect)
                    .galley(egui::pos2(rect.left() - scroll, rect.top()), galley);
                self.title_scroll_offset += ui.input(|i| i.predicted_dt);
            }
        });

        ui.add_space(SPACER);

        // Row 2: Elapsed (left) | Remaining (right)
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(Self::format_time(elapsed))
                    .color(self.accent.base())
                    .size(20.0)
                    .monospace(),
            );

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("-{}", Self::format_time(remaining)))
                        .color(self.accent.base())
                        .size(20.0)
                        .monospace(),
                );
            });
        });

        ui.add_space(SPACER);

        // Waveform
        self.waveform.show(ui);

        ui.add_space(SPACER);

        // Bottom bar: settings button pinned to bottom
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
            let btn = ui.add_sized(
                [ui.available_width(), 32.0],
                egui::Button::new(
                    RichText::new("Settings")
                        .color(self.accent.mid())
                        .size(14.0),
                )
                .fill(self.accent.tint())
                .rounding(4.0),
            );
            if btn.clicked() {
                self.open_settings();
            }
        });
    }

    fn open_settings(&mut self) {
        self.settings.return_to = self.view;
        self.settings.selected = 0;
        self.settings.editing = false;
        self.view = View::Settings;
    }

    fn settings_rows(&self) -> Vec<SettingsRow> {
        let mut rows = vec![SettingsRow::Back, SettingsRow::Pitch, SettingsRow::Accent];
        if self.available_update.is_some() {
            rows.push(SettingsRow::Update);
        }
        rows
    }

    fn set_accent(&mut self, index: usize) {
        self.accent_index = index % ACCENT_PRESETS.len();
        self.accent = Accent::from_preset(self.accent_index);
        self.spinner.set_accent(self.accent);
        self.waveform.set_accent(self.accent);
    }

    fn step_accent(&mut self, delta: isize) {
        let n = ACCENT_PRESETS.len() as isize;
        self.set_accent((self.accent_index as isize + delta).rem_euclid(n) as usize);
    }

    /// The encoder was clicked (or the row tapped) on a settings row.
    fn activate_setting(&mut self, row: SettingsRow) {
        match row {
            SettingsRow::Back => {
                self.settings.editing = false;
                self.view = self.settings.return_to;
            }
            SettingsRow::Pitch => self.settings.pitch_open = !self.settings.pitch_open,
            SettingsRow::Accent => self.settings.editing = !self.settings.editing,
            SettingsRow::Update => {
                if self.is_deck_active() {
                    self.flash("Stop the deck first!");
                } else if let Some(info) = self.available_update.clone() {
                    self.begin_update(info);
                }
            }
        }
    }

    /// Compact stereo peak meters for input (timecode) and output.
    fn draw_meters(&self, ui: &mut egui::Ui) {
        let label_color = Color32::from_rgb(180, 170, 150);
        let bar_height = 6.0;
        let gap = 2.0;

        ui.horizontal(|ui| {
            let half = (ui.available_width() - SPACER * 3.0) / 2.0;
            for (name, pair) in [("In", &self.meters[0..2]), ("Out", &self.meters[2..4])] {
                ui.add_sized(
                    [METER_LABEL_WIDTH, 14.0],
                    egui::Label::new(RichText::new(name).color(label_color).size(14.0)),
                );
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2((half - METER_LABEL_WIDTH).max(20.0), bar_height * 2.0 + gap),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                for (i, &peak) in pair.iter().enumerate() {
                    let top = rect.top() + i as f32 * (bar_height + gap);
                    let track = egui::Rect::from_min_size(
                        egui::pos2(rect.left(), top),
                        egui::vec2(rect.width(), bar_height),
                    );
                    painter.rect_filled(track, 0.0, Color32::from_rgb(20, 20, 20));

                    let db = 20.0 * peak.max(1e-6).log10();
                    let frac = ((db - METER_FLOOR_DB) / -METER_FLOOR_DB).clamp(0.0, 1.0);
                    let color = if db > -3.0 {
                        Color32::from_rgb(200, 80, 30)
                    } else {
                        self.accent.base()
                    };
                    let fill = egui::Rect::from_min_size(
                        track.min,
                        egui::vec2(track.width() * frac, bar_height),
                    );
                    painter.rect_filled(fill, 0.0, color);
                }
                ui.add_space(SPACER);
            }
        });
    }

    fn draw_status(&self, ui: &mut egui::Ui) {
        if !self.status_message.is_empty() {
            ui.add_space(SPACER);
            ui.label(
                RichText::new(&self.status_message)
                    .color(Color32::from_rgb(200, 80, 30))
                    .size(16.0),
            );
        }
    }

    /// Returns the row that was tapped, if any.
    fn draw_settings_view(&self, ui: &mut egui::Ui) -> Option<(usize, SettingsRow)> {
        let rows = self.settings_rows();
        let mut clicked = None;

        ui.label(
            RichText::new("Settings")
                .color(self.accent.bright())
                .size(20.0),
        );
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .enable_scrolling(false)
            .show(ui, |ui| {
                for (idx, &row) in rows.iter().enumerate() {
                    let selected = idx == self.settings.selected;
                    let text = match row {
                        SettingsRow::Back => "[Back]".to_string(),
                        SettingsRow::Pitch => {
                            let fold = if self.settings.pitch_open { "v" } else { ">" };
                            format!("{fold} Pitch")
                        }
                        SettingsRow::Accent => {
                            let name = ACCENT_PRESETS[self.accent_index].0;
                            if selected && self.settings.editing {
                                format!("Accent color: < {name} >")
                            } else {
                                format!("Accent color: {name}")
                            }
                        }
                        SettingsRow::Update => {
                            let version = self.available_update.as_ref().map_or("", |u| &u.version);
                            format!("Install update: {version}")
                        }
                    };

                    ui.horizontal(|ui| {
                        let response = ui.selectable_label(
                            selected,
                            RichText::new(text)
                                .color(if selected {
                                    self.accent.bright()
                                } else {
                                    self.accent.mid()
                                })
                                .size(16.0),
                        );
                        if row == SettingsRow::Accent {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 3.0, self.accent.base());
                        }
                        if selected {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            clicked = Some((idx, row));
                        }
                    });

                    if row == SettingsRow::Pitch && self.settings.pitch_open {
                        self.draw_pitch_plot(ui, 120.0);
                        ui.add_space(4.0);
                    }
                }

                ui.add_space(SPACER);
                self.draw_meters(ui);
                ui.separator();
                let info_color = Color32::from_rgb(180, 170, 150);
                ui.label(
                    RichText::new(format!("neowax {}", env!("CARGO_PKG_VERSION")))
                        .color(info_color)
                        .size(14.0),
                );
                ui.label(RichText::new(&self.os_version).color(info_color).size(14.0));
            });

        self.draw_status(ui);
        clicked
    }

    fn draw_pitch_plot(&self, ui: &mut egui::Ui, height: f32) {
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());

        let painter = ui.painter();
        painter.rect_filled(rect, 0.0, Color32::from_rgb(12, 12, 18));

        // Auto-range: find min/max of visible data, add small margin
        let mut min_p = 1.0f32;
        let mut max_p = 1.0f32;
        for &p in &self.pitch_history {
            if p.is_finite() && p > 0.1 {
                min_p = min_p.min(p);
                max_p = max_p.max(p);
            }
        }
        // Ensure at least ±0.2% so the plot isn't a flat line
        let margin = ((max_p - min_p) * 0.1).max(0.002);
        let plot_min = min_p - margin;
        let plot_max = max_p + margin;
        let plot_range = plot_max - plot_min;
        let pitch_to_y = |p: f32| rect.bottom() - ((p - plot_min) / plot_range) * rect.height();

        // 0% reference line (pitch = 1.0), always drawn if in range
        if plot_min < 1.0 && 1.0 < plot_max {
            let y = pitch_to_y(1.0);
            painter.line_segment(
                [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
                egui::Stroke::new(1.0_f32, Color32::from_rgb(50, 50, 60)),
            );
            painter.text(
                egui::pos2(rect.left() + 4.0, y - 2.0),
                egui::Align2::LEFT_BOTTOM,
                "0%",
                egui::FontId::monospace(9.0),
                Color32::from_rgb(80, 80, 90),
            );
        }

        // Draw pitch trace
        let n = self.pitch_history.len();
        if n < 2 {
            return;
        }

        let w = rect.width();
        let mut points: Vec<egui::Pos2> = Vec::with_capacity(n);
        for (i, &p) in self.pitch_history.iter().enumerate() {
            if !p.is_finite() || p < 0.1 {
                continue;
            }
            let x = rect.left() + w * (i as f32 / (n - 1).max(1) as f32);
            let y = pitch_to_y(p);
            points.push(egui::pos2(x, y));
        }

        if points.len() >= 2 {
            painter.add(egui::Shape::line(
                points,
                egui::Stroke::new(1.5_f32, self.accent.base()),
            ));
        }

        // Current value
        if let Some(&last) = self.pitch_history.back() {
            if last.is_finite() && last > 0.1 {
                painter.text(
                    egui::pos2(rect.right() - 4.0, rect.top() + 2.0),
                    egui::Align2::RIGHT_TOP,
                    format!("{:+.2}%", (last - 1.0) * 100.0),
                    egui::FontId::monospace(12.0),
                    self.accent.base(),
                );
            }
        }
    }

    /// Toggle folder at current selection and rebuild the flat list.
    fn toggle_selected_folder(&mut self) {
        if let Some(entry) = self.library_flat.get(self.selected_index) {
            if let FlatEntryKind::Folder { node_path, .. } = &entry.kind {
                let path = node_path.clone();
                library::toggle_folder(&mut self.library_tree, &path);
                self.library_flat = library::flatten_tree(&self.library_tree);
                // Clamp index in case tree shrank
                if self.selected_index >= self.library_flat.len() + 2 {
                    self.selected_index = self.library_flat.len().saturating_sub(1);
                }
            }
        }
    }

    fn draw_library_view(&mut self, ui: &mut egui::Ui) -> u8 {
        let mut action = 0u8; // 0=none, 1=load, 2=rescan, 3=back to deck, 4=toggle folder, 5=settings

        ui.label(
            RichText::new("Library")
                .color(self.accent.bright())
                .size(20.0),
        );
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .enable_scrolling(false)
            .show(ui, |ui| {
                // Fixed entries above the tree
                for (sel, label, act) in [
                    (SEL_BACK, "[Back]", 3u8),
                    (SEL_SETTINGS, "[Settings]", 5),
                    (SEL_RESCAN, "[Rescan]", 2),
                ] {
                    let selected = self.selected_index == sel;
                    let response = ui.selectable_label(
                        selected,
                        RichText::new(label)
                            .color(if selected {
                                self.accent.bright()
                            } else {
                                self.accent.mid()
                            })
                            .size(16.0),
                    );
                    if selected {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    if response.clicked() {
                        self.selected_index = sel;
                    }
                    if response.double_clicked() {
                        action = act;
                    }
                }

                ui.separator();

                // Tree entries
                for (idx, entry) in self.library_flat.iter().enumerate() {
                    let is_selected = idx == self.selected_index;
                    let indent = "  ".repeat(entry.depth);

                    match &entry.kind {
                        FlatEntryKind::Folder { name, open, .. } => {
                            let prefix = if *open { "v " } else { "> " };
                            let text = format!("{}{}{}", indent, prefix, name);
                            let color = if is_selected {
                                self.accent.base()
                            } else {
                                self.accent.dim()
                            };
                            let response = ui.selectable_label(
                                is_selected,
                                RichText::new(&text).color(color).size(16.0),
                            );
                            if is_selected {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }
                            if response.clicked() {
                                self.selected_index = idx;
                            }
                            if response.double_clicked() {
                                self.selected_index = idx;
                                action = 4; // toggle folder
                            }
                        }
                        FlatEntryKind::Track {
                            path, display_name, ..
                        } => {
                            let is_current = self.current_track_path.as_ref() == Some(path);

                            let text = if is_current {
                                format!("{}>> {}", indent, display_name)
                            } else {
                                format!("{}{}", indent, display_name)
                            };

                            let color = if is_current && is_selected {
                                self.accent.bright()
                            } else if is_current {
                                self.accent.base()
                            } else if is_selected {
                                self.accent.light()
                            } else {
                                Color32::from_rgb(180, 170, 150)
                            };

                            let response = ui.selectable_label(
                                is_selected,
                                RichText::new(&text).color(color).size(16.0),
                            );
                            if is_selected {
                                response.scroll_to_me(Some(egui::Align::Center));
                            }

                            if response.clicked() {
                                self.selected_index = idx;
                            }
                            if response.double_clicked() {
                                self.selected_index = idx;
                                if is_current {
                                    action = 3;
                                } else {
                                    action = 1;
                                }
                            }
                        }
                    }
                }
            });

        self.draw_status(ui);

        action
    }

    fn handle_input(&mut self, ctx: &egui::Context) {
        let mut action = 0u8;

        ctx.input(|i| {
            // Rotary encoder scroll (REL_HWHEEL) + mouse wheel + arrow keys
            let scroll = i.scroll_delta.y + i.scroll_delta.x;

            // Scroll inverted: scroll down (negative) = move up in list
            let up = scroll < 0.0 || i.key_pressed(egui::Key::ArrowUp);
            let down = scroll > 0.0 || i.key_pressed(egui::Key::ArrowDown);

            if self.view == View::Library {
                // Navigate library: [Back] → [Settings] → [Rescan] → tree entries...
                if up {
                    if self.selected_index == 0 {
                        self.selected_index = SEL_RESCAN;
                    } else if self.selected_index != SEL_BACK {
                        self.selected_index -= 1;
                    }
                }
                if down {
                    if self.selected_index >= SEL_BACK && self.selected_index < SEL_RESCAN {
                        self.selected_index += 1;
                    } else if self.selected_index == SEL_RESCAN {
                        if !self.library_flat.is_empty() {
                            self.selected_index = 0; // → first entry
                        }
                    } else if self.selected_index + 1 < self.library_flat.len() {
                        self.selected_index += 1;
                    }
                }
            }

            if self.view == View::Settings {
                if self.settings.editing {
                    if down {
                        self.step_accent(1);
                    }
                    if up {
                        self.step_accent(-1);
                    }
                } else {
                    let last = self.settings_rows().len() - 1;
                    if up {
                        self.settings.selected = self.settings.selected.saturating_sub(1);
                    }
                    if down {
                        self.settings.selected = (self.settings.selected + 1).min(last);
                    }
                }
            }

            if self.view == View::Update {
                if let Some(state) = self.update.as_mut() {
                    if matches!(state.phase, UpdatePhase::Confirm)
                        && (scroll != 0.0
                            || i.key_pressed(egui::Key::ArrowUp)
                            || i.key_pressed(egui::Key::ArrowDown))
                    {
                        state.selected = 1 - state.selected;
                    }
                }
            }

            if i.key_pressed(egui::Key::Enter) {
                action = 1; // button pressed
            }
        });

        if action == 1 {
            match self.view {
                View::Update => {
                    let Some(state) = &self.update else {
                        self.view = View::Settings;
                        return;
                    };
                    match state.phase {
                        UpdatePhase::Confirm if state.selected == 0 => self.start_install(),
                        UpdatePhase::Confirm | UpdatePhase::Failed(_) => {
                            self.update = None;
                            self.view = View::Settings;
                        }
                        UpdatePhase::Installing(_) => {}
                    }
                }
                View::Settings => {
                    let rows = self.settings_rows();
                    if let Some(&row) = rows.get(self.settings.selected) {
                        self.activate_setting(row);
                    }
                }
                View::Deck => {
                    // Button in deck view → open library
                    self.view = View::Library;
                }
                View::Library => {
                    if self.selected_index == SEL_BACK {
                        self.view = View::Deck;
                    } else if self.selected_index == SEL_SETTINGS {
                        self.open_settings();
                    } else if self.selected_index == SEL_RESCAN {
                        // Rescan
                        self.rescan_library();
                    } else if let Some(entry) = self.library_flat.get(self.selected_index) {
                        match &entry.kind {
                            FlatEntryKind::Folder { .. } => {
                                self.toggle_selected_folder();
                            }
                            FlatEntryKind::Track { path, .. } => {
                                let is_current = self.current_track_path.as_ref() == Some(path);
                                if is_current {
                                    self.view = View::Deck;
                                } else if self.is_deck_active() {
                                    self.status_message = "Stop the deck first!".to_string();
                                    self.status_timer = 2.0;
                                } else if !self.loading {
                                    self.load_selected_track();
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

impl eframe::App for NeowaxApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(egui::Visuals::dark());

        self.check_loaded_track();
        self.check_update_progress(ctx);
        self.update_from_engine();
        self.handle_input(ctx);

        // Decay status message
        if self.status_timer > 0.0 {
            self.status_timer -= ctx.input(|i| i.predicted_dt as f64);
            if self.status_timer <= 0.0 {
                self.status_message.clear();
            }
        }

        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(Color32::from_rgb(16, 16, 16))
                    .rounding(Rounding::ZERO)
                    .inner_margin(Margin::same(BORDER)),
            )
            .show(ctx, |ui| {
                // Loading progress bar at top
                if self.loading {
                    let progress = self.load_progress.load(Ordering::Relaxed) as f32 / 1000.0;
                    let bar_height = 4.0;
                    let available_width = ui.available_width();
                    let (bar_rect, _) = ui.allocate_exact_size(
                        egui::vec2(available_width, bar_height),
                        egui::Sense::hover(),
                    );
                    let painter = ui.painter();
                    painter.rect_filled(bar_rect, 0.0, Color32::from_rgb(30, 30, 30));
                    if progress > 0.0 {
                        let fill_rect = egui::Rect::from_min_max(
                            bar_rect.min,
                            egui::pos2(
                                bar_rect.left() + available_width * progress,
                                bar_rect.bottom(),
                            ),
                        );
                        painter.rect_filled(fill_rect, 0.0, self.accent.base());
                    }
                    ui.add_space(2.0);
                }

                match self.view {
                    View::Deck => self.draw_deck_view(ui),
                    View::Update => self.draw_update_view(ui),
                    View::Settings => {
                        if let Some((idx, row)) = self.draw_settings_view(ui) {
                            self.settings.selected = idx;
                            match row {
                                // Touch: tapping the color cycles it
                                SettingsRow::Accent => self.step_accent(1),
                                _ => self.activate_setting(row),
                            }
                        }
                    }
                    View::Library => {
                        let lib_action = self.draw_library_view(ui);
                        match lib_action {
                            2 => self.rescan_library(),
                            5 => self.open_settings(),
                            3 => self.view = View::Deck,
                            4 => self.toggle_selected_folder(),
                            1 if !self.is_deck_active() && !self.loading => {
                                self.load_selected_track();
                            }
                            _ => {}
                        }
                    }
                }
            });

        ctx.request_repaint();
    }
}

fn main() -> Result<(), eframe::Error> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    if args.check_args {
        return Ok(());
    }

    let default_device = args.alsa.as_deref().unwrap_or("default");
    let input_device = args
        .input_device
        .as_deref()
        .unwrap_or(default_device)
        .to_string();
    let output_device = args
        .output_device
        .as_deref()
        .unwrap_or(default_device)
        .to_string();

    let (win_w, win_h) = args
        .geometry
        .as_deref()
        .and_then(|g| {
            let parts: Vec<&str> = g.split('x').collect();
            if parts.len() == 2 {
                Some((parts[0].parse::<f32>().ok()?, parts[1].parse::<f32>().ok()?))
            } else {
                None
            }
        })
        .unwrap_or((1024.0, 600.0));

    let scan = library::scan_directory(&args.library);
    tracing::info!("Scanned library in {:?}", args.library);

    let running = Arc::new(AtomicBool::new(true));
    let restart_requested = Arc::new(AtomicBool::new(false));
    let mut engine = Engine::new(input_device, output_device, args.sample_rate);
    if let Some(buf) = args.buffer {
        engine.set_buffer_size(buf);
    }
    if let Some(buf) = args.capture_buffer {
        engine.set_capture_buffer_size(buf);
    }
    let deck_state = engine.state.clone();

    let (capture_handle, playback_handle, track_sender) = engine.run(
        running.clone(),
        None,
        &args.timecode,
        args.speed,
        args.phono,
        args.capture_cpu,
        args.playback_cpu,
    );

    let timecode_resolution = TimecodeDef::find_definition(&args.timecode)
        .map(|def| def.resolution as f64 * args.speed)
        .unwrap_or(1000.0);

    let app = NeowaxApp::new(
        deck_state,
        track_sender,
        args.library.clone(),
        scan,
        args.sample_rate,
        timecode_resolution,
        restart_requested.clone(),
    );

    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/neowax-256.png"))
        .expect("Failed to load app icon");

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([win_w, win_h])
        .with_min_inner_size([400.0, 300.0])
        .with_title("neowax")
        .with_icon(icon);

    if args.no_decor {
        viewport = viewport.with_decorations(false);
    }

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let result = eframe::run_native("neowax", options, Box::new(|_cc| Box::new(app)));

    running.store(false, Ordering::SeqCst);
    capture_handle.join().ok();
    playback_handle.join().ok();

    // After a successful update, replace this process with the new binary.
    // Done after joining the audio threads so ALSA devices are released.
    if restart_requested.load(Ordering::SeqCst) {
        use std::os::unix::process::CommandExt;
        match update::restart_path() {
            Ok(exe) => {
                let err = std::process::Command::new(exe)
                    .args(std::env::args_os().skip(1))
                    .exec();
                tracing::error!("Failed to restart: {}", err);
            }
            Err(e) => tracing::error!("Failed to locate executable: {}", e),
        }
    }

    result
}
