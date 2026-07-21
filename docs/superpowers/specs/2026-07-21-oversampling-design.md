# Oversampling / Downsampling Design

**Date:** 2026-07-21
**Status:** Approved

## Purpose

Fix biquad frequency-response cramping near Nyquist by optionally running the
entire EQ cascade at an elevated internal rate. Off by default: without
`--oversample`, the pipeline is structurally identical to today and stays
bit-perfect.

## Requirements

- CLI flag `--oversample <N>`, N ∈ {1, 2, 4, 8, 16}; default 1. Any other
  value is a parse error.
- N = 1 is a *structural* no-op: no resampler objects exist, the process loop
  is today's exact code path, and `flat_preset_is_bit_identical` still holds.
- N > 1: upsample by N (cascaded 2× stages) as the first signal step after the
  preamp, run the biquad cascade at N·fs with coefficients designed at N·fs,
  decimate back to fs as the last step.
- Resamplers are linear-phase Kaiser-windowed sinc halfband FIRs,
  ~120 dB stopband. Audiophile transparency: flat passband phase; filter
  ringing sits at the elevated Nyquist, far above the audible band.
- Real-time contract unchanged: no allocation, locks, or I/O in `process`.
- While oversampling is active the pipeline is by definition not bit-perfect
  (sinc filtering rewrites every sample). Bit-perfect claim applies to the
  default (off) mode only.

## Architecture (chosen: approach A)

Resamplers are concrete structs that wrap the untouched biquad cascade inside
`EqChain`. The `Filter` trait (`fn run(&mut self, f64) -> f64`) is a 1-in/1-out
same-rate map; resamplers are 1→N and N→1, so they do not implement it.
The `Stage` enum, per-channel `Vec<Stage>`, and the inlined biquad loop are
unchanged.

Rejected alternatives:
- **B — engine-level block resampling:** leaks DSP into `engine.rs`, needs N×
  scratch management there, untestable at `EqChain` level.
- **C — block-based rate-changing `Filter` trait:** rewrites every stage,
  vtable per block, kills the per-sample inline biquad path.

### New module `src/resample.rs`

| Item | Description |
|---|---|
| `halfband_taps(fs_in, passband_hz, atten_db) -> Vec<f64>` | Kaiser-windowed sinc, cutoff fs_in/2 (= fs_out/4), length 4k+3 so even-offset taps are exactly zero. Construction-time only. |
| `Upsampler2x` | Polyphase 1→2. Even branch is pure delay (center tap); odd branch ≈ taps/4 multiplies. Taps scaled ×2 for interpolation gain. |
| `Decimator2x` | Same tap set unscaled, folded 2→1. |
| `Oversampler` | Factor 2^k: k cascaded `Upsampler2x` + k mirrored `Decimator2x`. API: `up(x: f64, out: &mut [f64; 16]) -> usize`, `down(&[f64]) -> f64`, `latency_frames() -> f64`. Knows nothing about `Stage`/EQ. Per-channel instance, `Send`. |

### `dsp.rs` changes

- `EqChain::new(preset, device_rate, channels, oversample_factor)`.
  Coefficients designed at `device_rate × factor`.
- Holds `os: Option<Vec<Oversampler>>` (one per channel); `None` at factor 1.
- `process` per-sample path when active:
  `x → preamp → up (N samples into stack buffer [f64; 16]) → each sample
  through biquad cascade → down → 1 sample out`. Up-stage expansion runs
  back-to-front in the stack buffer (in-place doubling); no heap.
- `EqChain::latency_frames() -> f64` exposes total resampler group delay in
  device-rate frames (0.0 when off). Fractional for factors ≥ 4 (see below).

### Data flow (N = 4)

```
f32 in → f64 → ×preamp → up2x → up2x → [4 samples @ 4fs]
       → biquad cascade (@ 4fs) ×4 → down2x → down2x → f32 out
```

### CLI / engine integration

- `cli.rs`: `--oversample <N>`, default 1, clap value-parser rejects
  non-{1,2,4,8,16}.
- `EngineConfig` gains the factor; `engine.rs` passes it to `EqChain::new`,
  adds resampler group delay to the printed pipeline latency, and reports the
  factor in the startup line.

## Filter design specifics

Computed at construction from the actual device rate:

- **Stage 1** (sharp): passband edge `min(20 kHz, 0.9·fs/2)`, stopband edge
  mirrored (fs − edge), 120 dB → Kaiser β ≈ 12.3. ≈ 95 taps @ 48 k,
  ≈ 171 taps @ 44.1 k.
- **Stages 2..k**: same passband, Nyquist doubles per stage → transition band
  huge → ~11–23 taps. Same generator.
- Group delay: an up+down pair at stage j (running at 2^j·fs, N taps each)
  delays (N−1)/2^j device-rate frames. Stage 1 is integer ((N−1)/2, e.g.
  47 frames @ 48 k ≈ 1.0 ms); stages ≥ 2 land on fractional device frames,
  so the total is fractional for factors ≥ 4. Totals @ 48 k:
  ≈ 1.0 ms @ 2×, ≈ 1.1 ms @ 4×, ≈ 1.2 ms @ 16×; @ 44.1 k roughly double
  (stage 1 needs ~171 taps).

## Validation & error handling

- Factor validated at CLI parse time (clap).
- Band `fc` validated against **device** Nyquist explicitly when factor > 1
  (coefficient design at N·fs would otherwise accept fc up to N·fs/2;
  `rejects_band_above_nyquist` keeps its meaning).
- Tap generation is infallible for validated inputs. No new error paths in
  the audio callback.

## Testing

1. **Tap invariants:** symmetry, even-offset zeros, DC gain (2 up / 1 down),
   stopband ≥ 118 dB via DFT probe.
2. **Round-trip transparency:** at 2× (integer delay) a passband sine through
   up→down (no EQ) equals input shifted by `latency_frames()`, residual
   < −110 dBFS. At 4×/16× (fractional delay) assert delay-insensitive
   transparency instead: steady-state RMS gain within ±0.05 dB across the
   passband.
3. **Cramping fix:** peaking fc = 18 kHz, +6 dB, Q = 2 @ 44.1 k — realized
   gain at fc within 0.15 dB of nominal at 4×, and strictly closer than the
   1× result.
4. **No-op:** factor 1 → bit-identical output, `os` is `None`.
5. **Channel state independence** under oversampling.
6. **CLI:** accepts {1, 2, 4, 8, 16}; rejects 0, 3, 32.
7. **Latency:** impulse peak lands at `round(latency_frames())` (exact at 2×).
8. **Bench:** `benches/dsp.rs` gains 1×/4×/16× cases.
