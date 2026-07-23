//! Device discovery, selection by backend id or name substring, and output
//! sample-rate negotiation: lock the output to the source rate; warn on fallback.

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, Host, SampleFormat};
use std::collections::HashSet;

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

/// Best-effort backend-specific id — the stable selector for a device. On ALSA
/// this is the PCM name (e.g. `hw:CARD=S9Pro,DEV=0`, `plughw:...`,
/// `sysdefault:CARD=S9Pro`). Empty when unavailable.
fn device_id(d: &Device) -> String {
    d.id()
        .map_or_else(|_| String::new(), |id| id.id().to_string())
}

/// ALSA ids surfaced in the curated view as convenient system routes (the OS
/// picks/mixes for you). Everything else non-hardware is plugin noise hidden
/// unless `all`.
const ROUTE_IDS: [&str; 4] = ["default", "pipewire", "pulse", "jack"];

/// One physical card+device, holding its two hardware selectors.
struct CardGroup {
    /// The shared `CARD=…,DEV=…` grouping key.
    key: String,
    /// Display name (both selectors share it).
    name: String,
    hw: Option<String>,
    plughw: Option<String>,
}

/// Print input and output devices to stdout.
///
/// The ALSA backend lists one entry per PCM: raw hardware (`hw:CARD=…`), its
/// software-converting wrapper (`plughw:CARD=…`), and a pile of routing plugins
/// (`sysdefault`, `front`, `surround*`, `iec958`, `usbstream`, …) that mostly
/// share a display name. The curated default groups each card's `hw`/`plughw`
/// selectors under its name and lists the useful server routes; `all` dumps
/// every distinct PCM as `name  [id]` (deduped by id, the unique key). The id
/// is the exact string `--input`/`--output` matches.
///
/// # Errors
/// Returns an error if the host cannot enumerate its input or output devices.
pub fn list(host: &Host, all: bool) -> Result<()> {
    println!("Input devices:");
    print_section(
        host.input_devices().context("enumerating input devices")?,
        all,
    );
    println!("Output devices:");
    print_section(
        host.output_devices()
            .context("enumerating output devices")?,
        all,
    );
    if !all {
        println!(
            "\nShowing hardware devices and system routes. \
             `oxideq devices --all` lists every ALSA PCM."
        );
    }
    Ok(())
}

fn print_section(devices: impl Iterator<Item = Device>, all: bool) {
    if all {
        print_all(devices);
    } else {
        print_curated(devices);
    }
}

/// Print each device once (deduped by backend id, first-seen order), annotating
/// the display name with its id when one is available. Devices without an id
/// are never deduped — an id-less host must not collapse onto its first device.
fn print_all(devices: impl Iterator<Item = Device>) {
    let mut seen = HashSet::new();
    for d in devices {
        let id = device_id(&d);
        if !id.is_empty() && !seen.insert(id.clone()) {
            continue;
        }
        let name = device_name(&d);
        if id.is_empty() {
            println!("  {name}");
        } else {
            println!("  {name}  [{id}]");
        }
    }
}

/// Group `hw`/`plughw` selectors under each card and list system routes; skip
/// the plugin variants (`--all` shows those).
fn print_curated(devices: impl Iterator<Item = Device>) {
    let mut cards: Vec<CardGroup> = Vec::new();
    let mut routes: Vec<String> = Vec::new();
    // Hosts without ALSA-style ids (CoreAudio, WASAPI) match neither branch
    // below; keep their names so the curated view isn't empty there.
    let mut other: Vec<String> = Vec::new();
    for d in devices {
        let id = device_id(&d);
        if let Some((prefix, key)) = split_card_id(&id) {
            let idx = cards.iter().position(|g| g.key == key).unwrap_or_else(|| {
                cards.push(CardGroup {
                    key: key.to_owned(),
                    name: device_name(&d),
                    hw: None,
                    plughw: None,
                });
                cards.len() - 1
            });
            match prefix {
                "hw" => cards[idx].hw = Some(id),
                "plughw" => cards[idx].plughw = Some(id),
                _ => {}
            }
        } else if ROUTE_IDS.contains(&id.as_str()) {
            if !routes.contains(&id) {
                routes.push(id);
            }
        } else {
            let name = device_name(&d);
            if !other.contains(&name) {
                other.push(name);
            }
        }
    }
    if cards.is_empty() && routes.is_empty() {
        if other.is_empty() {
            println!("  (none found; try `oxideq devices --all`)");
        } else {
            for name in &other {
                println!("  {name}");
            }
        }
        return;
    }
    for g in &cards {
        println!("  {}", g.name);
        if let Some(id) = &g.hw {
            println!("    {id:<24} bit-perfect (locks to the source rate)");
        }
        if let Some(id) = &g.plughw {
            println!("    {id:<24} auto rate/format conversion");
        }
    }
    if !routes.is_empty() {
        println!("  Routes (OS picks/mixes): {}", routes.join(", "));
    }
}

/// Split an ALSA `hw:CARD=…,DEV=…` / `plughw:CARD=…,DEV=…` id into
/// `(prefix, key)` where `prefix` is `"hw"`/`"plughw"` and `key` is the shared
/// `CARD=…,DEV=…` grouping suffix. `None` for any other id.
fn split_card_id(id: &str) -> Option<(&str, &str)> {
    ["hw", "plughw"].into_iter().find_map(|prefix| {
        id.strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix(':'))
            .map(|key| (prefix, key))
    })
}

/// Index of the device `needle` selects among lowercased `(name, id)` pairs.
/// An exact id match wins outright: ids can be substrings of one another
/// (`hw:CARD=…` sits inside `plughw:CARD=…`), so substring alone would leave
/// the pick to enumeration order. Otherwise the first name/id substring match.
fn best_match(devices: &[(String, String)], needle: &str) -> Option<usize> {
    devices
        .iter()
        .position(|(_, id)| id == needle)
        .or_else(|| {
            devices
                .iter()
                .position(|(name, id)| name.contains(needle) || id.contains(needle))
        })
}

/// Find a device by its backend id or display name (see `list`), or the
/// default when `name_substr` is `None`. Matching is case-insensitive: an
/// exact id match (e.g. `hw:CARD=S9Pro,DEV=0`) wins; otherwise the first
/// device whose name or id contains `name_substr`.
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
    let mut devices: Vec<Device> = match dir {
        Direction::Input => host.input_devices()?.collect(),
        Direction::Output => host.output_devices()?.collect(),
    };
    let keys: Vec<(String, String)> = devices
        .iter()
        .map(|d| (device_name(d).to_lowercase(), device_id(d).to_lowercase()))
        .collect();
    let idx = best_match(&keys, &needle)
        .ok_or_else(|| anyhow!("no {dir:?} device matching {pat:?} (try `oxideq devices`)"))?;
    Ok(devices.swap_remove(idx))
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
    fn split_card_id_recognizes_hw_and_plughw_and_ignores_plugins() {
        assert_eq!(
            split_card_id("hw:CARD=0,DEV=0"),
            Some(("hw", "CARD=0,DEV=0"))
        );
        assert_eq!(
            split_card_id("plughw:CARD=0,DEV=0"),
            Some(("plughw", "CARD=0,DEV=0"))
        );
        // hw and plughw for the same card share a grouping key.
        assert_eq!(
            split_card_id("hw:CARD=0,DEV=0").map(|(_, k)| k),
            split_card_id("plughw:CARD=0,DEV=0").map(|(_, k)| k)
        );
        for other in [
            "sysdefault:CARD=0",
            "front:CARD=0,DEV=0",
            "pipewire",
            "null",
            "hwmix",
        ] {
            assert_eq!(split_card_id(other), None, "{other} is not a card selector");
        }
    }

    #[test]
    fn best_match_prefers_exact_id_over_enumeration_order() {
        let key = |name: &str, id: &str| (name.to_owned(), id.to_owned());
        // plughw listed first: its id *contains* the hw id as a substring, so
        // only the exact-match pass keeps `hw:…` from landing on it.
        let devs = [
            key("usb audio", "plughw:card=0,dev=0"),
            key("usb audio", "hw:card=0,dev=0"),
        ];
        assert_eq!(best_match(&devs, "hw:card=0,dev=0"), Some(1));
        assert_eq!(best_match(&devs, "plughw:card=0,dev=0"), Some(0));
        // No exact id → first name/id substring match.
        assert_eq!(best_match(&devs, "usb"), Some(0));
        assert_eq!(best_match(&devs, "card=0"), Some(0));
        assert_eq!(best_match(&devs, "spdif"), None);
    }

    #[test]
    fn rate_supported_checks_inclusive_ranges() {
        assert!(rate_supported(&[(44_100, 48_000)], 44_100));
        assert!(rate_supported(&[(44_100, 48_000)], 48_000));
        assert!(!rate_supported(&[(44_100, 48_000)], 96_000));
        assert!(!rate_supported(&[], 48_000));
    }
}
