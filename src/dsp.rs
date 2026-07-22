//! Preamp + pluggable filter cascade (v1 backend: Direct Form 1 biquads).
//!
//! Everything is allocated in construction; `process` is free of
//! allocation, locks, and I/O and is safe to call from a real-time
//! audio callback. Buffers are f32 (device format); all arithmetic,
//! coefficients, and state are f64.

use anyhow::{anyhow, Result};
use biquad::{Biquad, Coefficients, DirectForm1, ToHertz, Type};

use crate::preset::{Band, FilterKind, Preset};
use crate::resample::{Oversampler, MAX_FACTOR};

pub fn db_to_linear(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// One mono filter stage. Implementations hold *per-channel* state —
/// `EqChain` builds an independent instance per channel.
///
/// Samples are f64: the device boundary is f32, but all DSP arithmetic
/// runs in 64-bit (Design Decision 3).
///
/// Real-time contract: `run` is called once per sample from the audio
/// callback and must not allocate, lock, or perform I/O.
pub trait Filter: Send {
    fn run(&mut self, sample: f64) -> f64;
}

/// One stage in the cascade. Biquads are stored concretely so the inner
/// loop inlines them — no vtable call per sample on the v1 path; anything
/// else plugs in through the boxed `Filter` trait. Boxing happens once at
/// construction, never in the audio path.
pub enum Stage {
    Biquad(DirectForm1<f64>),
    Custom(Box<dyn Filter>),
}

impl Stage {
    #[inline]
    fn run(&mut self, sample: f64) -> f64 {
        match self {
            Stage::Biquad(b) => Biquad::run(b, sample),
            Stage::Custom(f) => f.run(sample),
        }
    }
}

pub struct EqChain {
    preamp: f64, // linear
    channels: usize,
    /// One cascade per channel: `stages[ch][stage]`.
    stages: Vec<Vec<Stage>>,
    /// One resampler per channel when oversampling; `None` at factor 1
    /// (structural no-op — the process loop is byte-for-byte today's).
    os: Option<Vec<Oversampler>>,
}

impl EqChain {
    /// Biquad-backed chain from an APO preset. `oversample` ∈
    /// {1, 2, 4, 8, 16}: above 1, the cascade runs at
    /// `oversample × sample_rate_hz` behind halfband resamplers.
    pub fn new(
        preset: &Preset,
        sample_rate_hz: f64,
        channels: usize,
        oversample: usize,
    ) -> Result<Self> {
        anyhow::ensure!(
            matches!(oversample, 1 | 2 | 4 | 8 | 16),
            "oversample factor must be 1, 2, 4, 8, or 16 (got {oversample})"
        );
        // Coefficient design at N·fs would accept fc up to N·fs/2 —
        // enforce the *device* Nyquist limit explicitly.
        if let Some(b) = preset
            .bands
            .iter()
            .find(|b| b.fc_hz >= sample_rate_hz / 2.0)
        {
            return Err(anyhow!(
                "band Fc {} Hz is at or above device Nyquist ({} Hz)",
                b.fc_hz,
                sample_rate_hz / 2.0
            ));
        }
        let internal_rate = sample_rate_hz * oversample as f64;
        let stages = (0..channels)
            .map(|_| {
                preset
                    .bands
                    .iter()
                    .map(|b| {
                        let c = coefficients(b, internal_rate)?;
                        Ok(Stage::Biquad(DirectForm1::<f64>::new(c)))
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        let os = (oversample > 1).then(|| {
            (0..channels)
                .map(|_| Oversampler::new(oversample, sample_rate_hz))
                .collect()
        });
        let mut chain = Self::from_stages(preset.preamp_db, stages);
        chain.os = os;
        Ok(chain)
    }

    /// Chain over arbitrary stages: `stages[ch][stage]`. The outer length
    /// is the channel count; every channel owns its own stage instances.
    pub fn from_stages(preamp_db: f64, stages: Vec<Vec<Stage>>) -> Self {
        Self {
            preamp: db_to_linear(preamp_db),
            channels: stages.len(),
            stages,
            os: None,
        }
    }

    /// Process an interleaved f32 buffer in place (`len` must be a
    /// multiple of the channel count). Zero-copy: each sample is widened
    /// to f64 in a register, run through preamp + cascade, and written
    /// back where it lay. Real-time safe: no allocation, locks, or I/O.
    pub fn process(&mut self, interleaved: &mut [f32]) {
        debug_assert_eq!(interleaved.len() % self.channels, 0);
        match &mut self.os {
            None => {
                for frame in interleaved.chunks_exact_mut(self.channels) {
                    for (sample, cascade) in frame.iter_mut().zip(self.stages.iter_mut()) {
                        let mut x = f64::from(*sample) * self.preamp;
                        for f in cascade.iter_mut() {
                            x = f.run(x);
                        }
                        *sample = x as f32; // intentional narrowing back to device format
                    }
                }
            }
            Some(os) => {
                let mut buf = [0.0f64; MAX_FACTOR];
                for frame in interleaved.chunks_exact_mut(self.channels) {
                    for ((sample, cascade), rs) in frame
                        .iter_mut()
                        .zip(self.stages.iter_mut())
                        .zip(os.iter_mut())
                    {
                        let x = f64::from(*sample) * self.preamp;
                        let n = rs.up(x, &mut buf);
                        for s in &mut buf[..n] {
                            for f in cascade.iter_mut() {
                                *s = f.run(*s);
                            }
                        }
                        *sample = rs.down(&mut buf) as f32;
                    }
                }
            }
        }
    }

    pub fn num_bands(&self) -> usize {
        self.stages.first().map_or(0, Vec::len)
    }

    /// Resampler group delay in device-rate frames (0.0 when
    /// oversampling is off).
    pub fn latency_frames(&self) -> f64 {
        self.os
            .as_ref()
            .and_then(|v| v.first())
            .map_or(0.0, Oversampler::latency_frames)
    }
}

fn coefficients(band: &Band, fs: f64) -> Result<Coefficients<f64>> {
    let ty = match band.kind {
        FilterKind::Peaking => Type::PeakingEQ(band.gain_db),
        FilterKind::LowShelf => Type::LowShelf(band.gain_db),
        FilterKind::HighShelf => Type::HighShelf(band.gain_db),
    };
    Coefficients::<f64>::from_params(ty, fs.hz(), band.fc_hz.hz(), band.q)
        .map_err(|e| anyhow!("invalid filter (Fc {} Hz, Q {}): {e:?}", band.fc_hz, band.q))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::{Band, FilterKind, Preset};
    use std::f32::consts::TAU;

    fn sine(freq: f32, fs: f32, secs: f32) -> Vec<f32> {
        (0..(fs * secs) as usize)
            .map(|i| (TAU * freq * i as f32 / fs).sin() * 0.25)
            .collect()
    }

    fn rms(s: &[f32]) -> f32 {
        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
    }

    /// Steady-state gain of `preset` at `freq`, in dB (second half of 1 s).
    fn gain_db_at(preset: &Preset, freq: f32, fs: f32) -> f32 {
        let mut chain = EqChain::new(preset, fs as f64, 1, 1).unwrap();
        let input = sine(freq, fs, 1.0);
        let mut output = input.clone();
        chain.process(&mut output);
        let half = input.len() / 2;
        20.0 * (rms(&output[half..]) / rms(&input[half..])).log10()
    }

    /// Analog RBJ peaking prototype magnitude at `f` Hz, in dB.
    fn analog_peaking_db(fc: f64, gain_db: f64, q: f64, f: f64) -> f64 {
        let a = 10f64.powf(gain_db / 40.0);
        let u = f / fc;
        let num = (1.0 - u * u).powi(2) + (u * a / q).powi(2);
        let den = (1.0 - u * u).powi(2) + (u / (a * q)).powi(2);
        10.0 * (num / den).log10()
    }

    /// Steady-state gain like `gain_db_at`, with an oversample factor.
    fn gain_db_at_os(preset: &Preset, freq: f32, fs: f32, factor: usize) -> f32 {
        let mut chain = EqChain::new(preset, fs as f64, 1, factor).unwrap();
        let input = sine(freq, fs, 1.0);
        let mut output = input.clone();
        chain.process(&mut output);
        let half = input.len() / 2;
        20.0 * (rms(&output[half..]) / rms(&input[half..])).log10()
    }

    fn one_band(kind: FilterKind, fc_hz: f64, gain_db: f64, q: f64) -> Preset {
        Preset {
            preamp_db: 0.0,
            bands: vec![Band {
                kind,
                fc_hz,
                gain_db,
                q,
            }],
        }
    }

    #[test]
    fn preamp_is_exact_stateless_linear_gain() {
        let preset = Preset {
            preamp_db: -6.020_6,
            bands: vec![],
        }; // ≈ ×0.5
        let mut chain = EqChain::new(&preset, 48_000.0, 2, 1).unwrap();
        let mut buf = [1.0f32, -1.0, 0.5, 0.25];
        chain.process(&mut buf);
        for (got, want) in buf.iter().zip([0.5f32, -0.5, 0.25, 0.125]) {
            assert!((got - want).abs() < 1e-4, "{got} vs {want}");
        }
    }

    #[test]
    fn flat_preset_is_bit_identical() {
        // preamp 0 dB → linear gain exactly 1.0; f32→f64 widening is
        // lossless and ×1.0 round-trips exactly → bit-exact no-op.
        let mut chain = EqChain::new(&Preset::default(), 48_000.0, 2, 1).unwrap();
        let input = sine(997.0, 48_000.0, 0.1);
        let mut output = input.clone();
        chain.process(&mut output);
        assert_eq!(input, output);
    }

    #[test]
    fn peaking_boosts_at_fc_and_leaves_far_field_alone() {
        let p = one_band(FilterKind::Peaking, 1_000.0, 6.0, 1.0);
        assert!((gain_db_at(&p, 1_000.0, 48_000.0) - 6.0).abs() < 0.3);
        assert!(gain_db_at(&p, 60.0, 48_000.0).abs() < 0.5);
        assert!(gain_db_at(&p, 15_000.0, 48_000.0).abs() < 0.5);
    }

    #[test]
    fn low_shelf_boosts_lows_only() {
        let p = one_band(FilterKind::LowShelf, 105.0, 6.0, 0.7);
        assert!((gain_db_at(&p, 20.0, 48_000.0) - 6.0).abs() < 0.5);
        assert!(gain_db_at(&p, 5_000.0, 48_000.0).abs() < 0.3);
    }

    #[test]
    fn high_shelf_cuts_highs_only() {
        let p = one_band(FilterKind::HighShelf, 10_000.0, -4.0, 0.7);
        assert!((gain_db_at(&p, 20_000.0, 48_000.0) + 4.0).abs() < 0.5);
        assert!(gain_db_at(&p, 200.0, 48_000.0).abs() < 0.3);
    }

    #[test]
    fn channels_have_independent_filter_state() {
        let p = one_band(FilterKind::Peaking, 1_000.0, 6.0, 1.0);
        let mono = sine(1_000.0, 48_000.0, 0.05);

        // Stereo: L = sine, R = digital silence.
        let mut stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, 0.0]).collect();
        EqChain::new(&p, 48_000.0, 2, 1)
            .unwrap()
            .process(&mut stereo);

        let mut mono_out = mono.clone();
        EqChain::new(&p, 48_000.0, 1, 1)
            .unwrap()
            .process(&mut mono_out);

        for (i, frame) in stereo.chunks(2).enumerate() {
            assert_eq!(
                frame[0], mono_out[i],
                "L sample {i} diverged from mono reference"
            );
            assert_eq!(frame[1], 0.0, "R sample {i} contaminated by L state");
        }
    }

    #[test]
    fn rejects_band_above_nyquist() {
        let p = one_band(FilterKind::Peaking, 30_000.0, 3.0, 1.0);
        assert!(EqChain::new(&p, 48_000.0, 2, 1).is_err());
    }

    /// A non-biquad stage plugged in through the `Filter` trait.
    struct Inverter;
    impl Filter for Inverter {
        fn run(&mut self, sample: f64) -> f64 {
            -sample
        }
    }

    #[test]
    fn custom_filters_plug_in_via_the_trait() {
        let stages: Vec<Vec<Stage>> = vec![
            vec![Stage::Custom(Box::new(Inverter))],
            vec![Stage::Custom(Box::new(Inverter))],
        ];
        let mut chain = EqChain::from_stages(0.0, stages);
        let mut buf = [0.25f32, -0.5, 1.0, 0.0];
        chain.process(&mut buf);
        assert_eq!(buf, [-0.25, 0.5, -1.0, 0.0]);
    }

    #[test]
    fn rejects_invalid_oversample_factor() {
        for bad in [0usize, 3, 5, 6, 32] {
            assert!(
                EqChain::new(&Preset::default(), 48_000.0, 2, bad).is_err(),
                "factor {bad} must be rejected"
            );
        }
    }

    #[test]
    fn oversampled_chain_still_rejects_band_above_device_nyquist() {
        // Coefficients at 4×fs would happily accept 30 kHz — the
        // explicit device-Nyquist check must still reject it.
        let p = one_band(FilterKind::Peaking, 30_000.0, 3.0, 1.0);
        assert!(EqChain::new(&p, 48_000.0, 2, 4).is_err());
    }

    #[test]
    fn factor_one_reports_zero_latency() {
        let chain = EqChain::new(&Preset::default(), 48_000.0, 2, 1).unwrap();
        assert_eq!(chain.latency_frames(), 0.0);
    }

    #[test]
    fn oversampling_uncramps_the_upper_skirt() {
        // RBJ peaking is EXACT at fc at any rate (bilinear prewarp);
        // cramping pinches the upper skirt toward 0 dB at device
        // Nyquist. Probe the skirt at 19.5 kHz — INSIDE the resampler
        // passband edge (min(20 kHz, 0.45·44100) = 19.845 kHz), so the
        // measurement sees cramping, not resampler transition droop.
        // Oracle = analog prototype.
        let p = one_band(FilterKind::Peaking, 18_000.0, 6.0, 2.0);
        let analog = analog_peaking_db(18_000.0, 6.0, 2.0, 19_500.0);
        let g1 = f64::from(gain_db_at_os(&p, 19_500.0, 44_100.0, 1));
        let g4 = f64::from(gain_db_at_os(&p, 19_500.0, 44_100.0, 4));
        assert!(
            (g4 - analog).abs() < 0.2,
            "4x gain {g4} dB should match analog {analog} dB"
        );
        assert!(
            (g1 - analog).abs() > 0.5,
            "1x gain {g1} dB should be visibly cramped vs analog {analog} dB"
        );
    }

    #[test]
    fn channels_stay_independent_under_oversampling() {
        let p = one_band(FilterKind::Peaking, 1_000.0, 6.0, 1.0);
        let mono = sine(1_000.0, 48_000.0, 0.05);
        let mut stereo: Vec<f32> = mono.iter().flat_map(|&s| [s, 0.0]).collect();
        EqChain::new(&p, 48_000.0, 2, 4)
            .unwrap()
            .process(&mut stereo);
        let mut mono_out = mono.clone();
        EqChain::new(&p, 48_000.0, 1, 4)
            .unwrap()
            .process(&mut mono_out);
        for (i, frame) in stereo.chunks(2).enumerate() {
            assert_eq!(frame[0], mono_out[i], "L sample {i} diverged");
            assert_eq!(frame[1], 0.0, "R sample {i} contaminated");
        }
    }
}
