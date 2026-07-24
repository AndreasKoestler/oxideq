//! Halfband polyphase resamplers for power-of-two oversampling.
//!
//! Construction computes Kaiser-windowed-sinc halfband taps; all
//! `push`/`up`/`down` paths are real-time safe: no allocation, locks,
//! or I/O.

use std::f64::consts::PI;

/// Stopband attenuation target for every halfband stage, dB.
const ATTEN_DB: f64 = 120.0;

/// Maximum supported oversampling factor (2^4).
pub const MAX_FACTOR: usize = 16;

/// Modified Bessel function of the first kind, order zero.
/// Power series; converges quickly for the β ≤ ~13 used here.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    // Bounded: a non-finite `x` never satisfies the convergence test
    // (NaN comparisons are false), so an unbounded loop could hang.
    // Real inputs converge in far fewer than 100 terms.
    for k in 1..=100u32 {
        let f = x / (2.0 * f64::from(k));
        term *= f * f;
        sum += term;
        if term < sum * 1e-18 {
            break;
        }
    }
    sum
}

/// Kaiser-windowed-sinc halfband lowpass for a 2× stage whose *input*
/// rate is `fs_in` Hz: cutoff `fs_in/2` (a quarter of the output rate).
///
/// Length is ≡ 3 (mod 4) so every even-offset tap is exactly zero and
/// the polyphase odd branch degenerates to the center tap (pure delay).
/// Normalized to unity DC gain (`sum == 1`), matching
/// `scipy.signal.firwin`'s default scaling.
#[must_use]
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

/// Polyphase 2× interpolator. Feed one sample at `fs_in`, get two at
/// `2·fs_in`. Even output phase is the sinc branch; odd phase is the
/// (scaled) center-tap delay.
#[derive(Debug)]
pub struct Upsampler2x {
    /// Even-index taps of the ×2-scaled prototype (the non-zero branch).
    branch: Vec<f64>,
    /// ×2-scaled center tap (≈ 1.0 exactly up to window normalization).
    center: f64,
    /// Center-branch delay in input samples: (m-1)/2.
    center_delay: usize,
    /// Ring buffer of past inputs; len == `branch.len()`.
    buf: Vec<f64>,
    pos: usize,
}

impl Upsampler2x {
    #[must_use]
    pub fn new(taps: &[f64]) -> Self {
        debug_assert!(
            taps.len() % 4 == 3,
            "halfband taps must have length 4k+3 (got {})",
            taps.len()
        );
        let m = (taps.len() - 1) / 2;
        let branch: Vec<f64> = taps.iter().step_by(2).map(|&t| 2.0 * t).collect();
        Self {
            center: 2.0 * taps[m],
            center_delay: (m - 1) / 2,
            buf: vec![0.0; branch.len()],
            branch,
            pos: 0,
        }
    }

    /// One input sample in, two output samples out, in time order.
    #[inline]
    pub fn push(&mut self, x: f64) -> [f64; 2] {
        let n = self.buf.len();
        self.buf[self.pos] = x;

        // Newest-first ring convolution as two contiguous slices instead of
        // an indexed walk with manual wrap: the compiler can't prove a
        // wrapped index in range and emits a per-tap bounds check that
        // blocks unrolling, while slice iterators carry their own length.
        // Tap visitation order is unchanged, so the f64 accumulation order —
        // and the output — stays bit-identical.
        let (buf_lo, buf_hi) = self.buf.split_at(self.pos + 1);
        let (br_lo, br_hi) = self.branch.split_at(self.pos + 1);
        let mut acc = 0.0;
        for (&c, &s) in br_lo.iter().zip(buf_lo.iter().rev()) {
            acc += c * s;
        }
        for (&c, &s) in br_hi.iter().zip(buf_hi.iter().rev()) {
            acc += c * s;
        }

        let delayed = self.buf[(self.pos + n - self.center_delay) % n];
        self.pos = (self.pos + 1) % n;
        [acc, self.center * delayed]
    }
}

/// Polyphase 2× decimator over the same (unscaled) halfband prototype.
#[derive(Debug)]
pub struct Decimator2x {
    /// Even-index taps (the non-zero branch).
    branch: Vec<f64>,
    /// Center tap (≈ 0.5).
    center: f64,
    /// Center-branch delay in odd-phase samples: (m-1)/2 + 1.
    center_delay: usize,
    even: Vec<f64>,
    odd: Vec<f64>,
    epos: usize,
    opos: usize,
}

impl Decimator2x {
    #[must_use]
    pub fn new(taps: &[f64]) -> Self {
        debug_assert!(
            taps.len() % 4 == 3,
            "halfband taps must have length 4k+3 (got {})",
            taps.len()
        );
        let m = (taps.len() - 1) / 2;
        let branch: Vec<f64> = taps.iter().step_by(2).copied().collect();
        let center_delay = (m - 1) / 2 + 1;
        Self {
            center: taps[m],
            center_delay,
            even: vec![0.0; branch.len()],
            odd: vec![0.0; center_delay + 1],
            branch,
            epos: 0,
            opos: 0,
        }
    }

    /// Two consecutive 2×-rate samples in (time order), one 1×-rate
    /// sample out.
    #[inline]
    pub fn push(&mut self, v0: f64, v1: f64) -> f64 {
        let ne = self.even.len();
        let no = self.odd.len();
        self.even[self.epos] = v0;
        self.odd[self.opos] = v1;

        // Same two-slice ring convolution as `Upsampler2x::push` (bounds-check
        // elision, identical accumulation order).
        let (ev_lo, ev_hi) = self.even.split_at(self.epos + 1);
        let (br_lo, br_hi) = self.branch.split_at(self.epos + 1);
        let mut acc = 0.0;
        for (&c, &s) in br_lo.iter().zip(ev_lo.iter().rev()) {
            acc += c * s;
        }
        for (&c, &s) in br_hi.iter().zip(ev_hi.iter().rev()) {
            acc += c * s;
        }

        let delayed = self.odd[(self.opos + no - self.center_delay) % no];
        self.epos = (self.epos + 1) % ne;
        self.opos = (self.opos + 1) % no;
        acc + self.center * delayed
    }
}

/// Cascaded halfband stages for oversampling by 2^k, k in 1..=4.
/// Stage `s` (0-based) resamples between `device_rate·2^s` and
/// `device_rate·2^(s+1)`.
#[derive(Debug)]
pub struct Oversampler {
    up: Vec<Upsampler2x>,
    down: Vec<Decimator2x>,
    factor: usize,
    latency: f64,
    /// Reused input buffer for `up`: each stage reads its `n` live inputs
    /// from here before scattering outputs into `out`. Pre-allocated so the
    /// per-sample copy is `n` slots, not the whole `MAX_FACTOR` array.
    scratch: [f64; MAX_FACTOR],
}

impl Oversampler {
    /// `factor` must be 2, 4, 8, or 16 — validated by the caller
    /// (`EqChain::new`); this constructor panics on anything else.
    ///
    /// # Panics
    /// Panics if `factor` is not one of 2, 4, 8, or 16.
    #[must_use]
    pub fn new(factor: usize, device_rate: f64) -> Self {
        assert!(
            matches!(factor, 2 | 4 | 8 | 16),
            "unsupported oversample factor {factor}"
        );
        let stages = factor.trailing_zeros() as usize;
        let mut up = Vec::with_capacity(stages);
        let mut down = Vec::with_capacity(stages);
        let mut latency = 0.0;
        for s in 0..stages {
            let fs_in = device_rate * f64::from(1 << s);
            let taps = halfband_taps(fs_in, 20_000.0, ATTEN_DB);
            // The up+down pair at this stage delays (len-1) samples at
            // 2^(s+1)·device_rate, i.e. (len-1)/2^(s+1) device frames.
            latency += (taps.len() - 1) as f64 / f64::from(1 << (s + 1));
            up.push(Upsampler2x::new(&taps));
            down.push(Decimator2x::new(&taps));
        }
        Self {
            up,
            down,
            factor,
            latency,
            scratch: [0.0; MAX_FACTOR],
        }
    }

    /// Expand one device-rate sample into `factor` samples at the
    /// oversampled rate. Returns the count written.
    #[inline]
    pub fn up(&mut self, x: f64, out: &mut [f64; MAX_FACTOR]) -> usize {
        out[0] = x;
        let mut n = 1;
        let scratch = &mut self.scratch;
        for stage in &mut self.up {
            // push() must see inputs in time order, and writing 2i/2i+1
            // straight into `out` would clobber not-yet-read inputs, so the
            // live prefix is staged in `scratch` first.
            scratch[..n].copy_from_slice(&out[..n]);
            for (i, &s) in scratch[..n].iter().enumerate() {
                let [a, b] = stage.push(s);
                out[2 * i] = a;
                out[2 * i + 1] = b;
            }
            n *= 2;
        }
        n
    }

    /// Collapse the first `factor` slots of `samples` back to one
    /// device-rate sample. Consumes the buffer in place.
    #[inline]
    pub fn down(&mut self, samples: &mut [f64; MAX_FACTOR]) -> f64 {
        let mut n = self.factor;
        // Highest-rate stage decimates first.
        for stage in self.down.iter_mut().rev() {
            for i in 0..n / 2 {
                samples[i] = stage.push(samples[2 * i], samples[2 * i + 1]);
            }
            n /= 2;
        }
        samples[0]
    }

    /// Total group delay in device-rate frames. Integer at 2×,
    /// fractional at higher factors.
    #[must_use]
    pub fn latency_frames(&self) -> f64 {
        self.latency
    }
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

    #[test]
    fn upsampler_impulse_response_is_the_scaled_prototype() {
        let taps = halfband_taps(48_000.0, 20_000.0, 120.0);
        let mut up = Upsampler2x::new(&taps);
        let mut out = Vec::new();
        for t in 0..taps.len() {
            let x = if t == 0 { 1.0 } else { 0.0 };
            out.extend(up.push(x));
        }
        // y[n] = g[n] = 2·taps[n]: the impulse response IS the filter.
        for (n, &y) in out.iter().enumerate().take(taps.len()) {
            assert!((y - 2.0 * taps[n]).abs() < 1e-15, "sample {n}: {y}");
        }
    }

    #[test]
    fn upsampler_passes_dc() {
        let taps = halfband_taps(48_000.0, 20_000.0, 120.0);
        let mut up = Upsampler2x::new(&taps);
        let mut last = [0.0f64; 2];
        for _ in 0..2 * taps.len() {
            last = up.push(1.0);
        }
        assert!((last[0] - 1.0).abs() < 1e-5, "even phase {}", last[0]);
        assert!((last[1] - 1.0).abs() < 1e-5, "odd phase {}", last[1]);
    }

    #[test]
    fn decimator_impulse_responses_match_tap_phases() {
        let taps = halfband_taps(48_000.0, 20_000.0, 120.0);
        // Impulse on the even input phase: y[t] = taps[2t].
        let mut d = Decimator2x::new(&taps);
        for t in 0..taps.len() {
            let y = d.push(if t == 0 { 1.0 } else { 0.0 }, 0.0);
            let expect = taps.get(2 * t).copied().unwrap_or(0.0);
            assert!((y - expect).abs() < 1e-15, "even-phase sample {t}: {y}");
        }
        // Impulse on the odd input phase: y[t] = taps[2t-1].
        let mut d = Decimator2x::new(&taps);
        for t in 0..taps.len() {
            let y = d.push(0.0, if t == 0 { 1.0 } else { 0.0 });
            let expect = if t == 0 {
                0.0
            } else {
                taps.get(2 * t - 1).copied().unwrap_or(0.0)
            };
            assert!((y - expect).abs() < 1e-15, "odd-phase sample {t}: {y}");
        }
    }

    #[test]
    fn decimator_passes_dc_and_rejects_input_nyquist() {
        let taps = halfband_taps(48_000.0, 20_000.0, 120.0);
        let mut d = Decimator2x::new(&taps);
        let mut last = 0.0;
        for _ in 0..2 * taps.len() {
            last = d.push(1.0, 1.0);
        }
        assert!((last - 1.0).abs() < 1e-5, "DC gain {last}");

        // +1,-1 alternating at 2·fs is a tone at fs — mid-stopband; it
        // would alias to DC if the filter leaked.
        let mut d = Decimator2x::new(&taps);
        let mut peak = 0.0f64;
        for t in 0..4 * taps.len() {
            let y = d.push(1.0, -1.0);
            if t > 2 * taps.len() {
                peak = peak.max(y.abs());
            }
        }
        assert!(peak < 1e-5, "stopband leak {peak}");
    }

    fn sine64(freq: f64, fs: f64, n: usize) -> Vec<f64> {
        use std::f64::consts::TAU;
        (0..n).map(|i| (TAU * freq * i as f64 / fs).sin()).collect()
    }

    #[test]
    fn round_trip_2x_is_a_pure_integer_delay() {
        let fs = 48_000.0;
        let mut os = Oversampler::new(2, fs);
        let lat = os.latency_frames();
        assert_eq!(lat.fract(), 0.0, "2x delay must be integer, got {lat}");
        let d = lat as usize;
        let input = sine64(1_000.0, fs, 4 * d + 4_800);
        let mut buf = [0.0f64; MAX_FACTOR];
        let out: Vec<f64> = input
            .iter()
            .map(|&x| {
                let n = os.up(x, &mut buf);
                assert_eq!(n, 2);
                os.down(&mut buf)
            })
            .collect();
        // -110 dBFS residual bound on a unit sine. Skip the first `d`
        // outputs: until t >= d they still draw on the zero-initialized
        // pre-roll — a causal-filter startup transient, not a resampler
        // defect.
        for t in d..input.len() - d {
            assert!(
                (out[t + d] - input[t]).abs() < 3.2e-6,
                "sample {t}: {} vs {}",
                out[t + d],
                input[t]
            );
        }
    }

    #[test]
    fn round_trip_higher_factors_are_rms_transparent() {
        let fs = 48_000.0;
        for factor in [4usize, 8, 16] {
            for freq in [100.0, 1_000.0, 10_000.0, 19_000.0] {
                let mut os = Oversampler::new(factor, fs);
                let input = sine64(freq, fs, 24_000);
                let mut buf = [0.0f64; MAX_FACTOR];
                let out: Vec<f64> = input
                    .iter()
                    .map(|&x| {
                        os.up(x, &mut buf);
                        os.down(&mut buf)
                    })
                    .collect();
                let rms =
                    |s: &[f64]| (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt();
                let half = input.len() / 2;
                let db = 20.0 * (rms(&out[half..]) / rms(&input[half..])).log10();
                assert!(db.abs() < 0.05, "{factor}x @ {freq} Hz: {db} dB");
            }
        }
    }

    #[test]
    fn impulse_peak_lands_at_reported_latency() {
        for factor in [2usize, 4, 8, 16] {
            let mut os = Oversampler::new(factor, 48_000.0);
            let lat = os.latency_frames();
            let mut buf = [0.0f64; MAX_FACTOR];
            let (mut peak_t, mut peak_v) = (0usize, 0.0f64);
            for t in 0..lat.ceil() as usize + 64 {
                let x = if t == 0 { 1.0 } else { 0.0 };
                os.up(x, &mut buf);
                let y = os.down(&mut buf).abs();
                if y > peak_v {
                    (peak_t, peak_v) = (t, y);
                }
            }
            assert!(
                (peak_t as f64 - lat).abs() <= 0.5,
                "factor {factor}: peak at {peak_t}, latency {lat}"
            );
            // Fractional `lat` (factor >= 4) puts the continuous impulse
            // peak *between* output samples, so the best single sample is
            // bounded by sinc(frac), frac = distance to the nearest sample
            // (0.637 at frac 0.5) — hence 10% of that ceiling, not an
            // unconditional 0.9.
            let frac = lat.fract().min(1.0 - lat.fract());
            let ideal_peak = if frac == 0.0 {
                1.0
            } else {
                (PI * frac).sin() / (PI * frac)
            };
            assert!(
                peak_v > 0.9 * ideal_peak,
                "factor {factor}: peak amplitude {peak_v}, ideal ceiling {ideal_peak}"
            );
        }
    }
}
