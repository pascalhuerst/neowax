use alsa::pcm::{Access, Format, HwParams};
use alsa::{Direction, PCM};
use anyhow::Result;

/// Default playback buffer size in samples.
/// Legacy xwax uses 240 (~4 periods at period_size=64).
/// Must be >= 2 * period_size. ~4 periods is the sweet spot.
const DEFAULT_PLAYBACK_BUFFER: i64 = 256;

#[derive(Clone)]
pub struct AlsaSettings {
    pub input_device: String,
    pub output_device: String,
    pub num_input_channels: u32,
    pub num_output_channels: u32,
    pub sample_rate: u32,
    /// Playback buffer size in samples (default: 240, matching legacy xwax)
    pub buffer_size: Option<u32>,
    pub period_size: Option<u32>,
}

/// Configure a PCM device for capture: smallest period, largest buffer.
fn configure_capture(
    pcm: &PCM,
    num_channels: u32,
    sample_rate: u32,
    buffer_size: i64,
) -> Result<()> {
    let hwp = HwParams::any(pcm)?;

    hwp.set_access(Access::RWInterleaved)?;
    hwp.set_format(Format::s16())?;
    hwp.set_channels(num_channels)?;

    // Disable ALSA's software resampling — we do our own pitch-based resampling
    hwp.set_rate_resample(false)?;

    hwp.set_rate(sample_rate, alsa::ValueOr::Nearest)?;

    // Smallest possible period for lowest latency wakeups
    hwp.set_period_size_near(2, alsa::ValueOr::Nearest)?;

    // Maximum capture buffer to minimize drops
    hwp.set_buffer_size_near(buffer_size)?;

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

/// Configure a PCM device for playback: smallest period, tiny buffer.
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

    // Smallest possible period for lowest latency wakeups
    hwp.set_period_size_near(2, alsa::ValueOr::Nearest)?;

    // Small playback buffer to keep latency low
    match hwp.set_buffer_size(buffer_size) {
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                "Buffer of {} samples failed ({}), trying nearest",
                buffer_size,
                e
            );
            hwp.set_buffer_size_near(buffer_size)?;
        }
    }

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
        pb_buffer,
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
