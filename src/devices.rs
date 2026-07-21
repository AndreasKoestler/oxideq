//! Device discovery, selection by name substring, and output sample-rate
//! negotiation (PRD 3.2: lock output to the source rate; warn on fallback).

use anyhow::{anyhow, Context, Result};
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
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| "<unnamed>".into())
}

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

pub fn find(host: &Host, dir: Direction, name_substr: Option<&str>) -> Result<Device> {
    let (default, all): (Option<Device>, Vec<Device>) = match dir {
        Direction::Input => (host.default_input_device(), host.input_devices()?.collect()),
        Direction::Output => (
            host.default_output_device(),
            host.output_devices()?.collect(),
        ),
    };
    match name_substr {
        None => default.ok_or_else(|| anyhow!("no default {dir:?} device")),
        Some(pat) => {
            let needle = pat.to_lowercase();
            all.into_iter()
                .find(|d| device_name(d).to_lowercase().contains(&needle))
                .ok_or_else(|| anyhow!("no {dir:?} device matching {pat:?} (try `oxideq devices`)"))
        }
    }
}

/// Pick an output rate given `(min, max)` supported ranges. Exact match
/// wins (`true`); otherwise the nearest range endpoint (`false`) — the OS
/// will resample and the caller must warn (PRD §5 reliability).
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

/// Negotiate an f32 output config at `want_rate`. Buffer size is left as
/// `Default`; the engine sets it after clamping to the device's range.
pub fn output_config(
    device: &Device,
    want_rate: u32,
    channels: u16,
) -> Result<(cpal::StreamConfig, bool)> {
    let ranges: Vec<(u32, u32)> = device
        .supported_output_configs()
        .context("querying output configs")?
        .filter(|r| r.channels() == channels && r.sample_format() == SampleFormat::F32)
        .map(|r| (r.min_sample_rate(), r.max_sample_rate()))
        .collect();
    let (rate, exact) = pick_rate(&ranges, want_rate)
        .ok_or_else(|| anyhow!("output device has no {channels}-channel f32 config"))?;
    Ok((
        cpal::StreamConfig {
            channels,
            sample_rate: rate,
            buffer_size: cpal::BufferSize::Default,
        },
        exact,
    ))
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
}
