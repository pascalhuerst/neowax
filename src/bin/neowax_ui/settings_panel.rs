//! The on-device settings panel.
//!
//! Built for a single clickable encoder: scrolling moves the selection, a
//! click activates the row. Rows come in a few kinds:
//!
//! * **Action**: a click runs it (Back, Install update, Apply & restart).
//! * **Group**: a click folds or unfolds the rows beneath it.
//! * **Toggle**: a click flips it.
//! * **Choice**: a click starts editing, the encoder cycles through the
//!   values and another click confirms. A tap on a touch screen just cycles
//!   to the next value.
//! * **Display**: not selectable (level meters, pitch graph).
//!
//! Adding a setting means a `SettingId`, its label/value/step functions below
//! and a place in `settings_rows`. Settings forced by a command-line flag are
//! shown greyed out and can't be changed.

use super::*;
use neowax::audio::list_alsa_devices;
use neowax::settings::{
    library_candidates, CAPTURE_BUFFERS, PLAYBACK_BUFFERS, SAMPLE_RATES, SPEEDS,
};
use neowax::timecode::TimecodeDef;
use std::collections::HashSet;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SettingId {
    Back,
    Apply,
    /// The single available package
    Update,
    /// Fold-out listing several available packages
    GroupUpdates,
    /// A newer release found on GitHub
    OnlineUpdate,
    OnlineUpdates,
    OnlineCheck,
    /// One entry of that list (index into `available_updates`)
    UpdateVersion(usize),
    Meters,
    SwapInput,
    TimecodeEnabled,
    GroupAudio,
    GroupSystem,
    GroupDisplay,
    GroupPitch,
    PitchPlot,
    InputDevice,
    OutputDevice,
    Library,
    Timecode,
    Speed,
    Phono,
    SampleRate,
    Buffer,
    CaptureBuffer,
    CaptureCpu,
    PlaybackCpu,
    Accent,
}

/// Settings that are only read at startup.
const RESTART_SETTINGS: &[SettingId] = &[
    SettingId::InputDevice,
    SettingId::OutputDevice,
    SettingId::Library,
    SettingId::Timecode,
    SettingId::Speed,
    SettingId::Phono,
    SettingId::SampleRate,
    SettingId::Buffer,
    SettingId::CaptureBuffer,
    SettingId::CaptureCpu,
    SettingId::PlaybackCpu,
];

const AUDIO_ROWS: &[SettingId] = &[
    SettingId::Meters,
    SettingId::TimecodeEnabled,
    SettingId::SwapInput,
    SettingId::InputDevice,
    SettingId::OutputDevice,
    SettingId::Timecode,
    SettingId::Speed,
    SettingId::Phono,
    SettingId::SampleRate,
    SettingId::Buffer,
    SettingId::CaptureBuffer,
    SettingId::CaptureCpu,
    SettingId::PlaybackCpu,
];

#[derive(Clone, Copy, PartialEq)]
enum RowKind {
    Action,
    Group,
    Toggle,
    Choice,
    Display,
}

#[derive(Clone, Copy, PartialEq)]
enum RowStyle {
    Normal,
    /// Forced by a command-line flag
    Locked,
    /// Needs the user's attention
    Warning,
}

struct Row {
    id: SettingId,
    kind: RowKind,
    style: RowStyle,
    depth: usize,
    label: String,
}

impl Row {
    fn new(id: SettingId, kind: RowKind, label: &str) -> Self {
        Self {
            id,
            kind,
            style: RowStyle::Normal,
            depth: 0,
            label: label.to_string(),
        }
    }

    fn selectable(&self) -> bool {
        self.kind != RowKind::Display
    }
}

pub struct SettingsState {
    /// Index among the selectable rows
    selected: usize,
    open: HashSet<SettingId>,
    /// A choice row is being edited: the encoder changes the value, click confirms
    pub editing: bool,
    /// View to return to on [Back]
    return_to: View,
}

impl SettingsState {
    pub fn new() -> Self {
        Self {
            selected: 0,
            // Audio holds the level meters, so it starts unfolded
            open: HashSet::from([SettingId::GroupAudio]),
            editing: false,
            return_to: View::Deck,
        }
    }
}

/// Like `cycle`, for owned strings. A current value that isn't in `list`
/// (hand-edited file, flag) is kept as the first entry so it can be returned to.
fn cycle_string(mut list: Vec<String>, current: &str, delta: isize) -> String {
    if !list.iter().any(|x| x == current) {
        list.insert(0, current.to_string());
    }
    let at = list.iter().position(|x| x == current).unwrap_or(0) as isize;
    list.swap_remove((at + delta).rem_euclid(list.len() as isize) as usize)
}

/// Next (or previous) entry of `list` after `current`, wrapping around.
fn cycle<T: PartialEq + Copy>(list: &[T], current: &T, delta: isize) -> T {
    let at = list.iter().position(|x| x == current).unwrap_or(0) as isize;
    list[(at + delta).rem_euclid(list.len() as isize) as usize]
}

fn setting_key(id: SettingId) -> Option<&'static str> {
    Some(match id {
        SettingId::SwapInput => "swap_input",
        SettingId::TimecodeEnabled => "timecode_enabled",
        SettingId::InputDevice => "input_device",
        SettingId::OutputDevice => "output_device",
        SettingId::Library => "library",
        SettingId::Timecode => "timecode",
        SettingId::Speed => "speed",
        SettingId::Phono => "phono",
        SettingId::SampleRate => "sample_rate",
        SettingId::Buffer => "buffer",
        SettingId::CaptureBuffer => "capture_buffer",
        SettingId::CaptureCpu => "capture_cpu",
        SettingId::PlaybackCpu => "playback_cpu",
        _ => return None,
    })
}

fn on_off(on: bool) -> &'static str {
    if on {
        "on"
    } else {
        "off"
    }
}

impl NeowaxApp {
    pub(super) fn open_settings(&mut self) {
        self.settings.return_to = self.view;
        self.settings.selected = 0;
        self.settings.editing = false;
        self.view = View::Settings;
    }

    // ---- accent color -------------------------------------------------

    /// Show an accent color without touching the saved settings.
    pub(super) fn apply_accent(&mut self, index: usize) {
        self.accent_index = index % ACCENT_PRESETS.len();
        self.accent = Accent::from_preset(self.accent_index);
        self.spinner.set_accent(self.accent);
        self.waveform.set_accent(self.accent);
    }

    /// The user picked an accent color: show it and queue it for saving.
    fn set_accent(&mut self, index: usize) {
        self.apply_accent(index);
        self.config.accent = ACCENT_PRESETS[self.accent_index].0.to_string();
        self.config_dirty = true;
    }

    // ---- persistence --------------------------------------------------

    /// Write queued setting changes. Held back while a value is being edited
    /// so scrolling through values doesn't write to the SD card on every tick.
    pub(super) fn save_config_if_dirty(&mut self) {
        if !self.config_dirty || self.settings.editing {
            return;
        }
        self.config_dirty = false;
        let Some(path) = &self.config_path else {
            return;
        };
        if let Err(e) = self.config.save(path) {
            tracing::error!("Failed to save settings: {:#}", e);
            self.flash("Could not save settings");
        }
    }

    // ---- setting values -----------------------------------------------

    fn is_locked(&self, id: SettingId) -> bool {
        setting_key(id).is_some_and(|k| self.locked.contains(k))
    }

    /// The values to show: what the process runs with if a command-line flag
    /// forces the setting, otherwise what is saved.
    fn shown(&self, id: SettingId) -> &Settings {
        if self.is_locked(id) {
            &self.startup
        } else {
            &self.config
        }
    }

    fn label(id: SettingId) -> &'static str {
        match id {
            SettingId::Back => "[Back]",
            SettingId::Apply => "Apply changes & restart",
            SettingId::Update | SettingId::UpdateVersion(_) => "Install update",
            SettingId::GroupUpdates => "Updates",
            SettingId::OnlineUpdate | SettingId::OnlineCheck => "",
            SettingId::OnlineUpdates => "Online update check",
            SettingId::SwapInput => "Swap input L/R",
            SettingId::TimecodeEnabled => "Timecode control",
            SettingId::GroupAudio => "Audio",
            SettingId::GroupSystem => "System",
            SettingId::GroupDisplay => "Display",
            SettingId::GroupPitch => "Pitch",
            SettingId::InputDevice => "Input device",
            SettingId::OutputDevice => "Output device",
            SettingId::Library => "Music library",
            SettingId::Timecode => "Timecode",
            SettingId::Speed => "Timecode speed",
            SettingId::Phono => "Phono input",
            SettingId::SampleRate => "Sample rate",
            SettingId::Buffer => "Playback buffer",
            SettingId::CaptureBuffer => "Capture buffer",
            SettingId::CaptureCpu => "Capture CPU",
            SettingId::PlaybackCpu => "Playback CPU",
            SettingId::Accent => "Accent color",
            SettingId::Meters | SettingId::PitchPlot => "",
        }
    }

    fn value_text(&self, id: SettingId) -> String {
        let s = self.shown(id);
        match id {
            SettingId::Accent => ACCENT_PRESETS[self.accent_index].0.to_string(),
            SettingId::SwapInput => {
                on_off(self.deck_state.swap_input.load(Ordering::Relaxed)).to_string()
            }
            SettingId::TimecodeEnabled => {
                on_off(self.deck_state.timecode_enabled.load(Ordering::Relaxed)).to_string()
            }
            SettingId::InputDevice => s.input_device.clone(),
            SettingId::OutputDevice => s.output_device.clone(),
            SettingId::OnlineUpdates => on_off(s.online_updates).to_string(),
            SettingId::Library => s
                .library
                .as_ref()
                .map_or_else(|| "(none)".to_string(), |p| p.display().to_string()),
            SettingId::Timecode => s.timecode.clone(),
            SettingId::Speed => SPEEDS
                .iter()
                .find(|(_, v)| (v - s.speed).abs() < 0.01)
                .map_or_else(|| format!("{:.2}x", s.speed), |(n, _)| n.to_string()),
            SettingId::Phono => on_off(s.phono).to_string(),
            SettingId::SampleRate => format!("{} Hz", s.sample_rate),
            SettingId::Buffer => format!("{} frames", s.buffer),
            SettingId::CaptureBuffer => format!("{} frames", s.capture_buffer),
            SettingId::CaptureCpu => format!("core {}", s.capture_cpu),
            SettingId::PlaybackCpu => format!("core {}", s.playback_cpu),
            _ => String::new(),
        }
    }

    /// Has the saved value of a restart-only setting changed since startup?
    fn differs_from_startup(&self, id: SettingId) -> bool {
        let (c, s) = (&self.config, &self.startup);
        !self.is_locked(id)
            && match id {
                SettingId::InputDevice => c.input_device != s.input_device,
                SettingId::OutputDevice => c.output_device != s.output_device,
                SettingId::Library => c.library != s.library,
                SettingId::Timecode => c.timecode != s.timecode,
                SettingId::Speed => (c.speed - s.speed).abs() > 1e-6,
                SettingId::Phono => c.phono != s.phono,
                SettingId::SampleRate => c.sample_rate != s.sample_rate,
                SettingId::Buffer => c.buffer != s.buffer,
                SettingId::CaptureBuffer => c.capture_buffer != s.capture_buffer,
                SettingId::CaptureCpu => c.capture_cpu != s.capture_cpu,
                SettingId::PlaybackCpu => c.playback_cpu != s.playback_cpu,
                _ => false,
            }
    }

    fn needs_restart(&self) -> bool {
        RESTART_SETTINGS
            .iter()
            .any(|&id| self.differs_from_startup(id))
    }

    /// Change a choice setting by `delta` entries.
    fn step_setting(&mut self, id: SettingId, delta: isize) {
        let cpus: Vec<usize> =
            (0..std::thread::available_parallelism().map_or(1, |n| n.get())).collect();
        let speeds: Vec<f64> = SPEEDS.iter().map(|(_, v)| *v).collect();
        let c = &mut self.config;
        match id {
            SettingId::Accent => {
                let n = ACCENT_PRESETS.len() as isize;
                self.set_accent((self.accent_index as isize + delta).rem_euclid(n) as usize);
                return;
            }
            SettingId::InputDevice => {
                c.input_device = cycle_string(list_alsa_devices(), &c.input_device, delta)
            }
            SettingId::OutputDevice => {
                c.output_device = cycle_string(list_alsa_devices(), &c.output_device, delta)
            }
            SettingId::Library => {
                let candidates = library_candidates()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect();
                let current = c
                    .library
                    .as_ref()
                    .map_or(String::new(), |p| p.display().to_string());
                c.library = Some(PathBuf::from(cycle_string(candidates, &current, delta)));
            }
            SettingId::Timecode => {
                let next = cycle(TimecodeDef::NAMES, &c.timecode.as_str(), delta);
                c.timecode = next.to_string();
            }
            SettingId::Speed => {
                let at = speeds.iter().position(|v| (v - c.speed).abs() < 0.01);
                let current = at.map_or(speeds[0], |i| speeds[i]);
                c.speed = cycle(&speeds, &current, delta);
            }
            SettingId::SampleRate => c.sample_rate = cycle(SAMPLE_RATES, &c.sample_rate, delta),
            SettingId::Buffer => c.buffer = cycle(PLAYBACK_BUFFERS, &c.buffer, delta),
            SettingId::CaptureBuffer => {
                c.capture_buffer = cycle(CAPTURE_BUFFERS, &c.capture_buffer, delta)
            }
            SettingId::CaptureCpu => c.capture_cpu = cycle(&cpus, &c.capture_cpu, delta),
            SettingId::PlaybackCpu => c.playback_cpu = cycle(&cpus, &c.playback_cpu, delta),
            _ => return,
        }
        self.config_dirty = true;
    }

    fn toggle_setting(&mut self, id: SettingId) {
        match id {
            SettingId::SwapInput => {
                let swapped = !self.deck_state.swap_input.load(Ordering::Relaxed);
                self.deck_state.swap_input.store(swapped, Ordering::Relaxed);
                self.config.swap_input = swapped;
            }
            SettingId::Phono => self.config.phono = !self.config.phono,
            SettingId::OnlineUpdates => {
                self.config.online_updates = !self.config.online_updates;
                // Turning it on checks right away; turning it off forgets the result
                self.online = OnlineState::Idle;
                self.start_online_check();
            }
            SettingId::TimecodeEnabled => {
                let enabled = !self.deck_state.timecode_enabled.load(Ordering::Relaxed);
                self.deck_state
                    .timecode_enabled
                    .store(enabled, Ordering::Relaxed);
                self.config.timecode_enabled = enabled;
                // The two modes position the track differently: start clean
                self.clear_deck();
            }
            _ => return,
        }
        self.config_dirty = true;
    }

    // ---- rows ---------------------------------------------------------

    fn settings_rows(&self) -> Vec<Row> {
        let mut rows = vec![Row::new(SettingId::Back, RowKind::Action, "[Back]")];

        if self.needs_restart() {
            let mut row = Row::new(
                SettingId::Apply,
                RowKind::Action,
                Self::label(SettingId::Apply),
            );
            row.style = RowStyle::Warning;
            rows.push(row);
        }
        if self.config.online_updates {
            match &self.online {
                OnlineState::Available(release) => rows.push(Row::new(
                    SettingId::OnlineUpdate,
                    RowKind::Action,
                    &format!("Download update: {} (online)", release.version),
                )),
                OnlineState::Downloading(release, _) => rows.push(Row::new(
                    SettingId::OnlineUpdate,
                    RowKind::Action,
                    &format!("Downloading {}...", release.version),
                )),
                _ => {}
            }
        }

        match self.available_updates.as_slice() {
            [] => {}
            [only] => rows.push(Row::new(
                SettingId::Update,
                RowKind::Action,
                &Self::update_text(only, false),
            )),
            several => {
                let label = format!("Updates ({} versions)", several.len());
                rows.push(Row::new(SettingId::GroupUpdates, RowKind::Group, &label));
                if self.settings.open.contains(&SettingId::GroupUpdates) {
                    for (i, candidate) in several.iter().enumerate() {
                        let mut row = Row::new(
                            SettingId::UpdateVersion(i),
                            RowKind::Action,
                            &Self::update_text(candidate, true),
                        );
                        row.depth = 1;
                        rows.push(row);
                    }
                }
            }
        }

        // Crossed channels are worth interrupting for, even with Audio folded
        if let Some(warning) = self.reverse_warning_row() {
            rows.push(warning);
        }

        let groups: [(SettingId, &[SettingId]); 4] = [
            (SettingId::GroupAudio, AUDIO_ROWS),
            (
                SettingId::GroupSystem,
                &[
                    SettingId::Library,
                    SettingId::OnlineUpdates,
                    SettingId::OnlineCheck,
                ],
            ),
            (SettingId::GroupDisplay, &[SettingId::Accent]),
            (SettingId::GroupPitch, &[SettingId::PitchPlot]),
        ];
        for (group, children) in groups {
            rows.push(Row::new(group, RowKind::Group, Self::label(group)));
            if self.settings.open.contains(&group) {
                for &id in children {
                    if id == SettingId::OnlineCheck && !self.config.online_updates {
                        continue;
                    }
                    let kind = match id {
                        SettingId::Phono
                        | SettingId::SwapInput
                        | SettingId::TimecodeEnabled
                        | SettingId::OnlineUpdates => RowKind::Toggle,
                        SettingId::OnlineCheck => RowKind::Action,
                        SettingId::PitchPlot | SettingId::Meters => RowKind::Display,
                        _ => RowKind::Choice,
                    };
                    let label = if id == SettingId::OnlineCheck {
                        self.online_check_text()
                    } else {
                        Self::label(id).to_string()
                    };
                    let mut row = Row::new(id, kind, &label);
                    row.depth = 1;
                    if self.is_locked(id) {
                        row.style = RowStyle::Locked;
                    }
                    rows.push(row);
                }
            }
        }
        rows
    }

    /// Shown while the timecode runs backwards and the input isn't swapped yet.
    fn reverse_warning_row(&self) -> Option<Row> {
        let id = SettingId::SwapInput;
        let swapped = self.deck_state.swap_input.load(Ordering::Relaxed);
        let timecode_on = self.deck_state.timecode_enabled.load(Ordering::Relaxed);
        if !timecode_on || !self.reverse.detected() || swapped || self.is_locked(id) {
            return None;
        }
        let mut row = Row::new(
            id,
            RowKind::Action,
            "! Timecode runs backwards: swap input L/R",
        );
        row.style = RowStyle::Warning;
        Some(row)
    }

    fn online_check_text(&self) -> String {
        match &self.online {
            OnlineState::Idle => "Check for updates now".to_string(),
            OnlineState::Checking(_) => "Checking for updates...".to_string(),
            OnlineState::UpToDate => "Up to date (check again)".to_string(),
            OnlineState::Available(_) => "Update available (check again)".to_string(),
            OnlineState::Failed(e) => format!("Check failed: {e} (retry)"),
            OnlineState::Downloading(..) => "Downloading...".to_string(),
        }
    }

    fn update_text(candidate: &UpdateCandidate, in_list: bool) -> String {
        let older = candidate.ordering == VersionOrdering::Less;
        let version = &candidate.info.version;
        match (in_list, older) {
            (false, false) => format!("Install update: {version}"),
            (false, true) => format!("Install older version: {version}"),
            (true, false) => format!("{version} (newer)"),
            (true, true) => format!("{version} (older)"),
        }
    }

    fn selectable_rows(&self) -> Vec<Row> {
        let mut rows = self.settings_rows();
        rows.retain(Row::selectable);
        rows
    }

    fn row_text(&self, row: &Row, selected: bool) -> String {
        let mut text = match row.kind {
            RowKind::Action | RowKind::Display => row.label.clone(),
            RowKind::Group => {
                let fold = if self.settings.open.contains(&row.id) {
                    "v"
                } else {
                    ">"
                };
                format!("{fold} {}", row.label)
            }
            RowKind::Toggle => format!("{}: {}", row.label, self.value_text(row.id)),
            RowKind::Choice => {
                if selected && self.settings.editing {
                    format!("{}: < {} >", row.label, self.value_text(row.id))
                } else {
                    format!("{}: {}", row.label, self.value_text(row.id))
                }
            }
        };
        if row.style == RowStyle::Locked {
            text.push_str(" (command line)");
        } else if self.differs_from_startup(row.id) {
            text.push_str(" *");
        }
        text
    }

    // ---- input --------------------------------------------------------

    /// Encoder scrolled while no value is being edited.
    pub(super) fn move_settings_selection(&mut self, up: bool, down: bool) {
        let last = self.selectable_rows().len().saturating_sub(1);
        if up {
            self.settings.selected = self.settings.selected.saturating_sub(1);
        }
        if down {
            self.settings.selected = (self.settings.selected + 1).min(last);
        }
    }

    /// Encoder scrolled while a choice is being edited.
    pub(super) fn step_selected_setting(&mut self, delta: isize) {
        if let Some(row) = self.selectable_rows().get(self.settings.selected) {
            self.step_setting(row.id, delta);
        }
    }

    /// Keep the selection valid when rows come and go (groups fold, warnings clear).
    pub(super) fn clamp_settings_selection(&mut self) {
        let last = self.selectable_rows().len().saturating_sub(1);
        self.settings.selected = self.settings.selected.min(last);
    }

    /// Encoder clicked.
    pub(super) fn activate_selected_setting(&mut self) {
        let rows = self.selectable_rows();
        if let Some(row) = rows.get(self.settings.selected) {
            self.activate_setting(row);
        }
    }

    /// A row was tapped on the touch screen.
    pub(super) fn tap_setting(&mut self, index: usize) {
        self.settings.selected = index;
        self.settings.editing = false;
        let rows = self.selectable_rows();
        let Some(row) = rows.get(index) else {
            return;
        };
        if row.kind == RowKind::Choice && row.style != RowStyle::Locked {
            self.step_setting(row.id, 1);
        } else {
            self.activate_setting(row);
        }
    }

    fn activate_setting(&mut self, row: &Row) {
        if row.style == RowStyle::Locked {
            self.flash("Set on the command line");
            return;
        }
        match row.kind {
            RowKind::Group => {
                if !self.settings.open.remove(&row.id) {
                    self.settings.open.insert(row.id);
                }
            }
            RowKind::Choice => self.settings.editing = !self.settings.editing,
            RowKind::Toggle => self.toggle_setting(row.id),
            RowKind::Action | RowKind::Display => self.run_action(row.id),
        }
    }

    fn run_action(&mut self, id: SettingId) {
        match id {
            SettingId::Back => {
                self.settings.editing = false;
                self.view = self.settings.return_to;
            }
            SettingId::SwapInput => self.toggle_setting(id),
            SettingId::Update | SettingId::UpdateVersion(_) => {
                let index = match id {
                    SettingId::UpdateVersion(i) => i,
                    _ => 0,
                };
                if self.is_deck_active() {
                    self.flash("Stop the deck first!");
                } else if let Some(candidate) = self.available_updates.get(index).cloned() {
                    self.begin_update(candidate);
                }
            }
            SettingId::OnlineCheck => self.start_online_check(),
            SettingId::OnlineUpdate => {
                if self.is_deck_active() {
                    self.flash("Stop the deck first!");
                } else {
                    self.start_online_download();
                }
            }
            SettingId::Apply => {
                if self.is_deck_active() {
                    self.flash("Stop the deck first!");
                } else {
                    // The settings are already saved; restarting picks them up
                    self.save_config_if_dirty();
                    self.restart_requested.store(true, Ordering::SeqCst);
                    self.close_requested = true;
                }
            }
            _ => {}
        }
    }

    // ---- drawing ------------------------------------------------------

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
                    draw_peak_bar(painter, track, peak, self.accent.base(), CLIP_COLOR);
                }
                ui.add_space(SPACER);
            }
        });
    }

    pub(super) fn draw_status(&self, ui: &mut egui::Ui) {
        if !self.status_message.is_empty() {
            ui.add_space(SPACER);
            ui.label(
                RichText::new(&self.status_message)
                    .color(Color32::from_rgb(200, 80, 30))
                    .size(16.0),
            );
        }
    }

    /// Returns the index (among selectable rows) of a row that was tapped.
    pub(super) fn draw_settings_view(&self, ui: &mut egui::Ui) -> Option<usize> {
        let rows = self.settings_rows();
        let mut clicked = None;
        let mut index = 0;

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
                for row in &rows {
                    match row.id {
                        SettingId::Meters => {
                            self.draw_meters(ui);
                            ui.add_space(SPACER);
                            continue;
                        }
                        SettingId::PitchPlot => {
                            self.draw_pitch_plot(ui, 120.0);
                            ui.add_space(4.0);
                            continue;
                        }
                        _ => {}
                    }

                    let this = index;
                    index += 1;
                    let selected = this == self.settings.selected;
                    let color = match row.style {
                        RowStyle::Locked => Color32::from_rgb(110, 110, 110),
                        RowStyle::Warning => Color32::from_rgb(200, 80, 30),
                        RowStyle::Normal if selected => self.accent.bright(),
                        RowStyle::Normal => self.accent.mid(),
                    };

                    ui.horizontal(|ui| {
                        ui.add_space(row.depth as f32 * 16.0);
                        let response = ui.selectable_label(
                            selected,
                            RichText::new(self.row_text(row, selected))
                                .color(color)
                                .size(16.0),
                        );
                        if row.id == SettingId::Accent {
                            let (rect, _) = ui
                                .allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                            ui.painter().rect_filled(rect, 3.0, self.accent.base());
                        }
                        if selected {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if response.clicked() {
                            clicked = Some(this);
                        }
                    });
                }

                ui.separator();
                let info_color = Color32::from_rgb(180, 170, 150);
                ui.label(
                    RichText::new(format!("neowax {}", env!("CARGO_PKG_VERSION")))
                        .color(info_color)
                        .size(14.0),
                );
                ui.label(RichText::new(&self.os_version).color(info_color).size(14.0));
                if self.needs_restart() {
                    ui.label(
                        RichText::new("* applied after restart")
                            .color(info_color)
                            .size(14.0),
                    );
                }
            });

        self.draw_status(ui);
        clicked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_wraps_in_both_directions() {
        let list = [1, 2, 3];
        assert_eq!(cycle(&list, &3, 1), 1);
        assert_eq!(cycle(&list, &1, -1), 3);
        assert_eq!(cycle(&list, &2, 1), 3);
    }

    #[test]
    fn cycle_with_unknown_current_starts_at_the_front() {
        assert_eq!(cycle(&[10, 20], &99, 1), 20);
    }

    #[test]
    fn cycle_string_keeps_a_custom_value_reachable() {
        let list = vec!["default".to_string(), "plughw:card".to_string()];
        // A value that isn't in the list is offered first, so stepping
        // forward leaves it ...
        assert_eq!(cycle_string(list.clone(), "custom", 1), "default");
        // ... and stepping back from the first real entry returns to it
        let with_custom = vec!["custom".to_string(), "default".to_string()];
        assert_eq!(cycle_string(with_custom, "default", -1), "custom");
    }

    #[test]
    fn cycle_string_wraps() {
        let list = vec!["a".to_string(), "b".to_string()];
        assert_eq!(cycle_string(list.clone(), "b", 1), "a");
        assert_eq!(cycle_string(list, "a", -1), "b");
    }
}
