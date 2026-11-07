use alsa::pcm::{Access, Format, HwParams};
use alsa::{Direction, PCM};
use anyhow::{Context, Result};

/// Default playback buffer size in frames.
/// 256 frames at 48kHz = ~5.3ms latency.
const DEFAULT_PLAYBACK_BUFFER: i64 = 256;

/// Default capture buffer size in frames.
/// 1024 frames at 48kHz = ~21ms — large to avoid overruns.
const DEFAULT_CAPTURE_BUFFER: i64 = 1024;

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
    /// Playback buffer size in frames (default: 256)
    pub buffer_size: Option<u32>,
    /// Capture buffer size in frames (default: 1024)
    pub capture_buffer_size: Option<u32>,
    pub period_size: Option<u32>,
}

/// Configure a PCM device with the given buffer size.
fn configure_pcm(
    pcm: &PCM,
    label: &str,
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

    hwp.set_buffer_size_near(buffer_size)?;

    // Period = buffer / target_periods for reasonable wakeup frequency
    let period = (buffer_size / TARGET_PERIODS).max(2);
    hwp.set_period_size_near(period, alsa::ValueOr::Nearest)?;

    pcm.hw_params(&hwp)?;

    let actual_rate = hwp.get_rate()?;
    let actual_channels = hwp.get_channels()?;
    let actual_buffer = hwp.get_buffer_size()?;
    let actual_period = hwp.get_period_size()?;
    let latency_ms = actual_buffer as f64 / actual_rate as f64 * 1000.0;

    tracing::info!(
        "{}: rate={}, channels={}, buffer={} ({:.1}ms), period={}, periods={}",
        label,
        actual_rate,
        actual_channels,
        actual_buffer,
        latency_ms,
        actual_period,
        actual_buffer / actual_period
    );

    Ok(())
}

pub fn configure_audio_devices(settings: &AlsaSettings) -> Result<(PCM, PCM, usize)> {
    let cap_buffer = settings
        .capture_buffer_size
        .map(|b| b as i64)
        .unwrap_or(DEFAULT_CAPTURE_BUFFER);

    let pb_buffer = settings
        .buffer_size
        .map(|b| b as i64)
        .unwrap_or(DEFAULT_PLAYBACK_BUFFER);

    tracing::info!("Configuring capture: {}", settings.input_device);
    let capture = PCM::new(&settings.input_device, Direction::Capture, false)
        .map_err(|e| anyhow::anyhow!("{}", describe_open_error(&e)))
        .with_context(|| format!("Input device {}", settings.input_device))?;
    configure_pcm(
        &capture,
        "Capture",
        settings.num_input_channels,
        settings.sample_rate,
        cap_buffer,
    )
    .with_context(|| format!("Input device {}", settings.input_device))?;

    tracing::info!("Configuring playback: {}", settings.output_device);
    let playback = PCM::new(&settings.output_device, Direction::Playback, false)
        .map_err(|e| anyhow::anyhow!("{}", describe_open_error(&e)))
        .with_context(|| format!("Output device {}", settings.output_device))?;
    configure_pcm(
        &playback,
        "Playback",
        settings.num_output_channels,
        settings.sample_rate,
        pb_buffer,
    )
    .with_context(|| format!("Output device {}", settings.output_device))?;

    let buffer_size = {
        let hwp = capture.hw_params_current()?;
        hwp.get_buffer_size()? as usize
    };

    Ok((capture, playback, buffer_size))
}

/// ALSA devices that can be selected for input and output: the system
/// default plus one `plughw:<card id>` entry per sound card.
pub fn list_alsa_devices() -> Vec<String> {
    let mut devices = vec!["default".to_string()];
    for card in alsa::card::Iter::new().flatten() {
        let id = alsa::ctl::Ctl::from_card(&card, false)
            .and_then(|ctl| ctl.card_info())
            .and_then(|info| info.get_id().map(str::to_string));
        match id {
            Ok(id) => devices.push(format!("plughw:{id}")),
            Err(e) => tracing::warn!("Cannot read sound card {}: {}", card.get_index(), e),
        }
    }
    devices
}

/// Which way a sound device is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceUse {
    Capture,
    Playback,
}

/// Can `name` be opened for `usage`? Used to vet a choice before saving it.
/// A device that is merely busy (our own engine has it open) counts as fine:
/// that is not the same as broken. Returns the reason otherwise.
pub fn check_device(name: &str, usage: DeviceUse) -> Result<(), String> {
    let direction = match usage {
        DeviceUse::Capture => Direction::Capture,
        DeviceUse::Playback => Direction::Playback,
    };
    // Non-blocking, so a device in use fails immediately instead of waiting
    match PCM::new(name, direction, true) {
        Ok(_) => Ok(()),
        Err(e) if e.errno() == libc::EBUSY => Ok(()),
        Err(e) => Err(describe_open_error(&e)),
    }
}

/// A short, readable reason for a failed `snd_pcm_open`.
fn describe_open_error(error: &alsa::Error) -> String {
    describe_errno(error.errno())
}

fn describe_errno(errno: i32) -> String {
    match errno {
        libc::ENOENT | libc::ENODEV => "no such sound device".to_string(),
        libc::EACCES | libc::EPERM => "permission denied".to_string(),
        libc::EBUSY => "device busy".to_string(),
        // The kernel's internal "not supported", e.g. an HDMI port with nothing connected
        524 | libc::EOPNOTSUPP => "not supported (nothing connected?)".to_string(),
        other => format!("cannot be opened (error {other})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_described_readably() {
        assert_eq!(describe_errno(libc::ENOENT), "no such sound device");
        assert!(describe_errno(524).starts_with("not supported"));
        assert!(describe_errno(12345).contains("12345"));
    }

    #[test]
    fn a_device_that_does_not_exist_is_reported() {
        let result = check_device("neowax_no_such_card", DeviceUse::Playback);
        assert!(result.is_err(), "{result:?}");
        assert!(check_device("neowax_no_such_card", DeviceUse::Capture).is_err());
    }
}
