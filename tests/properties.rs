//! Metamorphic properties: implementation-independent invariants of
//! the EQ chain and resampler. Signal-level case counts are kept low —
//! each case processes real audio.

use biquad::{Coefficients, ToHertz, Type};
use clap::Parser;
use oxideq::cli::{Cli, Cmd};
use oxideq::dsp::EqChain;
use oxideq::preset::{Band, FilterKind, Preset};
use proptest::prelude::*;
use std::f64::consts::TAU;

const FS: f64 = 48_000.0;
/// Measurement window: 0.4 s total, gain from the second half.
const WIN: usize = 19_200;

/// Snap a frequency to a whole number of cycles per measurement
/// half-window so RMS windowing leakage doesn't eat the tolerance.
fn snap(freq: f64) -> f64 {
    let half_secs = WIN as f64 / 2.0 / FS;
    (freq * half_secs).round().max(1.0) / half_secs
}

fn process(preset: &Preset, factor: usize, input: &[f32]) -> Vec<f32> {
    let mut chain = EqChain::new(preset, FS, 1, factor).expect("valid chain");
    let mut out = input.to_vec();
    chain.process(&mut out);
    out
}

fn gain_db_at(preset: &Preset, freq: f64, factor: usize) -> f64 {
    let input: Vec<f32> = (0..WIN)
        .map(|i| ((TAU * freq * i as f64 / FS).sin() * 0.25) as f32)
        .collect();
    let out = process(preset, factor, &input);
    let rms = |s: &[f32]| {
        (s.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>() / s.len() as f64).sqrt()
    };
    let half = WIN / 2;
    20.0 * (rms(&out[half..]) / rms(&input[half..])).log10()
}

fn band_strategy() -> impl Strategy<Value = Band> {
    (
        prop_oneof![
            Just(FilterKind::Peaking),
            Just(FilterKind::LowShelf),
            Just(FilterKind::HighShelf),
        ],
        40.0..18_000.0f64,
        -12.0..12.0f64,
        0.3..3.0f64,
    )
        .prop_map(|(kind, fc_hz, gain_db, q)| Band {
            kind,
            fc_hz,
            gain_db,
            q,
        })
}

fn preset_strategy() -> impl Strategy<Value = Preset> {
    (-6.0..0.0f64, prop::collection::vec(band_strategy(), 1..5))
        .prop_map(|(preamp_db, bands)| Preset { preamp_db, bands })
}

fn factor_strategy() -> impl Strategy<Value = usize> {
    prop_oneof![Just(1usize), Just(2), Just(4), Just(8), Just(16)]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]

    #[test]
    fn chain_is_linear(
        preset in preset_strategy(),
        factor in factor_strategy(),
        a in prop::collection::vec(-0.5f32..0.5, 512),
        b in prop::collection::vec(-0.5f32..0.5, 512),
    ) {
        let sum: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        let out_sum = process(&preset, factor, &sum);
        let out_a = process(&preset, factor, &a);
        let out_b = process(&preset, factor, &b);
        for i in 0..sum.len() {
            let lhs = f64::from(out_sum[i]);
            let rhs = f64::from(out_a[i]) + f64::from(out_b[i]);
            prop_assert!((lhs - rhs).abs() < 1e-3, "sample {}: {} vs {}", i, lhs, rhs);
        }
    }

    #[test]
    fn chain_is_time_invariant(
        preset in preset_strategy(),
        factor in factor_strategy(),
        input in prop::collection::vec(-1.0f32..1.0, 512),
        shift in 1usize..64,
    ) {
        let out = process(&preset, factor, &input);
        let mut delayed = vec![0.0f32; shift];
        delayed.extend_from_slice(&input);
        let out_delayed = process(&preset, factor, &delayed);
        // Causal chain, zero initial state, zeros stay exactly zero
        // through FIRs and biquads → bit-identical shifted output.
        prop_assert_eq!(&out_delayed[shift..], &out[..]);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    #[test]
    fn low_band_gain_is_factor_independent(
        fc in 100.0..2_000.0f64,
        gain in -12.0..12.0f64,
        q in 0.5..3.0f64,
    ) {
        // Cramping is negligible at low fc, so gain at fc must not
        // depend on the factor. Catches designing coefficients at the
        // wrong rate (fc would shift by the factor).
        let fc = snap(fc);
        let p = Preset {
            preamp_db: 0.0,
            bands: vec![Band { kind: FilterKind::Peaking, fc_hz: fc, gain_db: gain, q }],
        };
        let g1 = gain_db_at(&p, fc, 1);
        let g4 = gain_db_at(&p, fc, 4);
        prop_assert!((g1 - g4).abs() < 0.05, "1x {} dB vs 4x {} dB at {} Hz", g1, g4, fc);
    }

    #[test]
    fn measured_gain_matches_the_transfer_function(
        fc in 200.0..10_000.0f64,
        gain in -12.0..12.0f64,
        q in 0.5..3.0f64,
        probe in 100.0..20_000.0f64,
    ) {
        let probe = snap(probe);
        let p = Preset {
            preamp_db: 0.0,
            bands: vec![Band { kind: FilterKind::Peaking, fc_hz: fc, gain_db: gain, q }],
        };
        let c = Coefficients::<f64>::from_params(
            Type::PeakingEQ(gain), FS.hz(), fc.hz(), q,
        ).expect("valid params");
        let w = TAU * probe / FS;
        let (c1, s1) = (w.cos(), -w.sin());
        let (c2, s2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let nr = c.b0 + c.b1 * c1 + c.b2 * c2;
        let ni = c.b1 * s1 + c.b2 * s2;
        let dr = 1.0 + c.a1 * c1 + c.a2 * c2;
        let di = c.a1 * s1 + c.a2 * s2;
        let expected = 10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10();
        let measured = gain_db_at(&p, probe, 1);
        prop_assert!(
            (measured - expected).abs() < 0.05,
            "measured {} dB vs |H| {} dB at {} Hz", measured, expected, probe
        );
    }
}

proptest! {
    #[test]
    fn cli_accepts_exactly_the_power_of_two_factors(n in 0u32..64) {
        let arg = n.to_string();
        let res = Cli::try_parse_from([
            "oxideq", "run", "--preset", "p.txt", "--oversample", &arg,
        ]);
        let valid = matches!(n, 1 | 2 | 4 | 8 | 16);
        prop_assert_eq!(res.is_ok(), valid, "n = {}", n);
        if valid {
            let Cmd::Run(a) = res.expect("parsed").cmd else {
                panic!("expected Run");
            };
            prop_assert_eq!(a.oversample, n as usize);
        }
    }
}
