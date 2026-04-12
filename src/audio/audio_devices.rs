use alsa::pcm::{Access, Format, HwParams};
use alsa::{Direction, PCM};
use anyhow::Result;

/// Default playback buffer size in frames.
/// 256 frames at 48kHz = ~5.3ms latency — good balance for vinyl control.
const DEFAULT_PLAYBACK_BUFFER: i64 = 256;

/// Capture buffer: large to avoid drops. 1024 frames at 48kHz = ~21ms.
const CAPTURE_BUFFER: i64 = 1024;

/// Target number of periods per buffer. 4 is the standard sweet spot:
/// frequent enough wakeups without excessive interrupt overhead.
const TARGET_PERIODS: i64 = 4;

#[derive(Clone)]
pub struct AlsaSettings {
    pub input_device: String,
    pub output_device: String,
    pub num_input_channels: u32,
    pub num_output_channels: u32,
    pub sample_rate: u32,
    /// Playback buffer size in frames (default: 256, ~5.3ms at 48kHz)
    pub buffer_size: Option<u32>,
    pub period_size: Option<u32>,
}

/// Configure a PCM device for capture: large buffer, sensible period.
fn configure_capture(
    pcm: &PCM,
    num_channels: u32,
    sample_rate: u32,
) -> Result<()> {
    let hwp = HwParams::any(pcm)?;

    hwp.set_access(Access::RWInterleaved)?;
    hwp.set_format(Format::s16())?;
    hwp.set_channels(num_channels)?;

    // Disable ALSA's software resampling — we do our own pitch-based resampling
    hwp.set_rate_resample(false)?;

    hwp.set_rate(sample_rate, alsa::ValueOr::Nearest)?;

    // Large capture buffer to avoid overruns
    hwp.set_buffer_size_near(CAPTURE_BUFFER)?;

    // Period = buffer / target_periods for reasonable wakeup frequency
    let period = (CAPTURE_BUFFER / TARGET_PERIODS).max(2);
    hwp.set_period_size_near(period, alsa::ValueOr::Nearest)?;

    pcm.hw_params(&hwp)?;

    let actual_rate = hwp.get_rate()?;
    let actual_channels = hwp.get_channels()?;
    let actual_buffer = hwp.get_buffer_size()?;
    let actual_period = hwp.get_period_size()?;

    tracing::info!(
        "Capture: rate={}, channels={}, buffer={}, period={}, periods={}",
        actual_rate,
        actual_channels,
        actual_buffer,
        actual_period,
        actual_buffer / actual_period
    );

    Ok(())
}

/// Configure a PCM device for playback: small buffer for low latency.
fn configure_playback(
    pcm: &PCM,
    num_channels: u32,
    sample_rate: u32,
    buffer_size: i64,
) -> Result<()> {
    let hwp = HwParams::any(pcm)?;

    hwp.set_access(Access::RWInterleaved)?;
    hwp.set_format(Format::s16())?;
    hwp.set_channels(num_channels)?;

    // Disable ALSA's software resampling
    hwp.set_rate_resample(false)?;

    hwp.set_rate(sample_rate, alsa::ValueOr::Nearest)?;

    // Small playback buffer to keep latency low
    hwp.set_buffer_size_near(buffer_size)?;

    // Period = buffer / target_periods
    let period = (buffer_size / TARGET_PERIODS).max(2);
    hwp.set_period_size_near(period, alsa::ValueOr::Nearest)?;

    pcm.hw_params(&hwp)?;

    let actual_rate = hwp.get_rate()?;
    let actual_channels = hwp.get_channels()?;
    let actual_buffer = hwp.get_buffer_size()?;
    let actual_period = hwp.get_period_size()?;

    tracing::info!(
        "Playback: rate={}, channels={}, buffer={} (requested {}), period={}, periods={}",
        actual_rate,
        actual_channels,
        actual_buffer,
        buffer_size,
        actual_period,
        actual_buffer / actual_period
    );

    Ok(())
}

pub fn configure_audio_devices(settings: &AlsaSettings) -> Result<(PCM, PCM, usize)> {
    tracing::info!("Configuring capture: {}", settings.input_device);

    let pb_buffer = settings
        .buffer_size
        .map(|b| b as i64)
        .unwrap_or(DEFAULT_PLAYBACK_BUFFER);

    let capture = PCM::new(&settings.input_device, Direction::Capture, false)?;
    configure_capture(
        &capture,
        settings.num_input_channels,
        settings.sample_rate,
    )?;

    tracing::info!("Configuring playback: {}", settings.output_device);
    let playback = PCM::new(&settings.output_device, Direction::Playback, false)?;

    configure_playback(
        &playback,
        settings.num_output_channels,
        settings.sample_rate,
        pb_buffer,
    )?;

    let buffer_size = {
        let hwp = capture.hw_params_current()?;
        hwp.get_buffer_size()? as usize
    };

    Ok((capture, playback, buffer_size))
}
