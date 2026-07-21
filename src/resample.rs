//! Halfband polyphase resamplers for power-of-two oversampling.
//!
//! Construction computes Kaiser-windowed-sinc halfband taps; all
//! `push`/`up`/`down` paths are real-time safe: no allocation, locks,
//! or I/O.

use std::f64::consts::PI;

/// Stopband attenuation target for every halfband stage, dB.
#[allow(dead_code)] // consumed by the resampler stages built in Task 4
const ATTEN_DB: f64 = 120.0;

/// Maximum supported oversampling factor (2^4).
pub const MAX_FACTOR: usize = 16;

/// Modified Bessel function of the first kind, order zero.
/// Power series; converges quickly for the β ≤ ~13 used here.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let mut k = 1.0;
    loop {
        let f = x / (2.0 * k);
        term *= f * f;
        sum += term;
        if term < sum * 1e-18 {
            return sum;
        }
        k += 1.0;
    }
}

/// Kaiser-windowed-sinc halfband lowpass for a 2× stage whose *input*
/// rate is `fs_in` Hz: cutoff `fs_in/2` (a quarter of the output rate).
///
/// Length is ≡ 3 (mod 4) so every even-offset tap is exactly zero and
/// the polyphase odd branch degenerates to the center tap (pure delay).
/// Normalized to unity DC gain (`sum == 1`), matching
/// `scipy.signal.firwin`'s default scaling.
pub fn halfband_taps(fs_in: f64, passband_hz: f64, atten_db: f64) -> Vec<f64> {
    let fp = passband_hz.min(0.45 * fs_in);
    // Transition width fs_in - 2·fp Hz, normalized to the output rate
    // 2·fs_in: Δw = 2π(fs_in - 2fp)/(2fs_in) = π(1 - 2fp/fs_in).
    let delta_w = PI * (1.0 - 2.0 * fp / fs_in);
    let beta = 0.1102 * (atten_db - 8.7);
    let n_est = (atten_db - 7.95) / (2.285 * delta_w) + 1.0;
    let mut n = n_est.ceil() as usize;
    while n % 4 != 3 {
        n += 1;
    }
    let m = (n - 1) / 2;
    let denom = bessel_i0(beta);
    let mut taps: Vec<f64> = (0..n)
        .map(|i| {
            let k = i as i64 - m as i64;
            let ideal = if k == 0 {
                0.5
            } else if k % 2 == 0 {
                0.0
            } else {
                let kf = k as f64;
                (PI * kf / 2.0).sin() / (PI * kf)
            };
            let frac = k as f64 / m as f64;
            ideal * bessel_i0(beta * (1.0 - frac * frac).sqrt()) / denom
        })
        .collect();
    let sum: f64 = taps.iter().sum();
    for t in &mut taps {
        *t /= sum;
    }
    taps
}

#[cfg(test)]
mod tests {
    use super::*;

    /// |H(e^jw)| in dB of a tap set at normalized w (rad/sample at the
    /// stage's output rate).
    fn gain_db(taps: &[f64], w: f64) -> f64 {
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for (n, &t) in taps.iter().enumerate() {
            re += t * (w * n as f64).cos();
            im -= t * (w * n as f64).sin();
        }
        10.0 * (re * re + im * im).log10()
    }

    #[test]
    fn taps_have_halfband_structure() {
        for fs in [44_100.0, 48_000.0, 88_200.0, 96_000.0, 192_000.0] {
            let t = halfband_taps(fs, 20_000.0, 120.0);
            assert_eq!(t.len() % 4, 3, "length must be 4k+3 at fs {fs}");
            let m = (t.len() - 1) / 2;
            for (i, &tap) in t.iter().enumerate() {
                let off = i as i64 - m as i64;
                if off != 0 && off % 2 == 0 {
                    assert_eq!(tap, 0.0, "even offset {off} not zero at fs {fs}");
                }
                assert_eq!(tap, t[t.len() - 1 - i], "asymmetric at {i}, fs {fs}");
            }
            let sum: f64 = t.iter().sum();
            assert!((sum - 1.0).abs() < 1e-12, "DC gain {sum} at fs {fs}");
        }
    }

    #[test]
    fn stopband_meets_118_db() {
        for fs in [44_100.0, 48_000.0] {
            let t = halfband_taps(fs, 20_000.0, 120.0);
            let fp = 20_000f64.min(0.45 * fs);
            // Stopband edge at the output rate: f = fs - fp  →  w = π(1 - fp/fs).
            let w_stop = PI * (1.0 - fp / fs);
            for k in 0..=400 {
                let w = w_stop + (PI - w_stop) * f64::from(k) / 400.0;
                let g = gain_db(&t, w);
                assert!(g < -118.0, "only {g:.1} dB at w {w:.3}, fs {fs}");
            }
        }
    }

    #[test]
    fn passband_is_flat() {
        let fs = 48_000.0;
        let t = halfband_taps(fs, 20_000.0, 120.0);
        // gain_db's w is rad/sample at the OUTPUT rate 2·fs, so a tone
        // at f Hz sits at w = 2π·f/(2fs) = π·f/fs. Passband edge 20 kHz.
        let w_pass = PI * 20_000.0 / fs;
        for k in 0..=200 {
            let w = w_pass * f64::from(k) / 200.0;
            let g = gain_db(&t, w);
            assert!(g.abs() < 1e-4, "ripple {g} dB at w {w:.3}");
        }
    }
}
