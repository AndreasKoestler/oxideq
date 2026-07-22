//! Device discovery, selection by name substring, and output sample-rate
//! negotiation: lock the output to the source rate; warn on fallback.

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Host, SampleFormat};

#[derive(Debug, Clone, Copy)]
pub enum Direction {
    Input,
    Output,
}

/// Best-effort human-readable device name (never fails `list`/`find` just
/// because a device vanished mid-enumeration).
fn device_name(d: &Device) -> String {
    d.description()
        .map_or_else(|_| "<unnamed>".into(), |desc| desc.name().to_string())
}

/// Print input and output device names to stdout.
///
/// # Errors
/// Returns an error if the host cannot enumerate its input or output devices.
pub fn list(host: &Host) -> Result<()> {
    println!("Input devices:");
    for d in host.input_devices().context("enumerating input devices")? {
        println!("  {}", device_name(&d));
    }
    println!("Output devices:");
    for d in host
        .output_devices()
        .context("enumerating output devices")?
    {
        println!("  {}", device_name(&d));
    }
    Ok(())
}

/// Find a device by case-insensitive name substring, or the default when
/// `name_substr` is `None`.
///
/// # Errors
/// Returns an error if enumeration fails, or if no default / matching device
/// exists for `dir`.
pub fn find(host: &Host, dir: Direction, name_substr: Option<&str>) -> Result<Device> {
    let Some(pat) = name_substr else {
        return match dir {
            Direction::Input => host.default_input_device(),
            Direction::Output => host.default_output_device(),
        }
        .ok_or_else(|| anyhow!("no default {dir:?} device"));
    };
    let needle = pat.to_lowercase();
    let matches = |d: &Device| device_name(d).to_lowercase().contains(&needle);
    match dir {
        Direction::Input => host.input_devices()?.find(matches),
        Direction::Output => host.output_devices()?.find(matches),
    }
    .ok_or_else(|| anyhow!("no {dir:?} device matching {pat:?} (try `oxideq devices`)"))
}

/// Pick an output rate given `(min, max)` supported ranges. Exact match
/// wins (`true`); otherwise the nearest range endpoint (`false`) — the OS
/// will resample and the caller must warn.
#[must_use]
pub fn pick_rate(ranges: &[(u32, u32)], want: u32) -> Option<(u32, bool)> {
    if ranges.iter().any(|&(lo, hi)| want >= lo && want <= hi) {
        return Some((want, true));
    }
    ranges
        .iter()
        .map(|&(lo, hi)| if want < lo { lo } else { hi })
        .min_by_key(|r| r.abs_diff(want))
        .map(|r| (r, false))
}

/// True if `rate` falls inside any supported `(min, max)` range.
#[must_use]
pub fn rate_supported(ranges: &[(u32, u32)], rate: u32) -> bool {
    ranges.iter().any(|&(lo, hi)| (lo..=hi).contains(&rate))
}

fn f32_ranges<I>(configs: I, channels: u16) -> Vec<(u32, u32)>
where
    I: Iterator<Item = cpal::SupportedStreamConfigRange>,
{
    configs
        .filter(|r| r.channels() == channels && r.sample_format() == SampleFormat::F32)
        .map(|r| (r.min_sample_rate(), r.max_sample_rate()))
        .collect()
}

/// Negotiate the single rate the whole pipeline runs at. The ring buffer
/// has no resampler, so capture and playback MUST open at the same rate.
/// Prefer `want` (the source rate — bit-perfect); otherwise the nearest
/// output-supported rate, provided the input can also capture at it (the
/// OS resamples the capture side; the caller must warn). No common rate
/// is an error — proceeding would drift the ring and pitch-shift audio.
///
/// # Errors
/// Returns an error if device configs cannot be queried, the output has no
/// matching f32 config, or input and output share no common rate.
pub fn negotiate_rate(
    input: &Device,
    output: &Device,
    want: u32,
    channels: u16,
) -> Result<(u32, bool)> {
    let out_ranges = f32_ranges(
        output
            .supported_output_configs()
            .context("querying output configs")?,
        channels,
    );
    let (rate, exact) = pick_rate(&out_ranges, want)
        .ok_or_else(|| anyhow!("output device has no {channels}-channel f32 config"))?;
    if exact {
        return Ok((rate, true));
    }
    let in_ranges = f32_ranges(
        input
            .supported_input_configs()
            .context("querying input configs")?,
        channels,
    );
    if rate_supported(&in_ranges, rate) {
        return Ok((rate, false));
    }
    bail!(
        "no common rate: source runs at {want} Hz, output falls back to {rate} Hz, \
         and the input cannot capture at {rate} Hz — set both devices to one rate \
         in your sound settings"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_rate_inside_a_supported_range_wins() {
        assert_eq!(pick_rate(&[(8_000, 192_000)], 44_100), Some((44_100, true)));
        assert_eq!(pick_rate(&[(48_000, 48_000)], 48_000), Some((48_000, true)));
    }

    #[test]
    fn unsupported_rate_falls_back_to_nearest_endpoint() {
        // want 44100, device only does 48k..192k → 48000 (not exact)
        assert_eq!(
            pick_rate(&[(48_000, 192_000)], 44_100),
            Some((48_000, false))
        );
        // want 192k, device caps at 96k → 96000
        assert_eq!(
            pick_rate(&[(44_100, 96_000)], 192_000),
            Some((96_000, false))
        );
        // two disjoint ranges: nearest endpoint across all of them
        assert_eq!(
            pick_rate(&[(8_000, 16_000), (44_100, 48_000)], 32_000),
            Some((44_100, false))
        );
    }

    #[test]
    fn no_ranges_means_no_rate() {
        assert_eq!(pick_rate(&[], 48_000), None);
    }

    #[test]
    fn rate_supported_checks_inclusive_ranges() {
        assert!(rate_supported(&[(44_100, 48_000)], 44_100));
        assert!(rate_supported(&[(44_100, 48_000)], 48_000));
        assert!(!rate_supported(&[(44_100, 48_000)], 96_000));
        assert!(!rate_supported(&[], 48_000));
    }
}
