//! Equalizer APO / `AutoEQ` preset parsing.

use anyhow::{Result, bail};
use nom::branch::alt;
use nom::bytes::complete::{tag_no_case, take_while1};
use nom::character::complete::{char, digit1, one_of, space0, space1};
use nom::combinator::{map_res, opt, recognize, value};
use nom::sequence::{preceded, terminated};
use nom::{IResult, Parser};

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

/// What a single line of the preset file turned out to be. Grammar only
/// classifies; the skip/warn/error policy lives in `parse`.
#[derive(Debug)]
enum Line<'a> {
    Preamp(f64),
    /// `args` is `Some` only when the full `Fc .. Hz Gain .. dB Q ..`
    /// argument list parsed.
    Filter {
        on: bool,
        kind: &'a str,
        args: Option<(f64, f64, f64)>,
    },
    Other,
}

/// Unsigned decimal: `105`, `0.70`.
fn unum(i: &str) -> IResult<&str, f64> {
    map_res(recognize((digit1, opt((char('.'), digit1)))), str::parse).parse(i)
}

/// Signed decimal: `-6.8`, `+1.5`, `2`.
fn snum(i: &str) -> IResult<&str, f64> {
    map_res(
        recognize((opt(one_of("+-")), digit1, opt((char('.'), digit1)))),
        str::parse,
    )
    .parse(i)
}

/// One whitespace-delimited word (the filter-type token).
fn token(i: &str) -> IResult<&str, &str> {
    take_while1(|c: char| !c.is_whitespace()).parse(i)
}

/// `Filter <n>:` — whitespace between `Filter` and the number is optional.
fn filter_head(i: &str) -> IResult<&str, ()> {
    value(
        (),
        (
            space0,
            tag_no_case("filter"),
            space0,
            digit1,
            char(':'),
            space0,
        ),
    )
    .parse(i)
}

fn onoff(i: &str) -> IResult<&str, bool> {
    alt((
        value(true, tag_no_case("on")),
        value(false, tag_no_case("off")),
    ))
    .parse(i)
}

/// `Fc <num> Hz` etc.; no space required before the unit (`100Hz`).
fn fc_field(i: &str) -> IResult<&str, f64> {
    preceded(
        (tag_no_case("fc"), space1),
        terminated(unum, (space0, tag_no_case("hz"))),
    )
    .parse(i)
}

fn gain_field(i: &str) -> IResult<&str, f64> {
    preceded(
        (tag_no_case("gain"), space1),
        terminated(snum, (space0, tag_no_case("db"))),
    )
    .parse(i)
}

fn q_field(i: &str) -> IResult<&str, f64> {
    preceded((tag_no_case("q"), space1), unum).parse(i)
}

/// Full filter line with a valid argument list.
fn strict_filter(i: &str) -> IResult<&str, Line<'_>> {
    (
        filter_head,
        terminated(tag_no_case("on"), space1),
        terminated(token, space1),
        terminated(fc_field, space1),
        terminated(gain_field, space1),
        q_field,
    )
        .map(|((), _, kind, fc_hz, gain_db, q)| Line::Filter {
            on: true,
            kind,
            args: Some((fc_hz, gain_db, q)),
        })
        .parse(i)
}

/// Anything recognizably a filter line, valid arguments or not. Decides
/// *whether* a line is a filter; `strict_filter` validates the arguments.
fn loose_filter(i: &str) -> IResult<&str, Line<'_>> {
    (filter_head, onoff, space1, token)
        .map(|((), on, _, kind)| Line::Filter {
            on,
            kind,
            args: None,
        })
        .parse(i)
}

/// `Preamp: <num> dB` — no space allowed before the colon.
fn preamp(i: &str) -> IResult<&str, Line<'_>> {
    terminated(
        preceded((space0, tag_no_case("preamp"), char(':'), space0), snum),
        (space0, tag_no_case("db")),
    )
    .map(Line::Preamp)
    .parse(i)
}

/// Ordered choice; trailing content after a match is allowed, and lines
/// matching nothing are `Other` (ignored).
fn classify(line: &str) -> Line<'_> {
    alt((strict_filter, loose_filter, preamp))
        .parse(line)
        .map_or(Line::Other, |(_, l)| l)
}

/// Parse an Equalizer APO / `AutoEQ` preset into bands plus warnings.
///
/// # Errors
/// Returns an error on a line that is recognizably a filter but whose
/// argument list is malformed (e.g. missing Gain/Q).
pub fn parse(text: &str) -> Result<Parsed> {
    let mut out = Parsed::default();
    let mut preamp_seen = false;
    for (idx, line) in text.lines().enumerate() {
        let n = idx + 1;
        match classify(line) {
            Line::Preamp(v) => {
                if preamp_seen {
                    out.warnings.push(format!(
                        "line {n}: duplicate Preamp overrides the previous one"
                    ));
                }
                preamp_seen = true;
                out.preset.preamp_db = v;
            }
            Line::Filter { on: false, .. } => {
                out.warnings
                    .push(format!("line {n}: filter is OFF, skipped"));
            }
            Line::Filter {
                on: true,
                kind,
                args,
            } => {
                let kind = match kind.to_ascii_uppercase().as_str() {
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
                let Some((fc_hz, gain_db, q)) = args else {
                    bail!("line {n}: unrecognized filter syntax: {line:?}");
                };
                out.preset.bands.push(Band {
                    kind,
                    fc_hz,
                    gain_db,
                    q,
                });
            }
            Line::Other => {}
        }
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

    /// Lines that are neither `Preamp:` nor `Filter N:` — comments,
    /// `AutoEQ` headers, blanks, prose — are skipped without warnings and
    /// don't disturb the filters around them.
    #[test]
    fn unrecognized_lines_are_silently_skipped() {
        let text = "\
# EQ preset for Some Headphone, generated 2026-07-21
GraphicEQ: 20 0.0; 21 0.1; 22 0.2

free-form note a human left here
Preamp: -6.8 dB
Filter 1: ON PK Fc 105 Hz Gain -1.1 dB Q 0.70
";
        let p = parse(text).unwrap();
        assert_eq!(p.preset.preamp_db, -6.8);
        assert_eq!(p.preset.bands.len(), 1);
        assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    }

    #[test]
    fn accepts_explicit_plus_signs() {
        let p = parse("Preamp: +1.5 dB\nFilter 1: ON PK Fc 100 Hz Gain +2.5 dB Q 1.0").unwrap();
        assert_eq!(p.preset.preamp_db, 1.5);
        assert_eq!(p.preset.bands[0].gain_db, 2.5);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn duplicate_preamp_warns_and_last_wins() {
        let p = parse("Preamp: -1 dB\nPreamp: -2 dB").unwrap();
        assert_eq!(p.preset.preamp_db, -2.0);
        assert_eq!(p.warnings.len(), 1, "{:?}", p.warnings);
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
            -15.48,
            &[
                (FilterKind::LowShelf, 105.0, 5.5, 0.70),
                (FilterKind::Peaking, 20.0, 7.6, 6.00),
                (FilterKind::Peaking, 27.4, 5.4, 1.47),
                (FilterKind::Peaking, 100.1, -5.2, 0.61),
                (FilterKind::Peaking, 213.6, -1.2, 1.22),
                (FilterKind::Peaking, 755.3, 1.0, 1.27),
                (FilterKind::Peaking, 1194.7, 1.8, 0.93),
                (FilterKind::Peaking, 3372.2, 3.6, 4.19),
                (FilterKind::Peaking, 4714.6, -5.1, 3.12),
                (FilterKind::HighShelf, 10000.0, -0.5, 0.70),
            ],
        );
    }

    // Edge cases pinning quirks of the original regex grammar, so the
    // nom port stays behavior-compatible.

    #[test]
    fn lowercase_keywords_accepted() {
        let p = parse("preamp: -1 dB\nfilter 1: on pk fc 100 hz gain 1 db q 1.0").unwrap();
        assert_eq!(p.preset.preamp_db, -1.0);
        assert_eq!(p.preset.bands.len(), 1);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn no_space_between_filter_and_number() {
        let p = parse("Filter1: ON PK Fc 100 Hz Gain 1 dB Q 1.0").unwrap();
        assert_eq!(p.preset.bands.len(), 1);
    }

    #[test]
    fn no_space_before_hz_and_db() {
        let p = parse("Filter 1: ON PK Fc 100Hz Gain 1dB Q 1.0").unwrap();
        assert_eq!(p.preset.bands.len(), 1);
        assert_eq!(p.preset.bands[0].fc_hz, 100.0);
        assert_eq!(p.preset.bands[0].gain_db, 1.0);
    }

    #[test]
    fn trailing_garbage_after_q_is_ignored() {
        let p = parse("Filter 1: ON PK Fc 100 Hz Gain 1 dB Q 1.0 whatever").unwrap();
        assert_eq!(p.preset.bands.len(), 1);
    }

    #[test]
    fn space_before_preamp_colon_is_not_a_preamp_line() {
        let p = parse("Preamp : -1 dB").unwrap();
        assert_eq!(p.preset.preamp_db, 0.0);
        assert!(p.warnings.is_empty());
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
