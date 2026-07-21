# Oversampling / Downsampling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Optional power-of-two oversampling of the EQ cascade (`--oversample <N>`) via cascaded linear-phase halfband FIR resamplers; a structural no-op by default.

**Architecture:** New `src/resample.rs` holds Kaiser-windowed-sinc halfband tap generation plus polyphase `Upsampler2x`/`Decimator2x`/`Oversampler` structs. `EqChain` optionally wraps its (untouched) per-channel biquad cascade with one `Oversampler` per channel; biquad coefficients are designed at N×fs. CLI → `EngineConfig` → `EqChain::new` plumbing carries the factor.

**Tech Stack:** Rust, existing deps (`biquad`, `clap`, `anyhow`) + `proptest` (dev-dep). Python 3 + numpy/scipy offline only, to generate golden fixtures.

**Spec:** `docs/superpowers/specs/2026-07-21-oversampling-design.md` — read it first.

## Global Constraints

- Factor set is exactly {1, 2, 4, 8, 16}; anything else is an error (CLI parse error / `EqChain::new` error).
- Factor 1 is a *structural* no-op: no `Oversampler` exists, the process loop is the current code path, `flat_preset_is_bit_identical` must keep passing unmodified semantics.
- Real-time contract: no allocation, locks, or I/O in `EqChain::process` or any resampler `push`/`up`/`down` path. All buffers pre-allocated at construction; per-sample scratch is a stack array `[f64; 16]`.
- All DSP arithmetic f64; device boundary stays f32.
- Halfband taps: length ≡ 3 (mod 4), even-offset taps exactly 0.0, symmetric, normalized so `sum(taps) == 1` (matches `scipy.signal.firwin` default scaling). Stopband target 120 dB (assert ≥ 118 in tests).
- Band `fc` must be `< device_rate / 2` even when coefficients are designed at N×fs.
- The pre-commit hook runs the full test suite on every `git commit` — a failing suite blocks the commit; never bypass it, never `git push`.
- Rust style: no `unwrap()` outside tests, iterator chains over index loops where natural, `cargo fmt` + `cargo clippy` clean before each commit.

---

### Task 1: Halfband tap generation

**Files:**
- Create: `src/resample.rs`
- Modify: `src/lib.rs` (add module)
- Test: inline `#[cfg(test)] mod tests` in `src/resample.rs`

**Interfaces:**
- Consumes: nothing (leaf module).
- Produces:
  - `pub const MAX_FACTOR: usize = 16;`
  - `pub fn halfband_taps(fs_in: f64, passband_hz: f64, atten_db: f64) -> Vec<f64>` — taps of the halfband lowpass for a 2× stage whose *input* rate is `fs_in`, at the output rate `2·fs_in`, cutoff `fs_in/2`.
  - private `fn bessel_i0(x: f64) -> f64`.
  - private `const ATTEN_DB: f64 = 120.0;` (used by Task 4).

- [ ] **Step 1: Create module skeleton and register it**

Add to `src/lib.rs` after `pub mod preset;`:

```rust
pub mod resample;
```

Create `src/resample.rs`:

```rust
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
```

- [ ] **Step 2: Write the failing tests**

Append to `src/resample.rs`:

```rust
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
```

(The stopband test uses the same w convention: stopband edge `fs − fp` Hz → `w = π(1 − fp/fs)`.)

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test resample`
Expected: compile error — `halfband_taps` not found.

- [ ] **Step 4: Implement**

Insert between the constants and the test module:

```rust
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test resample`
Expected: 3 passed. Sanity: at 48 k the length is 95; at 44.1 k it is 159.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/lib.rs src/resample.rs
git commit -m "feat: halfband Kaiser-sinc tap generation"
```

---

### Task 2: Upsampler2x

**Files:**
- Modify: `src/resample.rs`
- Test: same file's tests module

**Interfaces:**
- Consumes: `halfband_taps` (Task 1).
- Produces: `pub struct Upsampler2x` with
  - `pub fn new(taps: &[f64]) -> Self`
  - `pub fn push(&mut self, x: f64) -> [f64; 2]` — one input sample at fs_in, two output samples at 2·fs_in **in time order**.

Polyphase background for the implementer: interpolation by 2 = zero-stuff then filter with the ×2-scaled prototype `g = 2·taps`. With length 4k+3 and center index `m = 2k+1`, even output phase `y[2t] = Σ_j g[2j]·x[t-j]` (the non-zero sinc taps) and odd phase `y[2t+1] = g[m]·x[t-(m-1)/2]` (center tap only — a scaled pure delay). Even phase is *first* in time.

- [ ] **Step 1: Write the failing tests**

Add to the tests module:

```rust
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
```

(DC tolerance is 1e-5, not 1e-12: the windowed filter's DC branch gains are exact only to the ~1e-6 stopband floor.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test resample`
Expected: compile error — `Upsampler2x` not found.

- [ ] **Step 3: Implement**

```rust
/// Polyphase 2× interpolator. Feed one sample at fs_in, get two at
/// 2·fs_in. Even output phase is the sinc branch; odd phase is the
/// (scaled) center-tap delay.
pub struct Upsampler2x {
    /// Even-index taps of the ×2-scaled prototype (the non-zero branch).
    branch: Vec<f64>,
    /// ×2-scaled center tap (≈ 1.0 exactly up to window normalization).
    center: f64,
    /// Center-branch delay in input samples: (m-1)/2.
    center_delay: usize,
    /// Ring buffer of past inputs; len == branch.len().
    buf: Vec<f64>,
    pos: usize,
}

impl Upsampler2x {
    pub fn new(taps: &[f64]) -> Self {
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
        let mut acc = 0.0;
        let mut idx = self.pos;
        for &c in &self.branch {
            acc += c * self.buf[idx];
            idx = if idx == 0 { n - 1 } else { idx - 1 };
        }
        let delayed = self.buf[(self.pos + n - self.center_delay) % n];
        self.pos = (self.pos + 1) % n;
        [acc, self.center * delayed]
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test resample`
Expected: all pass.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/resample.rs
git commit -m "feat: polyphase 2x upsampler"
```

---

### Task 3: Decimator2x

**Files:**
- Modify: `src/resample.rs`
- Test: same file's tests module

**Interfaces:**
- Consumes: `halfband_taps`.
- Produces: `pub struct Decimator2x` with
  - `pub fn new(taps: &[f64]) -> Self`
  - `pub fn push(&mut self, v0: f64, v1: f64) -> f64` — two consecutive samples at 2·fs (time order), one output at fs.

Derivation for the implementer: `y[t] = Σ_i h[i]·v[2t-i]` with unscaled taps. Split by parity of `i`: even `i = 2j` reads even-index inputs `v_even[t-j] = v[2(t-j)]`; odd `i` has only the center `m = 2k+1`, reading `v[2t-m] = v_odd[t-k-1]`. So: `y = Σ_j h[2j]·v_even[t-j] + h[m]·v_odd[t-(k+1)]` where `k = (m-1)/2`.

- [ ] **Step 1: Write the failing tests**

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test resample`
Expected: compile error — `Decimator2x` not found.

- [ ] **Step 3: Implement**

```rust
/// Polyphase 2× decimator over the same (unscaled) halfband prototype.
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
    pub fn new(taps: &[f64]) -> Self {
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
        let mut acc = 0.0;
        let mut idx = self.epos;
        for &c in &self.branch {
            acc += c * self.even[idx];
            idx = if idx == 0 { ne - 1 } else { idx - 1 };
        }
        let delayed = self.odd[(self.opos + no - self.center_delay) % no];
        self.epos = (self.epos + 1) % ne;
        self.opos = (self.opos + 1) % no;
        acc + self.center * delayed
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test resample`
Expected: all pass.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/resample.rs
git commit -m "feat: polyphase 2x decimator"
```

---

### Task 4: Oversampler (cascaded stages)

**Files:**
- Modify: `src/resample.rs`
- Test: same file's tests module

**Interfaces:**
- Consumes: `halfband_taps`, `Upsampler2x`, `Decimator2x`, `ATTEN_DB`, `MAX_FACTOR`.
- Produces: `pub struct Oversampler` with
  - `pub fn new(factor: usize, device_rate: f64) -> Self` — `factor ∈ {2,4,8,16}`; panics otherwise (caller validates; `EqChain::new` is the fallible boundary).
  - `pub fn up(&mut self, x: f64, out: &mut [f64; MAX_FACTOR]) -> usize` — returns `factor`.
  - `pub fn down(&mut self, samples: &mut [f64; MAX_FACTOR]) -> f64` — consumes the first `factor` slots **in place** (spec's `down(&[f64])` adjusted to `&mut` to fold stages without a copy).
  - `pub fn latency_frames(&self) -> f64` — total up+down group delay in device-rate frames (fractional for factor ≥ 4).

- [ ] **Step 1: Write the failing tests**

```rust
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
        // -110 dBFS residual bound on a unit sine.
        for t in 0..input.len() - d {
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
                let rms = |s: &[f64]| {
                    (s.iter().map(|x| x * x).sum::<f64>() / s.len() as f64).sqrt()
                };
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
            assert!(peak_v > 0.9, "factor {factor}: peak amplitude {peak_v}");
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test resample`
Expected: compile error — `Oversampler` not found.

- [ ] **Step 3: Implement**

```rust
/// Cascaded halfband stages for oversampling by 2^k, k in 1..=4.
/// Stage `s` (0-based) resamples between `device_rate·2^s` and
/// `device_rate·2^(s+1)`.
pub struct Oversampler {
    up: Vec<Upsampler2x>,
    down: Vec<Decimator2x>,
    factor: usize,
    latency: f64,
}

impl Oversampler {
    /// `factor` must be 2, 4, 8, or 16 — validated by the caller
    /// (`EqChain::new`); this constructor panics on anything else.
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
        }
    }

    /// Expand one device-rate sample into `factor` samples at the
    /// oversampled rate. Returns the count written.
    #[inline]
    pub fn up(&mut self, x: f64, out: &mut [f64; MAX_FACTOR]) -> usize {
        out[0] = x;
        let mut n = 1;
        for stage in &mut self.up {
            // Copy, then expand forward: push() must see inputs in
            // time order, and writing 2i/2i+1 in place would clobber
            // unread inputs.
            let tmp = *out;
            for (i, &s) in tmp[..n].iter().enumerate() {
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
    pub fn latency_frames(&self) -> f64 {
        self.latency
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test resample`
Expected: all pass. Sanity: latency at 48 k ≈ 47.0 (2×), 54.5 (4×), 57.25 (8×), 58.375 (16×) frames.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/resample.rs
git commit -m "feat: cascaded power-of-two oversampler"
```

---

### Task 5: EqChain integration

**Files:**
- Modify: `src/dsp.rs` (struct, `new`, `process`, new accessor, tests)
- Modify: `src/engine.rs:109` (call site — temporary `, 1` until Task 6)
- Modify: `benches/dsp.rs:21`, `tests/perf.rs:18` (call sites)

**Interfaces:**
- Consumes: `resample::{Oversampler, MAX_FACTOR}`.
- Produces:
  - `EqChain::new(preset: &Preset, sample_rate_hz: f64, channels: usize, oversample: usize) -> Result<Self>` — **signature change**; `oversample ∈ {1,2,4,8,16}` else `Err`.
  - `EqChain::latency_frames(&self) -> f64` — 0.0 when factor is 1.
  - `EqChain::from_stages` unchanged (factor-1 semantics, `os: None`).

- [ ] **Step 1: Update all existing call sites mechanically**

Every existing `EqChain::new(a, b, c)` becomes `EqChain::new(a, b, c, 1)`:
- `src/engine.rs:109`: `EqChain::new(preset, f64::from(rate), ch, 1)?` (Task 6 replaces the `1` with the config value)
- `benches/dsp.rs:21`, `tests/perf.rs:18`
- every test in `src/dsp.rs` that calls `EqChain::new` (6 sites)

This won't compile until Step 3 — that's fine; do Steps 1–3 as one edit batch.

- [ ] **Step 2: Write the failing tests**

Add to `src/dsp.rs` tests module:

```rust
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
        EqChain::new(&p, 48_000.0, 2, 4).unwrap().process(&mut stereo);
        let mut mono_out = mono.clone();
        EqChain::new(&p, 48_000.0, 1, 4)
            .unwrap()
            .process(&mut mono_out);
        for (i, frame) in stereo.chunks(2).enumerate() {
            assert_eq!(frame[0], mono_out[i], "L sample {i} diverged");
            assert_eq!(frame[1], 0.0, "R sample {i} contaminated");
        }
    }
```

Also extend the existing `flat_preset_is_bit_identical` — it already proves the factor-1 no-op once its constructor gains `, 1`.

- [ ] **Step 3: Implement**

In `src/dsp.rs`:

```rust
use crate::resample::{Oversampler, MAX_FACTOR};
```

Struct gains a field:

```rust
pub struct EqChain {
    preamp: f64, // linear
    channels: usize,
    /// One cascade per channel: `stages[ch][stage]`.
    stages: Vec<Vec<Stage>>,
    /// One resampler per channel when oversampling; `None` at factor 1
    /// (structural no-op — the process loop is byte-for-byte today's).
    os: Option<Vec<Oversampler>>,
}
```

`new` (replaces the current body; note coefficients at the *internal* rate and the explicit device-Nyquist check):

```rust
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
```

(`from_stages` gains `os: None` in its struct literal; nothing else changes there. The explicit fc check now runs for factor 1 too — it subsumes what `coefficients()` used to reject, and `rejects_band_above_nyquist` keeps passing.)

`process` becomes a two-arm match; the `None` arm is the existing loop verbatim:

```rust
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
```

New accessor:

```rust
    /// Resampler group delay in device-rate frames (0.0 when
    /// oversampling is off).
    pub fn latency_frames(&self) -> f64 {
        self.os
            .as_ref()
            .and_then(|v| v.first())
            .map_or(0.0, Oversampler::latency_frames)
    }
```

- [ ] **Step 4: Run the full suite**

Run: `cargo test`
Expected: all pass, including every pre-existing dsp test (now with `, 1`) and the six new ones.

- [ ] **Step 5: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/dsp.rs src/engine.rs benches/dsp.rs tests/perf.rs
git commit -m "feat: oversampled EQ cascade in EqChain"
```

---

### Task 6: CLI flag and engine plumbing

**Files:**
- Modify: `src/cli.rs` (flag + parser + tests)
- Modify: `src/engine.rs` (`EngineConfig`, call site, startup print)
- Modify: `src/main.rs` (pass-through)

**Interfaces:**
- Consumes: `EqChain::{new, latency_frames}` (Task 5).
- Produces: `RunArgs.oversample: usize` (default 1), `EngineConfig.oversample: usize`.

- [ ] **Step 1: Write the failing CLI tests**

Add to `src/cli.rs` tests module:

```rust
    #[test]
    fn oversample_accepts_powers_of_two_and_defaults_to_one() {
        for n in [1usize, 2, 4, 8, 16] {
            let Cmd::Run(a) = parse(&[
                "run", "--preset", "p.txt", "--oversample", &n.to_string(),
            ])
            .unwrap()
            .cmd
            else {
                panic!("expected Run")
            };
            assert_eq!(a.oversample, n);
        }
        let Cmd::Run(a) = parse(&["run", "--preset", "p.txt"]).unwrap().cmd else {
            panic!("expected Run")
        };
        assert_eq!(a.oversample, 1);
    }

    #[test]
    fn oversample_rejects_everything_else() {
        for bad in ["0", "3", "5", "32", "-2", "two"] {
            assert!(
                parse(&["run", "--preset", "p.txt", "--oversample", bad]).is_err(),
                "{bad} must be rejected"
            );
        }
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test cli`
Expected: compile error — no field `oversample`.

- [ ] **Step 3: Implement flag + parser**

In `src/cli.rs`, add to `RunArgs`:

```rust
    /// Oversample the EQ cascade by this factor (1 = off, bit-perfect)
    #[arg(long, default_value_t = 1, value_parser = parse_oversample)]
    pub oversample: usize,
```

and below the struct:

```rust
fn parse_oversample(s: &str) -> Result<usize, String> {
    match s.parse::<usize>() {
        Ok(n @ (1 | 2 | 4 | 8 | 16)) => Ok(n),
        Ok(n) => Err(format!("must be 1, 2, 4, 8, or 16 (got {n})")),
        Err(e) => Err(e.to_string()),
    }
}
```

- [ ] **Step 4: Plumb through engine and main**

`src/engine.rs` — `EngineConfig` gains the field:

```rust
pub struct EngineConfig {
    pub buffer_frames: u32,
    pub channels: u16,
    /// EQ-cascade oversampling factor (1 = off).
    pub oversample: usize,
}
```

The chain construction (Task 5 left it at `, 1`) — **capture the latency here**: `chain` moves into the input-stream closure a few lines below, so it cannot be queried at print time:

```rust
    let mut chain = EqChain::new(preset, f64::from(rate), ch, cfg.oversample)?;
    let num_bands = chain.num_bands();
    let os_latency_frames = chain.latency_frames();
```

The latency/startup report (replaces the current `latency_ms` + `println!`, after the streams start):

```rust
    let os_ms = os_latency_frames / f64::from(rate) * 1_000.0;
    let latency_ms =
        (frames as f64 * (2 + PREFILL_BLOCKS) as f64) / f64::from(rate) * 1_000.0 + os_ms;
    let os_note = if cfg.oversample > 1 {
        format!(", {}x oversampled", cfg.oversample)
    } else {
        String::new()
    };
    println!(
        "oxideq: {rate} Hz, {ch} ch, {num_bands} bands{os_note}, block {frames} frames (~{latency_ms:.1} ms pipeline latency)"
    );
```

`src/main.rs` — the `EngineConfig` literal gains:

```rust
            oversample: a.oversample,
```

- [ ] **Step 5: Run the full suite**

Run: `cargo test`
Expected: all pass.

- [ ] **Step 6: fmt, clippy, commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add src/cli.rs src/engine.rs src/main.rs
git commit -m "feat: --oversample flag plumbed to the engine"
```

---

### Task 7: scipy golden-vector fixtures

**Files:**
- Create: `tools/gen_fixtures.py`
- Create: `tests/data/` fixtures (generated, committed)
- Create: `tests/golden.rs`

**Interfaces:**
- Consumes: `resample::halfband_taps`, `Upsampler2x`, `EqChain` public APIs.
- Produces: nothing for later tasks (terminal verification artifacts).

- [ ] **Step 1: Write the generator script**

Create `tools/gen_fixtures.py`:

```python
#!/usr/bin/env python3
"""Golden test vectors for oxideq's resampler and EQ.

Independent reimplementation of the DSP in numpy/scipy. Run offline
from the repo root (never during `cargo test`):

    python3 tools/gen_fixtures.py

Requires numpy and scipy:  python3 -m pip install --user numpy scipy
Output: tests/data/*.f64 — raw little-endian float64.
"""

from pathlib import Path

import numpy as np
from scipy import signal

OUT = Path("tests/data")
ATTEN_DB = 120.0


def halfband_taps(fs_in: float, fp: float = 20_000.0) -> np.ndarray:
    """Mirror of src/resample.rs::halfband_taps, via scipy.firwin."""
    fp = min(fp, 0.45 * fs_in)
    dw = np.pi * (1.0 - 2.0 * fp / fs_in)
    n = int(np.ceil((ATTEN_DB - 7.95) / (2.285 * dw) + 1.0))
    while n % 4 != 3:
        n += 1
    beta = 0.1102 * (ATTEN_DB - 8.7)
    # cutoff 0.5 of Nyquist at the output rate == fs_out/4. firwin's
    # default scale=True normalizes DC gain to exactly 1, matching Rust.
    return signal.firwin(n, 0.5, window=("kaiser", beta))


def rbj_peaking(fs: float, f0: float, gain_db: float, q: float):
    """RBJ cookbook peaking EQ, normalized (a0 == 1)."""
    a = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * f0 / fs
    alpha = np.sin(w0) / (2.0 * q)
    b = np.array([1.0 + alpha * a, -2.0 * np.cos(w0), 1.0 - alpha * a])
    den = np.array([1.0 + alpha / a, -2.0 * np.cos(w0), 1.0 - alpha / a])
    return b / den[0], den / den[0]


def up2x(taps: np.ndarray, x: np.ndarray) -> np.ndarray:
    """Streaming-truncated 2x interpolation: first 2*len(x) samples."""
    return signal.upfirdn(2.0 * taps, x, up=2)[: 2 * len(x)]


def down2x(taps: np.ndarray, v: np.ndarray) -> np.ndarray:
    """Streaming-truncated 2x decimation: first len(v)//2 samples."""
    return signal.upfirdn(taps, v, down=2)[: len(v) // 2]


def write(name: str, arr: np.ndarray) -> None:
    (OUT / name).write_bytes(np.asarray(arr, dtype="<f8").tobytes())
    print(f"  {name}: {len(arr)} samples")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)

    # -- taps ---------------------------------------------------------
    for fs in (44_100.0, 48_000.0, 88_200.0, 96_000.0):
        write(f"hb_taps_{int(fs)}.f64", halfband_taps(fs))

    # -- single-stage 2x upsampler waveform @ 48 k --------------------
    rng = np.random.default_rng(0xB1A5)
    x = rng.uniform(-1.0, 1.0, 1024)
    write("up2x_in.f64", x)
    write("up2x_out_48000.f64", up2x(halfband_taps(48_000.0), x))

    # -- EQ waveform, factor 1 @ 48 k ---------------------------------
    # Preset: preamp -3 dB; Peaking 1 kHz +6 dB Q 1; Peaking 8 kHz
    # -4 dB Q 2. Peaking-only: shelf formulas differ between cookbook
    # variants, and shelves are covered by the transfer-function
    # proptest instead.
    eq_in = rng.uniform(-1.0, 1.0, 4096).astype(np.float32)
    write("eq_in.f64", eq_in.astype(np.float64))

    def run_eq(sig64: np.ndarray, fs: float) -> np.ndarray:
        z = sig64 * 10.0 ** (-3.0 / 20.0)  # preamp first, like EqChain
        for f0, g, q in ((1_000.0, 6.0, 1.0), (8_000.0, -4.0, 2.0)):
            b, a = rbj_peaking(fs, f0, g, q)
            z = signal.lfilter(b, a, z)
        return z

    out_1x = run_eq(eq_in.astype(np.float64), 48_000.0)
    write("eq_out_48000_1x.f64", out_1x.astype(np.float32).astype(np.float64))

    # -- full 4x-oversampled EQ chain @ 44.1 k ------------------------
    t1 = halfband_taps(44_100.0)
    t2 = halfband_taps(88_200.0)
    z = eq_in.astype(np.float64) * 10.0 ** (-3.0 / 20.0)
    z = up2x(t1, z)
    z = up2x(t2, z)
    for f0, g, q in ((1_000.0, 6.0, 1.0), (8_000.0, -4.0, 2.0)):
        b, a = rbj_peaking(4 * 44_100.0, f0, g, q)
        z = signal.lfilter(b, a, z)
    z = down2x(t2, z)
    z = down2x(t1, z)
    write("eq_out_44100_4x.f64", z.astype(np.float32).astype(np.float64))


if __name__ == "__main__":
    main()
```

**Detail that matters:** the Rust chain applies the preamp *before* upsampling and narrows to f32 only at the outermost boundary; the script mirrors both (preamp-first, and a final f32 round-trip on EQ outputs so fixtures carry the same quantization).

- [ ] **Step 2: Generate the fixtures**

```bash
python3 -c "import scipy" 2>/dev/null || python3 -m pip install --user numpy scipy
python3 tools/gen_fixtures.py
ls -la tests/data/
```

Expected: 8 `.f64` files; `hb_taps_48000.f64` is 95 samples (760 bytes), `hb_taps_44100.f64` is 159.

- [ ] **Step 3: Write the failing golden tests**

Create `tests/golden.rs`:

```rust
//! Golden-vector tests against fixtures generated by
//! tools/gen_fixtures.py (numpy/scipy — an independent implementation
//! of the same math). Regenerate with:  python3 tools/gen_fixtures.py

use oxideq::dsp::EqChain;
use oxideq::preset::{Band, FilterKind, Preset};
use oxideq::resample::{halfband_taps, Upsampler2x};

fn fixture(name: &str) -> Vec<f64> {
    let path = format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"));
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("{path}: {e} — run tools/gen_fixtures.py"));
    bytes
        .chunks_exact(8)
        .map(|c| f64::from_le_bytes(c.try_into().expect("8-byte chunk")))
        .collect()
}

fn golden_preset() -> Preset {
    Preset {
        preamp_db: -3.0,
        bands: vec![
            Band {
                kind: FilterKind::Peaking,
                fc_hz: 1_000.0,
                gain_db: 6.0,
                q: 1.0,
            },
            Band {
                kind: FilterKind::Peaking,
                fc_hz: 8_000.0,
                gain_db: -4.0,
                q: 2.0,
            },
        ],
    }
}

fn assert_close(ours: &[f64], golden: &[f64], tol: f64, what: &str) {
    assert_eq!(ours.len(), golden.len(), "{what}: length");
    for (i, (a, b)) in ours.iter().zip(golden).enumerate() {
        assert!(
            (a - b).abs() < tol,
            "{what}: sample {i}: {a} vs golden {b}"
        );
    }
}

#[test]
fn taps_match_scipy_firwin() {
    for fs in [44_100.0, 48_000.0, 88_200.0, 96_000.0] {
        let ours = halfband_taps(fs, 20_000.0, 120.0);
        let golden = fixture(&format!("hb_taps_{}.f64", fs as u32));
        assert_close(&ours, &golden, 1e-14, &format!("taps @ {fs}"));
    }
}

#[test]
fn upsampler_matches_scipy_upfirdn() {
    let taps = halfband_taps(48_000.0, 20_000.0, 120.0);
    let input = fixture("up2x_in.f64");
    let golden = fixture("up2x_out_48000.f64");
    let mut up = Upsampler2x::new(&taps);
    let ours: Vec<f64> = input.iter().flat_map(|&x| up.push(x)).collect();
    assert_close(&ours, &golden, 1e-12, "up2x waveform");
}

#[test]
fn eq_waveform_matches_scipy_lfilter() {
    let input = fixture("eq_in.f64");
    let golden = fixture("eq_out_48000_1x.f64");
    let mut buf: Vec<f32> = input.iter().map(|&x| x as f32).collect();
    let mut chain = EqChain::new(&golden_preset(), 48_000.0, 1, 1).unwrap();
    chain.process(&mut buf);
    let ours: Vec<f64> = buf.iter().map(|&x| f64::from(x)).collect();
    // f32 boundary + DF1-vs-DF2T rounding: 1e-6 abs still catches any
    // real coefficient or plumbing bug by orders of magnitude.
    assert_close(&ours, &golden, 1e-6, "EQ 1x waveform");
}

#[test]
fn oversampled_chain_matches_scipy_end_to_end() {
    let input = fixture("eq_in.f64");
    let golden = fixture("eq_out_44100_4x.f64");
    let mut buf: Vec<f32> = input.iter().map(|&x| x as f32).collect();
    let mut chain = EqChain::new(&golden_preset(), 44_100.0, 1, 4).unwrap();
    chain.process(&mut buf);
    let ours: Vec<f64> = buf.iter().map(|&x| f64::from(x)).collect();
    assert_close(&ours, &golden, 1e-6, "EQ 4x waveform");
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --test golden`
Expected: 4 passed. If `taps_match_scipy_firwin` fails on length, the Rust and Python length formulas drifted — fix the copy, don't loosen the tolerance.

- [ ] **Step 5: Commit script + fixtures + tests**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add tools/gen_fixtures.py tests/data tests/golden.rs
git commit -m "test: scipy golden vectors for resampler and EQ"
```

---

### Task 8: Property-based tests

**Files:**
- Modify: `Cargo.toml` (dev-dependency)
- Create: `tests/properties.rs`

**Interfaces:**
- Consumes: `EqChain`, `preset` types, `cli::Cli`, `biquad::Coefficients` (regular dependency — available to integration tests).
- Produces: nothing downstream.

- [ ] **Step 1: Add the dev-dependency**

In `Cargo.toml` under `[dev-dependencies]`:

```toml
proptest = "1"
```

- [ ] **Step 2: Write the property tests**

Create `tests/properties.rs`:

```rust
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
```

- [ ] **Step 3: Run them**

Run: `cargo test --test properties`
Expected: 5 passed (takes a few seconds — signal-level cases process ~0.4 s of audio each).

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock tests/properties.rs
git commit -m "test: metamorphic proptests for chain and CLI"
```

---

### Task 9: Benches and docs

**Files:**
- Modify: `benches/dsp.rs`
- Modify: `README.md`

**Interfaces:**
- Consumes: `EqChain::new` (4-arg).
- Produces: nothing downstream.

- [ ] **Step 1: Extend the bench over factors**

Replace `bench_process` in `benches/dsp.rs` (constructor was already 4-arg after Task 5):

```rust
fn bench_process(c: &mut Criterion) {
    let parsed = preset::parse(include_str!("../presets/example.txt")).unwrap();
    assert_eq!(
        parsed.preset.bands.len(),
        10,
        "bench expects the 10-band fixture"
    );

    let mut g = c.benchmark_group("dsp");
    g.throughput(Throughput::Elements(FRAMES as u64));
    for factor in [1usize, 4, 16] {
        let mut chain = EqChain::new(&parsed.preset, 48_000.0, CHANNELS, factor).unwrap();
        // Non-silent, non-constant input so denormal handling costs are visible.
        let mut buf: Vec<f32> = (0..FRAMES * CHANNELS)
            .map(|i| ((i % 97) as f32 / 97.0) - 0.5)
            .collect();
        g.bench_function(format!("process_256frame_stereo_10band_{factor}x"), |b| {
            b.iter(|| chain.process(black_box(&mut buf)));
        });
    }
    g.finish();
}
```

- [ ] **Step 2: Run the bench once to confirm it executes**

Run: `cargo bench --bench dsp -- --quick`
Expected: three benchmark lines (1x, 4x, 16x); 4x roughly 4–6× the 1x time. Record the numbers in the commit message body.

- [ ] **Step 3: Document the flag in README.md**

Add under the CLI/usage documentation (near the `--buffer` description):

```markdown
### Oversampling

`--oversample <N>` (N ∈ {1, 2, 4, 8, 16}, default 1) runs the EQ cascade at
N× the device rate behind linear-phase halfband resamplers (Kaiser-windowed
sinc, ~120 dB stopband). This removes the biquad frequency-response
"cramping" near Nyquist — audible as slightly pinched high-frequency EQ
shapes at 44.1/48 kHz. Costs: ~N× DSP CPU and ~1 ms of extra latency
(reported at startup). With the default of 1 no resampler code runs at all
and the pipeline stays bit-perfect; with N > 1 every sample is rewritten by
the resampling filters, so bit-perfectness is intentionally traded for
response accuracy.
```

- [ ] **Step 4: Full suite + commit**

```bash
cargo test
cargo fmt && cargo clippy --all-targets -- -D warnings
git add benches/dsp.rs README.md
git commit -m "bench+docs: oversampling factors and README section"
```

---

## Execution notes

- Tasks 1–4 are pure `src/resample.rs` and independent of the rest; Tasks 5–9 depend on their predecessors in order.
- The pre-commit hook runs the whole test suite on every commit; a red suite blocks the commit — that is the intended checkpoint per task.
- `tests/perf.rs` keeps its factor-1 NFR budget (the <1%-of-core requirement applies to the default configuration only).
- If `stopband_meets_118_db` is marginal on some rate, raise `ATTEN_DB` to 122.0 rather than loosening the assertion.
