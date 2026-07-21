# OxidEQ — Build PRD (ralph loop)

Bit-perfect parametric EQ pipeline in Rust (PipeWire + CoreAudio). One task per
iteration. **Full per-task detail — code, tests, commands, commit messages — lives
in `docs/superpowers/plans/2026-07-20-oxideq.md`. Follow the matching task
section exactly, including its checkbox steps and design decisions.**

## Tasks

- [x] Task 1: Project scaffold (crate, deps, pre-commit hook, example preset)
- [x] Task 2: Equalizer APO preset parser + real-preset fixtures (`src/preset.rs`, `presets/*.txt`)
- [x] Task 3: DSP — pluggable `Filter` trait + preamp + f64 biquad cascade (`src/dsp.rs`)
- [ ] Task 4: CPU & latency benchmarks + NFR guard test (`benches/dsp.rs`, `tests/perf.rs`)
- [ ] Task 5: Device discovery & sample-rate negotiation (`src/devices.rs`)
- [ ] Task 6: Real-time engine — ring buffer + streams (`src/engine.rs`)
- [ ] Task 7: CLI + end-to-end wiring (`src/cli.rs`, `src/main.rs`)
- [ ] Task 8: Tier 1 — PipeWire virtual sink installer (`src/routing.rs`)
- [ ] Task 9: Tier 3 — pw-link auto-wiring + feedback guard
- [ ] Task 10: macOS (CoreAudio/BlackHole) verification & docs (`docs/macos.md`)
- [ ] Task 11: README, routing tiers, performance validation docs

## Definition of done (every task)

- Tests written first where the plan's task section says so; `cargo test` green.
- `.githooks/pre-commit` passes (fmt --check, clippy -D warnings, test).
- One commit, message as given in the plan. Never push.
- `progress.md` gets an `## Iteration N` entry (3–5 bullets), including any
  deferred manual verifications (Linux-only steps on the macOS dev machine).

## Non-negotiable constraints (from the plan's Global Constraints)

- No allocation, locks, or I/O in real-time audio callbacks.
- No dynamics processing ever — clipping is counted and reported, never altered.
- DSP arithmetic/coefficients/state in f64; device I/O and ring buffer in f32.
- Filter stages go through the `dsp::Filter` trait; biquad DF1 is the v1 backend.
- Runtime deps only: cpal, biquad, ringbuf, regex, anyhow (+ criterion dev-only).
