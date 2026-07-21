//! Equalizer APO / AutoEQ preset parsing.

use anyhow::{bail, Result};
use regex::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterKind {
    Peaking,
    LowShelf,
    HighShelf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Band {
    pub kind: FilterKind,
    pub fc_hz: f64,
    pub gain_db: f64,
    pub q: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Preset {
    pub preamp_db: f64,
    pub bands: Vec<Band>,
}

/// Parse outcome: the preset plus warnings for understood-but-skipped
/// lines (OFF filters, unsupported filter types).
#[derive(Debug, Default)]
pub struct Parsed {
    pub preset: Preset,
    pub warnings: Vec<String>,
}

pub fn parse(text: &str) -> Result<Parsed> {
    let preamp_re = Regex::new(r"(?i)^\s*Preamp:\s*(-?\d+(?:\.\d+)?)\s*dB").unwrap();
    // Loose match decides *whether* a line is a filter; strict match
    // validates the arguments of supported types.
    let loose_re = Regex::new(r"(?i)^\s*Filter\s*\d+:\s*(ON|OFF)\s+(\S+)").unwrap();
    let strict_re = Regex::new(
        r"(?i)^\s*Filter\s*\d+:\s*ON\s+\S+\s+Fc\s+(\d+(?:\.\d+)?)\s*Hz\s+Gain\s+(-?\d+(?:\.\d+)?)\s*dB\s+Q\s+(\d+(?:\.\d+)?)",
    )
    .unwrap();

    let mut out = Parsed::default();
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        if let Some(c) = preamp_re.captures(line) {
            out.preset.preamp_db = c[1].parse()?;
            continue;
        }
        let Some(c) = loose_re.captures(line) else {
            continue; // not a filter line: ignore
        };
        if c[1].eq_ignore_ascii_case("OFF") {
            out.warnings
                .push(format!("line {n}: filter is OFF, skipped"));
            continue;
        }
        let kind = match c[2].to_ascii_uppercase().as_str() {
            "PK" => FilterKind::Peaking,
            "LSC" => FilterKind::LowShelf,
            "HSC" => FilterKind::HighShelf,
            other => {
                out.warnings.push(format!(
                    "line {n}: unsupported filter type {other}, skipped"
                ));
                continue;
            }
        };
        let Some(c) = strict_re.captures(line) else {
            bail!("line {n}: unrecognized filter syntax: {line:?}");
        };
        out.preset.bands.push(Band {
            kind,
            fc_hz: c[1].parse()?,
            gain_db: c[2].parse()?,
            q: c[3].parse()?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
Preamp: -6.8 dB
Filter 1: ON PK Fc 105 Hz Gain -1.1 dB Q 0.70
Filter 2: ON LSC Fc 105 Hz Gain 6.0 dB Q 0.70
Filter 3: ON HSC Fc 10000 Hz Gain -4.5 dB Q 0.70
Filter 4: OFF PK Fc 230 Hz Gain 1.0 dB Q 1.00
Filter 5: ON LP Fc 19000 Hz
";

    #[test]
    fn parses_preamp() {
        let p = parse(SAMPLE).unwrap();
        assert!((p.preset.preamp_db - -6.8).abs() < 1e-6);
    }

    #[test]
    fn parses_supported_bands_and_skips_rest() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.preset.bands.len(), 3);
        assert_eq!(
            p.preset.bands[0],
            Band {
                kind: FilterKind::Peaking,
                fc_hz: 105.0,
                gain_db: -1.1,
                q: 0.70
            }
        );
        assert_eq!(p.preset.bands[1].kind, FilterKind::LowShelf);
        assert_eq!(p.preset.bands[2].kind, FilterKind::HighShelf);
        assert_eq!(
            p.warnings.len(),
            2,
            "OFF filter + unsupported LP: {:?}",
            p.warnings
        );
    }

    #[test]
    fn missing_preamp_defaults_to_zero() {
        let p = parse("Filter 1: ON PK Fc 100 Hz Gain 1 dB Q 1.0").unwrap();
        assert_eq!(p.preset.preamp_db, 0.0);
        assert_eq!(p.preset.bands.len(), 1);
    }

    #[test]
    fn malformed_supported_filter_is_an_error() {
        assert!(parse("Filter 1: ON PK Fc 105 Hz").is_err());
    }

    #[test]
    fn parses_the_shipped_example_preset() {
        let p = parse(include_str!("../presets/example.txt")).unwrap();
        assert_eq!(p.preset.bands.len(), 10);
        assert!(p.warnings.is_empty());
    }

    /// Assert a parsed preset matches ground truth exactly (see the
    /// exactness note in the plan: decimal → f64 is deterministic).
    fn assert_preset(
        text: &str,
        preamp_db: f64,
        expected: &[(FilterKind, f64, f64, f64)], // (kind, fc_hz, gain_db, q)
    ) {
        let p = parse(text).unwrap();
        assert!(
            p.warnings.is_empty(),
            "unexpected warnings: {:?}",
            p.warnings
        );
        assert_eq!(p.preset.preamp_db, preamp_db);
        assert_eq!(p.preset.bands.len(), expected.len());
        for (i, (band, &(kind, fc, gain, q))) in p.preset.bands.iter().zip(expected).enumerate() {
            assert_eq!(band.kind, kind, "band {i} kind");
            assert_eq!(band.fc_hz, fc, "band {i} fc");
            assert_eq!(band.gain_db, gain, "band {i} gain");
            assert_eq!(band.q, q, "band {i} q");
        }
    }

    #[test]
    fn parses_koss_porta_pro_exactly() {
        assert_preset(
            include_str!("../presets/koss_porta_pro.txt"),
            -13.16,
            &[
                (FilterKind::LowShelf, 105.0, 18.9, 0.70),
                (FilterKind::Peaking, 66.6, -14.7, 0.43),
                (FilterKind::Peaking, 1531.9, 2.3, 0.18),
                (FilterKind::Peaking, 4853.0, -6.7, 2.99),
                (FilterKind::HighShelf, 10000.0, 0.2, 0.70),
            ],
        );
    }

    #[test]
    fn parses_meze_99_classics_exactly() {
        assert_preset(
            include_str!("../presets/meze_99_classics.txt"),
            -5.96,
            &[
                (FilterKind::LowShelf, 105.0, -3.1, 0.70),
                (FilterKind::Peaking, 198.3, -10.9, 0.87),
                (FilterKind::Peaking, 373.1, 5.3, 0.49),
                (FilterKind::Peaking, 3274.4, 5.7, 3.42),
                (FilterKind::HighShelf, 10000.0, -1.4, 0.70),
            ],
        );
    }
}
