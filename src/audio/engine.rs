use super::audio_devices::{configure_audio_devices, AlsaSettings};
use super::player::{Player, TimecodeState};
use super::track::Track;
use crate::timecode::{TimecodeDef, Timecoder};

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

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
    /// Why the audio isn't running (device missing, wrong choice, ...); `None`
    /// while it works. The engine keeps retrying, see `Engine::spawn`.
    audio_error: Mutex<Option<String>>,
    /// Incremented every time the audio threads (re)start
    pub generation: AtomicU32,
    /// Swap left/right of the captured timecode signal (for crossed cabling)
    pub swap_input: AtomicBool,
    /// Follow the timecode. When off, a loaded track plays by itself from the start.
    pub timecode_enabled: AtomicBool,
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
            audio_error: Mutex::new(None),
            generation: AtomicU32::new(0),
            swap_input: AtomicBool::new(false),
            timecode_enabled: AtomicBool::new(true),
            monitor: Mutex::new(Vec::new()),
            input_peak: [AtomicU32::new(0), AtomicU32::new(0)],
            output_peak: [AtomicU32::new(0), AtomicU32::new(0)],
        }
    }

    pub fn audio_error(&self) -> Option<String> {
        self.audio_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_audio_error(&self, error: Option<String>) {
        *self.audio_error.lock().unwrap_or_else(|e| e.into_inner()) = error;
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

#[derive(Clone)]
pub struct Engine {
    pub state: Arc<DeckState>,
    input_device: String,
    output_device: String,
    sample_rate: u32,
    buffer_size: Option<u32>,
    capture_buffer_size: Option<u32>,
}

/// What the audio threads need to know besides the devices.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub timecode: String,
    pub speed: f64,
    pub phono: bool,
    pub capture_cpu: usize,
    pub playback_cpu: usize,
}

/// Opened and prepared sound devices, ready to hand to the audio threads.
struct OpenedDevices {
    input: alsa::PCM,
    output: alsa::PCM,
    capture_period: usize,
    playback_period: usize,
}

/// How long to wait before trying the sound devices again.
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

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

    /// Start the audio engine on a supervisor thread and return its handle.
    ///
    /// The supervisor opens the sound devices and runs the capture and playback
    /// threads. It never gives up: if a device can't be opened (a USB card that
    /// shows up late, a wrong choice in the settings) the reason is published in
    /// `DeckState::audio_error` and it tries again every few seconds, so the
    /// app stays usable and the audio starts by itself once the device works.
    /// Track commands wait in `track_rx` until then. It ends once `running`
    /// is cleared, after the audio threads have stopped and released the devices.
    pub fn spawn(
        &self,
        running: Arc<AtomicBool>,
        track_rx: mpsc::Receiver<TrackCommand>,
        config: EngineConfig,
    ) -> std::thread::JoinHandle<()> {
        let engine = self.clone();
        std::thread::spawn(move || engine.supervise(running, track_rx, config))
    }

    fn supervise(
        &self,
        running: Arc<AtomicBool>,
        mut track_rx: mpsc::Receiver<TrackCommand>,
        config: EngineConfig,
    ) {
        let state = &self.state;
        let mut timecoder: Option<Timecoder> = None;
        let mut last_error = String::new();

        while running.load(Ordering::Relaxed) {
            // Building the timecode table takes seconds; do it once and keep it
            let tc = match timecoder.take() {
                Some(mut tc) => {
                    tc.reset();
                    tc
                }
                None => match self.build_timecoder(&config) {
                    Ok(tc) => tc,
                    Err(e) => {
                        self.report_failure(&mut last_error, format!("{e:#}"));
                        sleep_while(&running, RETRY_INTERVAL);
                        continue;
                    }
                },
            };

            let devices = match self.open_devices() {
                Ok(devices) => devices,
                Err(e) => {
                    self.report_failure(&mut last_error, format!("{e:#}"));
                    timecoder = Some(tc);
                    sleep_while(&running, RETRY_INTERVAL);
                    continue;
                }
            };

            last_error.clear();
            state.set_audio_error(None);
            state.generation.fetch_add(1, Ordering::SeqCst);
            tracing::info!("Audio started");

            let run = Arc::new(AtomicBool::new(true));
            let (capture, playback) =
                self.start_threads(devices, tc, track_rx, &config, run.clone());

            // Wait until both threads are done; either one ending (a device
            // error) stops the other through `run`.
            while !(capture.is_finished() && playback.is_finished()) {
                if !running.load(Ordering::Relaxed) {
                    run.store(false, Ordering::SeqCst);
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            timecoder = capture.join().ok();
            match playback.join() {
                Ok(rx) => track_rx = rx,
                Err(_) => {
                    // The track channel went down with the thread; nothing to reconnect
                    self.report_failure(&mut last_error, "audio thread crashed".to_string());
                    return;
                }
            }

            if running.load(Ordering::Relaxed) {
                tracing::warn!("Audio stopped, trying again");
                sleep_while(&running, RETRY_INTERVAL);
            }
        }
    }

    /// Publish a failure for the UI; log it only when it changes, not on every retry.
    fn report_failure(&self, last: &mut String, message: String) {
        if *last != message {
            tracing::error!("Audio not running: {}", message);
            *last = message.clone();
        }
        self.state.set_audio_error(Some(message));
    }

    fn build_timecoder(&self, config: &EngineConfig) -> Result<Timecoder> {
        tracing::info!("Building timecode LUT for '{}'...", config.timecode);
        let def = TimecodeDef::find_definition(&config.timecode)
            .ok_or_else(|| anyhow::anyhow!("unknown timecode '{}'", config.timecode))?;
        Ok(Timecoder::new(
            def,
            config.speed,
            self.sample_rate,
            config.phono,
        ))
    }

    fn open_devices(&self) -> Result<OpenedDevices> {
        let alsa_settings = AlsaSettings {
            input_device: self.input_device.clone(),
            output_device: self.output_device.clone(),
            num_input_channels: 2,
            num_output_channels: 2,
            sample_rate: self.sample_rate,
            buffer_size: self.buffer_size,
            capture_buffer_size: self.capture_buffer_size,
            period_size: None,
        };
        let (input, output, _) = configure_audio_devices(&alsa_settings)?;

        let capture_period = input
            .hw_params_current()
            .and_then(|p| p.get_period_size())
            .context("reading the capture period size")? as usize;
        let playback_period = output
            .hw_params_current()
            .and_then(|p| p.get_period_size())
            .context("reading the playback period size")? as usize;

        input.prepare().context("preparing capture")?;
        output.prepare().context("preparing playback")?;
        input.start().context("starting capture")?;

        Ok(OpenedDevices {
            input,
            output,
            capture_period,
            playback_period,
        })
    }

    /// Run capture (timecode decoding) and playback on their own threads. Each
    /// hands back what it was given (the timecoder, the track channel) when it
    /// ends, so a restart loses neither.
    fn start_threads(
        &self,
        devices: OpenedDevices,
        timecoder: Timecoder,
        track_rx: mpsc::Receiver<TrackCommand>,
        config: &EngineConfig,
        run: Arc<AtomicBool>,
    ) -> (
        std::thread::JoinHandle<Timecoder>,
        std::thread::JoinHandle<mpsc::Receiver<TrackCommand>>,
    ) {
        let state = self.state.clone();
        let sample_rate = self.sample_rate;
        let (capture_cpu, playback_cpu) = (config.capture_cpu, config.playback_cpu);
        let tc_safe = timecoder.get_safe();
        let tc_resolution = timecoder.get_resolution();

        // Shared between capture and playback threads
        let snapshot = Arc::new(TimecodeSnapshot::new());
        state.running.store(true, Ordering::SeqCst);

        // Capture thread: reads timecode audio, decodes position+pitch
        let (cap_snapshot, cap_state, cap_run) = (snapshot.clone(), state.clone(), run.clone());
        let capture = std::thread::spawn(move || {
            super::realtime::set_thread_affinity(capture_cpu);
            super::realtime::prioritize_thread();

            let mut timecoder = timecoder;
            if let Err(e) = run_capture_loop(
                devices.input,
                &mut timecoder,
                devices.capture_period,
                &cap_snapshot,
                &cap_state,
                &cap_run,
            ) {
                tracing::error!("Capture thread error: {:#}", e);
                cap_state.set_audio_error(Some(format!("Audio input stopped: {e:#}")));
            }
            cap_run.store(false, Ordering::SeqCst);
            cap_state.running.store(false, Ordering::SeqCst);
            tracing::info!("Capture thread stopped");
            timecoder
        });

        // Playback thread: generates audio from track + timecode snapshot
        let playback = std::thread::spawn(move || {
            super::realtime::set_thread_affinity(playback_cpu);
            super::realtime::prioritize_thread();

            if let Err(e) = run_playback_loop(
                devices.output,
                sample_rate,
                devices.playback_period,
                &track_rx,
                tc_safe,
                tc_resolution,
                &snapshot,
                &state,
                &run,
            ) {
                tracing::error!("Playback thread error: {:#}", e);
                state.set_audio_error(Some(format!("Audio output stopped: {e:#}")));
            }
            run.store(false, Ordering::SeqCst);
            state.running.store(false, Ordering::SeqCst);
            tracing::info!("Playback thread stopped");
            track_rx
        });

        (capture, playback)
    }
}

/// Sleep for `total`, but wake early once `running` is cleared.
fn sleep_while(running: &AtomicBool, total: Duration) {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total && running.load(Ordering::Relaxed) {
        std::thread::sleep(step);
        slept += step;
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
    timecoder: &mut Timecoder,
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
                snapshot.write(timecoder);

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
    track_rx: &mpsc::Receiver<TrackCommand>,
    tc_safe: u32,
    tc_resolution: f64,
    snapshot: &TimecodeSnapshot,
    state: &DeckState,
    running: &AtomicBool,
) -> Result<()> {
    let mut player = Player::new(sample_rate);

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

        player.set_free_run(!state.timecode_enabled.load(Ordering::Relaxed));

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
