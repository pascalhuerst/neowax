use super::audio_devices::{configure_audio_devices, AlsaSettings};
use super::player::Player;
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

/// Shared state between the realtime audio thread and the main/UI thread.
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
    /// Whether the realtime loop is running
    pub running: AtomicBool,
    /// Spinner monitor data (written by audio thread via try_lock)
    pub monitor: Mutex<Vec<u8>>,
}

impl DeckState {
    pub fn new() -> Self {
        Self {
            elapsed: AtomicU64::new(0),
            pitch: AtomicU64::new(0),
            vinyl_pitch: AtomicU64::new(0),
            timecode_pos: AtomicI32::new(i32::MIN),
            valid_counter: AtomicU32::new(0),
            running: AtomicBool::new(false),
            monitor: Mutex::new(Vec::new()),
        }
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
}

impl Engine {
    pub fn new(input_device: String, output_device: String, sample_rate: u32) -> Self {
        Self {
            state: Arc::new(DeckState::new()),
            input_device,
            output_device,
            sample_rate,
            buffer_size: None,
        }
    }

    pub fn set_buffer_size(&mut self, size: u32) {
        self.buffer_size = Some(size);
    }

    /// Start the audio engine in a dedicated realtime thread.
    ///
    /// Returns the thread handle and a sender for loading new tracks at runtime.
    pub fn run(
        &self,
        running: Arc<AtomicBool>,
        initial_track: Option<Arc<Track>>,
        timecode_name: &str,
        speed: f64,
        phono: bool,
    ) -> (std::thread::JoinHandle<()>, mpsc::Sender<TrackCommand>) {
        let state = self.state.clone();
        let sample_rate = self.sample_rate;
        let input_device = self.input_device.clone();
        let output_device = self.output_device.clone();
        let timecode_name = timecode_name.to_string();
        let buffer_size = self.buffer_size;

        let (track_tx, track_rx) = mpsc::channel::<TrackCommand>();

        let handle = std::thread::spawn(move || {
            super::realtime::set_thread_affinity(3);
            super::realtime::prioritize_thread();

            let result = run_realtime_loop(
                &input_device,
                &output_device,
                sample_rate,
                &timecode_name,
                speed,
                phono,
                buffer_size,
                initial_track,
                track_rx,
                &state,
                &running,
            );

            if let Err(e) = result {
                tracing::error!("Audio engine error: {}", e);
            }
            state.running.store(false, Ordering::SeqCst);
            tracing::info!("Audio engine stopped");
        });

        (handle, track_tx)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_realtime_loop(
    input_device: &str,
    output_device: &str,
    sample_rate: u32,
    timecode_name: &str,
    speed: f64,
    phono: bool,
    buffer_size: Option<u32>,
    initial_track: Option<Arc<Track>>,
    track_rx: mpsc::Receiver<TrackCommand>,
    state: &DeckState,
    running: &AtomicBool,
) -> Result<()> {
    let alsa_settings = AlsaSettings {
        input_device: input_device.to_string(),
        output_device: output_device.to_string(),
        num_input_channels: 2,
        num_output_channels: 2,
        sample_rate,
        buffer_size,
        period_size: None,
    };

    let (input_pcm, output_pcm, _) = configure_audio_devices(&alsa_settings)?;

    let period_size = input_pcm.hw_params_current()?.get_period_size()? as usize;
    let playback_buffer = output_pcm.hw_params_current()?.get_buffer_size()? as usize;

    input_pcm.prepare()?;
    output_pcm.prepare()?;

    // Start capture immediately
    input_pcm.start()?;

    // Initialize timecoder
    tracing::info!("Building timecode LUT for '{}'...", timecode_name);
    let def = TimecodeDef::find_definition(timecode_name)
        .ok_or_else(|| anyhow::anyhow!("Unknown timecode: {}", timecode_name))?;
    let mut timecoder = Timecoder::new(def, speed, sample_rate, phono);
    timecoder.monitor_init(MONITOR_SIZE).ok();

    // Initialize player
    let mut player = Player::new(sample_rate);
    if let Some(t) = initial_track {
        player.set_track(t);
    }

    state.running.store(true, Ordering::SeqCst);

    let mut capture_buf = vec![0i16; period_size * 2];
    let mut playback_buf = vec![0i16; period_size * 2];
    let mut period_counter: u32 = 0;

    tracing::info!(
        "Realtime loop started, period_size={}, playback_buffer={}",
        period_size,
        playback_buffer
    );

    while running.load(Ordering::SeqCst) {
        // Check for track commands (non-blocking)
        if let Ok(cmd) = track_rx.try_recv() {
            match cmd {
                TrackCommand::Load(track) => {
                    player.set_track(track);
                    tracing::info!("Track loaded into player");
                }
                TrackCommand::Clear => {
                    player.clear_track();
                    tracing::info!("Track cleared");
                }
            }
        }

        // Capture: blocks until a period of audio is available
        match input_pcm.io_i16()?.readi(&mut capture_buf) {
            Ok(frames) => {
                let n = frames as usize;

                // Feed captured audio to timecoder (extracts position + pitch)
                timecoder.submit(&capture_buf[..n * 2]);

                // Generate resampled output from the loaded track
                player.collect(&mut playback_buf[..n * 2], n, &timecoder);

                // Write output
                match output_pcm.io_i16()?.writei(&playback_buf[..n * 2]) {
                    Ok(_) => {}
                    Err(e) if e.errno() == libc::EPIPE => {
                        tracing::warn!("Playback underrun, recovering");
                        output_pcm.prepare()?;
                    }
                    Err(e) => return Err(e.into()),
                }

                // Publish scalar state (always, cheap)
                state
                    .elapsed
                    .store(player.get_elapsed().to_bits(), Ordering::Relaxed);
                state
                    .pitch
                    .store(player.get_effective_pitch().to_bits(), Ordering::Relaxed);
                state
                    .vinyl_pitch
                    .store(player.pitch.to_bits(), Ordering::Relaxed);
                state
                    .valid_counter
                    .store(timecoder.get_valid_counter(), Ordering::Relaxed);

                if let Some((pos, _)) = timecoder.get_position() {
                    state.timecode_pos.store(pos, Ordering::Relaxed);
                }
                // On transient dropout, hold last valid position instead of
                // publishing i32::MIN which causes spinner/waveform stutter.

                // Update monitor data periodically (uses try_lock to never block)
                period_counter += 1;
                if period_counter % MONITOR_UPDATE_INTERVAL == 0 {
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
