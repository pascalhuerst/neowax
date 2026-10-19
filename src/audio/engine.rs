use super::audio_devices::{configure_audio_devices, AlsaSettings};
use super::player::{Player, TimecodeState};
use super::track::Track;
use crate::timecode::{TimecodeDef, Timecoder};

use anyhow::Result;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// Commands sent from the UI to the audio thread
pub enum TrackCommand {
    Load(Arc<Track>),
    Clear,
}

const MONITOR_SIZE: i32 = 128;
/// Update monitor data every N periods (reduces realtime thread overhead)
const MONITOR_UPDATE_INTERVAL: u32 = 8;

/// Lock-free timecoder snapshot: written by capture thread, read by playback thread.
/// Cache-line aligned to avoid false sharing.
#[repr(C, align(64))]
struct TimecodeSnapshot {
    pitch: AtomicU64,
    position: AtomicI32,
    when: AtomicU64,
}

impl TimecodeSnapshot {
    fn new() -> Self {
        Self {
            pitch: AtomicU64::new(0f64.to_bits()),
            position: AtomicI32::new(i32::MIN),
            when: AtomicU64::new(0f64.to_bits()),
        }
    }

    fn write(&self, timecoder: &Timecoder) {
        self.pitch
            .store(timecoder.get_pitch().to_bits(), Ordering::Relaxed);
        match timecoder.get_position() {
            Some((pos, when)) => {
                self.when.store(when.to_bits(), Ordering::Relaxed);
                // Position store last with Release so the reader sees pitch+when
                self.position.store(pos, Ordering::Release);
            }
            None => {
                self.position.store(i32::MIN, Ordering::Release);
            }
        }
    }

    fn read(&self, safe: u32, resolution: f64) -> TimecodeState {
        // Position load with Acquire pairs with the Release store
        let pos = self.position.load(Ordering::Acquire);
        let pitch = f64::from_bits(self.pitch.load(Ordering::Relaxed));
        let when = f64::from_bits(self.when.load(Ordering::Relaxed));
        let position = if pos == i32::MIN {
            None
        } else {
            Some((pos, when))
        };
        TimecodeState {
            pitch,
            position,
            safe,
            resolution,
        }
    }
}

/// Shared state between the realtime audio threads and the main/UI thread.
/// Atomics for scalars, Mutex for monitor buffer (audio thread uses try_lock).
pub struct DeckState {
    /// Elapsed position in track, seconds (f64 bits as u64)
    pub elapsed: AtomicU64,
    /// Effective playback pitch: timecoder pitch * sync correction (f64 bits as u64)
    pub pitch: AtomicU64,
    /// Raw vinyl pitch from timecoder Kalman filter (f64 bits as u64)
    pub vinyl_pitch: AtomicU64,
    /// Raw timecode position (i32::MIN = unknown)
    pub timecode_pos: AtomicI32,
    /// Timecoder valid counter
    pub valid_counter: AtomicU32,
    /// Set by the playback thread on track load/clear; the capture thread resets
    /// the timecoder when it sees this, since the needle is lifted off the
    /// control record while a new track is loaded and the old lock is no longer valid.
    pub reset_timecoder: AtomicBool,
    /// Whether the realtime loop is running
    pub running: AtomicBool,
    /// Swap left/right of the captured timecode signal (for crossed cabling)
    pub swap_input: AtomicBool,
    /// Spinner monitor data (written by audio thread via try_lock)
    pub monitor: Mutex<Vec<u8>>,
    /// Peak sample magnitude (L, R) of captured audio since last taken
    input_peak: [AtomicU32; 2],
    /// Peak sample magnitude (L, R) of played audio since last taken
    output_peak: [AtomicU32; 2],
}

impl Default for DeckState {
    fn default() -> Self {
        Self::new()
    }
}

impl DeckState {
    pub fn new() -> Self {
        Self {
            elapsed: AtomicU64::new(0),
            pitch: AtomicU64::new(0),
            vinyl_pitch: AtomicU64::new(0),
            timecode_pos: AtomicI32::new(i32::MIN),
            valid_counter: AtomicU32::new(0),
            reset_timecoder: AtomicBool::new(false),
            running: AtomicBool::new(false),
            swap_input: AtomicBool::new(false),
            monitor: Mutex::new(Vec::new()),
            input_peak: [AtomicU32::new(0), AtomicU32::new(0)],
            output_peak: [AtomicU32::new(0), AtomicU32::new(0)],
        }
    }

    /// Peak levels since the last call, as `[in L, in R, out L, out R]`
    /// in the range 0.0..=1.0. Resets the peaks.
    pub fn take_peaks(&self) -> [f32; 4] {
        let take = |a: &AtomicU32| a.swap(0, Ordering::Relaxed) as f32 / 32768.0;
        [
            take(&self.input_peak[0]),
            take(&self.input_peak[1]),
            take(&self.output_peak[0]),
            take(&self.output_peak[1]),
        ]
    }

    pub fn get_elapsed(&self) -> f64 {
        f64::from_bits(self.elapsed.load(Ordering::Relaxed))
    }

    pub fn get_pitch(&self) -> f64 {
        f64::from_bits(self.pitch.load(Ordering::Relaxed))
    }

    pub fn get_vinyl_pitch(&self) -> f64 {
        f64::from_bits(self.vinyl_pitch.load(Ordering::Relaxed))
    }

    pub fn get_timecode_pos(&self) -> Option<i32> {
        let v = self.timecode_pos.load(Ordering::Relaxed);
        if v == i32::MIN {
            None
        } else {
            Some(v)
        }
    }
}

pub struct Engine {
    pub state: Arc<DeckState>,
    input_device: String,
    output_device: String,
    sample_rate: u32,
    buffer_size: Option<u32>,
    capture_buffer_size: Option<u32>,
}

impl Engine {
    pub fn new(input_device: String, output_device: String, sample_rate: u32) -> Self {
        Self {
            state: Arc::new(DeckState::new()),
            input_device,
            output_device,
            sample_rate,
            buffer_size: None,
            capture_buffer_size: None,
        }
    }

    pub fn set_buffer_size(&mut self, size: u32) {
        self.buffer_size = Some(size);
    }

    pub fn set_capture_buffer_size(&mut self, size: u32) {
        self.capture_buffer_size = Some(size);
    }

    /// Start the audio engine with separate capture and playback threads.
    ///
    /// Returns handles for both threads and a sender for loading new tracks.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &self,
        running: Arc<AtomicBool>,
        initial_track: Option<Arc<Track>>,
        timecode_name: &str,
        speed: f64,
        phono: bool,
        capture_cpu: usize,
        playback_cpu: usize,
    ) -> (
        std::thread::JoinHandle<()>,
        std::thread::JoinHandle<()>,
        mpsc::Sender<TrackCommand>,
    ) {
        let state = self.state.clone();
        let sample_rate = self.sample_rate;
        let input_device = self.input_device.clone();
        let output_device = self.output_device.clone();
        let timecode_name = timecode_name.to_string();
        let buffer_size = self.buffer_size;
        let capture_buffer_size = self.capture_buffer_size;

        let (track_tx, track_rx) = mpsc::channel::<TrackCommand>();

        // Shared between capture and playback threads
        let snapshot = Arc::new(TimecodeSnapshot::new());

        // Build timecoder on main thread to get constants before spawning
        tracing::info!("Building timecode LUT for '{}'...", timecode_name);
        let def = TimecodeDef::find_definition(&timecode_name)
            .unwrap_or_else(|| panic!("Unknown timecode: {}", timecode_name));
        let timecoder = Timecoder::new(def, speed, sample_rate, phono);
        let tc_safe = timecoder.get_safe();
        let tc_resolution = timecoder.get_resolution();

        // Configure ALSA devices on main thread (avoids race conditions)
        let alsa_settings = AlsaSettings {
            input_device: input_device.clone(),
            output_device: output_device.clone(),
            num_input_channels: 2,
            num_output_channels: 2,
            sample_rate,
            buffer_size,
            capture_buffer_size,
            period_size: None,
        };

        let (input_pcm, output_pcm, _) =
            configure_audio_devices(&alsa_settings).expect("Failed to configure audio devices");

        let capture_period = input_pcm
            .hw_params_current()
            .expect("capture hw params")
            .get_period_size()
            .expect("capture period") as usize;
        let playback_period = output_pcm
            .hw_params_current()
            .expect("playback hw params")
            .get_period_size()
            .expect("playback period") as usize;

        input_pcm.prepare().expect("capture prepare");
        output_pcm.prepare().expect("playback prepare");
        input_pcm.start().expect("capture start");

        state.running.store(true, Ordering::SeqCst);

        // Capture thread: reads timecode audio, decodes position+pitch
        let cap_snapshot = snapshot.clone();
        let cap_state = state.clone();
        let cap_running = running.clone();

        let capture_handle = std::thread::spawn(move || {
            super::realtime::set_thread_affinity(capture_cpu);
            super::realtime::prioritize_thread();

            if let Err(e) = run_capture_loop(
                input_pcm,
                timecoder,
                capture_period,
                &cap_snapshot,
                &cap_state,
                &cap_running,
            ) {
                tracing::error!("Capture thread error: {}", e);
            }
            cap_state.running.store(false, Ordering::SeqCst);
            tracing::info!("Capture thread stopped");
        });

        // Playback thread: generates audio from track + timecode snapshot
        let pb_snapshot = snapshot;
        let pb_state = state;
        let pb_running = running;

        let playback_handle = std::thread::spawn(move || {
            super::realtime::set_thread_affinity(playback_cpu);
            super::realtime::prioritize_thread();

            if let Err(e) = run_playback_loop(
                output_pcm,
                sample_rate,
                playback_period,
                initial_track,
                track_rx,
                tc_safe,
                tc_resolution,
                &pb_snapshot,
                &pb_state,
                &pb_running,
            ) {
                tracing::error!("Playback thread error: {}", e);
            }
            pb_state.running.store(false, Ordering::SeqCst);
            tracing::info!("Playback thread stopped");
        });

        (capture_handle, playback_handle, track_tx)
    }
}

/// Fold the peak magnitude of interleaved stereo samples into `peaks`.
fn publish_peaks(peaks: &[AtomicU32; 2], samples: &[i16]) {
    let (mut l, mut r) = (0u32, 0u32);
    for frame in samples.as_chunks::<2>().0 {
        l = l.max(frame[0].unsigned_abs() as u32);
        r = r.max(frame[1].unsigned_abs() as u32);
    }
    peaks[0].fetch_max(l, Ordering::Relaxed);
    peaks[1].fetch_max(r, Ordering::Relaxed);
}

fn run_capture_loop(
    input_pcm: alsa::PCM,
    mut timecoder: Timecoder,
    period_size: usize,
    snapshot: &TimecodeSnapshot,
    state: &DeckState,
    running: &AtomicBool,
) -> Result<()> {
    timecoder.monitor_init(MONITOR_SIZE).ok();

    let mut capture_buf = vec![0i16; period_size * 2];
    let mut period_counter: u32 = 0;
    let mut swapped = state.swap_input.load(Ordering::Relaxed);

    tracing::info!("Capture loop started, period_size={}", period_size);

    while running.load(Ordering::Relaxed) {
        if state.reset_timecoder.swap(false, Ordering::Relaxed) {
            timecoder.reset();
            tracing::info!("Timecoder reset for new track");
        }

        match input_pcm.io_i16()?.readi(&mut capture_buf) {
            Ok(frames) => {
                let n = frames;
                // Meters show the physical inputs; the swap only affects decoding
                publish_peaks(&state.input_peak, &capture_buf[..n * 2]);

                let swap = state.swap_input.load(Ordering::Relaxed);
                if swap != swapped {
                    swapped = swap;
                    timecoder.reset();
                }
                if swap {
                    for frame in capture_buf[..n * 2].as_chunks_mut::<2>().0 {
                        frame.swap(0, 1);
                    }
                }
                timecoder.submit(&capture_buf[..n * 2]);

                // Publish timecoder state to playback thread
                snapshot.write(&timecoder);

                // Publish timecoder-side DeckState
                state
                    .vinyl_pitch
                    .store(timecoder.get_pitch().to_bits(), Ordering::Relaxed);
                state
                    .valid_counter
                    .store(timecoder.get_valid_counter(), Ordering::Relaxed);

                if let Some((pos, _)) = timecoder.get_position() {
                    state.timecode_pos.store(pos, Ordering::Relaxed);
                }

                // Update monitor data periodically
                period_counter += 1;
                if period_counter.is_multiple_of(MONITOR_UPDATE_INTERVAL) {
                    if let Ok(mut mon) = state.monitor.try_lock() {
                        if let Some(data) = timecoder.get_monitor() {
                            *mon = data;
                        }
                    }
                }
            }
            Err(e) if e.errno() == libc::EPIPE => {
                tracing::warn!("Capture overrun, recovering");
                input_pcm.prepare()?;
                input_pcm.start()?;
            }
            Err(e) => return Err(e.into()),
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_playback_loop(
    output_pcm: alsa::PCM,
    sample_rate: u32,
    period_size: usize,
    initial_track: Option<Arc<Track>>,
    track_rx: mpsc::Receiver<TrackCommand>,
    tc_safe: u32,
    tc_resolution: f64,
    snapshot: &TimecodeSnapshot,
    state: &DeckState,
    running: &AtomicBool,
) -> Result<()> {
    let mut player = Player::new(sample_rate);
    if let Some(t) = initial_track {
        player.set_track(t);
    }

    let mut playback_buf = vec![0i16; period_size * 2];

    tracing::info!("Playback loop started, period_size={}", period_size);

    while running.load(Ordering::Relaxed) {
        // Check for track commands (non-blocking)
        if let Ok(cmd) = track_rx.try_recv() {
            match cmd {
                TrackCommand::Load(track) => {
                    player.set_track(track);
                    state.reset_timecoder.store(true, Ordering::Relaxed);
                    tracing::info!("Track loaded into player");
                }
                TrackCommand::Clear => {
                    player.clear_track();
                    state.reset_timecoder.store(true, Ordering::Relaxed);
                    tracing::info!("Track cleared");
                }
            }
        }

        // Read latest timecoder state
        let tc_state = snapshot.read(tc_safe, tc_resolution);

        // Generate audio
        player.collect_with_state(&mut playback_buf, period_size, &tc_state);

        publish_peaks(&state.output_peak, &playback_buf);

        // Write output
        match output_pcm.io_i16()?.writei(&playback_buf) {
            Ok(_) => {}
            Err(e) if e.errno() == libc::EPIPE => {
                tracing::warn!("Playback underrun, recovering");
                output_pcm.prepare()?;
            }
            Err(e) => return Err(e.into()),
        }

        // Publish player-side DeckState
        state
            .elapsed
            .store(player.get_elapsed().to_bits(), Ordering::Relaxed);
        state
            .pitch
            .store(player.get_effective_pitch().to_bits(), Ordering::Relaxed);
    }

    Ok(())
}
