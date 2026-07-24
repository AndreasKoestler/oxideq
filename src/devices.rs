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
/// the display name with its id when one is available.
fn print_all(devices: impl Iterator<Item = Device>) {
    for line in render_all(devices.map(|d| (device_name(&d), device_id(&d)))) {
        println!("  {line}");
    }
}

/// Format the `--all` listing: each device once, deduped by backend id in
/// first-seen order, as `name  [id]` (or bare `name` when id-less). Devices
/// without an id are never deduped — an id-less host must not collapse onto its
/// first device. Pure so the dedup/formatting is unit-testable.
fn render_all<I: IntoIterator<Item = (String, String)>>(devices: I) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for (name, id) in devices {
        if id.is_empty() {
            lines.push(name);
        } else if seen.insert(id.clone()) {
            lines.push(format!("{name}  [{id}]"));
        }
    }
    lines
}

/// Devices bucketed for the curated view: hardware cards (with their
/// `hw`/`plughw` selectors), well-known system routes, and everything else
/// (hosts without ALSA-style ids).
struct Curated {
    cards: Vec<CardGroup>,
    routes: Vec<String>,
    other: Vec<String>,
}

/// Index of the card group keyed by `key`, appending a new one (named `name`,
/// no selectors yet) when none exists.
fn card_slot(cards: &mut Vec<CardGroup>, key: &str, name: String) -> usize {
    if let Some(i) = cards.iter().position(|g| g.key == key) {
        return i;
    }
    cards.push(CardGroup {
        key: key.to_owned(),
        name,
        hw: None,
        plughw: None,
    });
    cards.len() - 1
}

/// Append `value` unless the bucket already holds it (first-seen dedup).
fn push_unique(bucket: &mut Vec<String>, value: String) {
    if !bucket.contains(&value) {
        bucket.push(value);
    }
}

impl Curated {
    /// Route one `(name, id)` device into its bucket: a card's `hw`/`plughw`
    /// selector, a well-known system route, or `other` (deduped).
    fn add(&mut self, name: String, id: String) {
        if let Some((prefix, key)) = split_card_id(&id) {
            let idx = card_slot(&mut self.cards, key, name);
            // split_card_id yields only "hw" or "plughw"; else is "plughw".
            if prefix == "hw" {
                self.cards[idx].hw = Some(id);
            } else {
                self.cards[idx].plughw = Some(id);
            }
        } else if ROUTE_IDS.contains(&id.as_str()) {
            push_unique(&mut self.routes, id);
        } else {
            // Hosts without ALSA-style ids (CoreAudio, WASAPI): keep the name so
            // the curated view isn't empty there.
            push_unique(&mut self.other, name);
        }
    }
}

/// Bucket `(name, id)` pairs into hardware card groups (a card's `hw`/`plughw`
/// selectors merged under one entry), well-known system routes, and everything
/// else. Pure so the grouping logic is unit-testable without real audio devices.
fn curate(devices: Vec<(String, String)>) -> Curated {
    let mut curated = Curated {
        cards: Vec::new(),
        routes: Vec::new(),
        other: Vec::new(),
    };
    for (name, id) in devices {
        curated.add(name, id);
    }
    curated
}

/// Output lines for one card: its name, then whichever of the bit-perfect
/// (`hw`) and converting (`plughw`) selectors it has.
fn render_card(g: &CardGroup) -> Vec<String> {
    let mut lines = vec![format!("  {}", g.name)];
    if let Some(id) = &g.hw {
        lines.push(format!(
            "    {id:<24} bit-perfect (locks to the source rate)"
        ));
    }
    if let Some(id) = &g.plughw {
        lines.push(format!("    {id:<24} auto rate/format conversion"));
    }
    lines
}

/// Format the curated buckets into indented output lines. Empty only when
/// there is genuinely nothing to show (no cards, routes, or others) — the
/// caller prints the "none found" hint. Pure so the layout is unit-testable.
fn render_curated(curated: &Curated) -> Vec<String> {
    if curated.cards.is_empty() && curated.routes.is_empty() {
        return curated
            .other
            .iter()
            .map(|name| format!("  {name}"))
            .collect();
    }
    let mut lines: Vec<String> = curated.cards.iter().flat_map(render_card).collect();
    if !curated.routes.is_empty() {
        lines.push(format!(
            "  Routes (OS picks/mixes): {}",
            curated.routes.join(", ")
        ));
    }
    lines
}

/// Group `hw`/`plughw` selectors under each card and list system routes; skip
/// the plugin variants (`--all` shows those).
fn print_curated(devices: impl Iterator<Item = Device>) {
    let curated = curate(devices.map(|d| (device_name(&d), device_id(&d))).collect());
    let lines = render_curated(&curated);
    if lines.is_empty() {
        println!("  (none found; try `oxideq devices --all`)");
        return;
    }
    for line in lines {
        println!("{line}");
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
    devices.iter().position(|(_, id)| id == needle).or_else(|| {
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

    fn pair(name: &str, id: &str) -> (String, String) {
        (name.to_owned(), id.to_owned())
    }

    #[test]
    fn curate_merges_hw_and_plughw_under_one_card() {
        let c = curate(vec![
            pair("S9 Pro", "hw:CARD=S9Pro,DEV=0"),
            pair("S9 Pro", "plughw:CARD=S9Pro,DEV=0"),
        ]);
        assert_eq!(c.cards.len(), 1);
        assert_eq!(c.cards[0].name, "S9 Pro");
        assert_eq!(c.cards[0].hw.as_deref(), Some("hw:CARD=S9Pro,DEV=0"));
        assert_eq!(
            c.cards[0].plughw.as_deref(),
            Some("plughw:CARD=S9Pro,DEV=0")
        );
        assert!(c.routes.is_empty());
        assert!(c.other.is_empty());
    }

    #[test]
    fn curate_collects_known_routes_deduped_and_keeps_others_by_name() {
        let c = curate(vec![
            pair("Default", "default"),
            pair("PipeWire", "pipewire"),
            pair("Default", "default"),   // duplicate id — dropped
            pair("MacBook Speakers", ""), // no ALSA id — falls to `other`
            pair("MacBook Speakers", ""), // duplicate name — dropped
        ]);
        assert!(c.cards.is_empty());
        assert_eq!(c.routes, ["default", "pipewire"]);
        assert_eq!(c.other, ["MacBook Speakers"]);
    }

    #[test]
    fn render_curated_lists_selectors_and_routes() {
        let c = curate(vec![
            pair("S9 Pro", "hw:CARD=S9Pro,DEV=0"),
            pair("S9 Pro", "plughw:CARD=S9Pro,DEV=0"),
            pair("Default", "default"),
        ]);
        let lines = render_curated(&c);
        assert_eq!(lines[0], "  S9 Pro");
        assert!(lines[1].contains("hw:CARD=S9Pro,DEV=0") && lines[1].contains("bit-perfect"));
        assert!(lines[2].contains("plughw:CARD=S9Pro,DEV=0") && lines[2].contains("auto rate"));
        assert_eq!(lines[3], "  Routes (OS picks/mixes): default");
    }

    #[test]
    fn render_curated_is_empty_when_nothing_found() {
        let c = curate(Vec::new());
        assert!(render_curated(&c).is_empty());
        // Others-only (no cards/routes) still renders their names.
        let only_other = curate(vec![pair("Speakers", "")]);
        assert_eq!(render_curated(&only_other), ["  Speakers"]);
    }

    #[test]
    fn render_all_dedups_by_id_and_keeps_idless() {
        let lines = render_all([
            pair("S9 Pro", "hw:CARD=S9Pro,DEV=0"),
            pair("S9 Pro alias", "hw:CARD=S9Pro,DEV=0"), // same id — dropped
            pair("Speakers", ""),                        // id-less — always kept
            pair("Speakers", ""),                        // id-less again — also kept
        ]);
        assert_eq!(
            lines,
            ["S9 Pro  [hw:CARD=S9Pro,DEV=0]", "Speakers", "Speakers",]
        );
    }
}
