# Progress Log

Ralph-loop iteration log for OxidEQ. Append an `## Iteration N` section per
iteration; keep each to 3–5 bullets: what was done, key decisions, gotchas,
and any deferred verifications (e.g. Linux-only PipeWire checks deferred on
the macOS dev machine). Task 4 iterations must record the measured bench and
perf-gate numbers as the machine's NFR baseline.

_No iterations yet._

## Iteration 1

- Ran `cargo init --name oxideq` in the existing (non-empty) repo, then `cargo add anyhow biquad cpal regex ringbuf`; set `edition = "2021"` (cargo defaulted to `"2024"`) and added `description` + `[profile.release]` (lto, codegen-units=1) to `Cargo.toml`.
- Resolved dependency versions: `anyhow 1.0.104`, `biquad 0.6.0`, `cpal 0.18.1`, `regex 1.13.1`, `ringbuf 0.5.1`. `biquad` cleared the `>= 0.4.2` floor with room to spare, so no version pin was needed; verified in source that `Type<DBGain>::{LowShelf, HighShelf, PeakingEQ}` all carry the gain parameter.
- Wrote `src/lib.rs` (crate doc comment only), `src/main.rs` (placeholder dispatcher print), `presets/example.txt` (10-band AutoEQ-style fixture: 1 LSC, 8 PK, 1 HSC, preamp -6.8 dB), `.githooks/pre-commit` (fmt --check, clippy -D warnings, test), and ran `git config core.hooksPath .githooks`.
- Verified `cargo build`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` all pass clean (0 tests, 0 failures) before committing; `Cargo.lock` committed since this is a binary application.
- Gotcha: `cargo init` in a non-empty directory works fine but defaults `edition` to `"2024"` regardless of installed toolchain support — must explicitly override to `"2021"` per the plan every time this pattern is reused.

## Iteration 2

- TDD: wrote `src/preset.rs` with only the `#[cfg(test)]` module (7 tests) first, confirmed RED via `cargo test preset` (compile errors: `Band`/`FilterKind`/`parse` undefined), then prepended the parser implementation (regex-based: `preamp_re`, `loose_re` to classify ON/OFF and filter type, `strict_re` to validate/extract args) and confirmed GREEN (7 passed).
- Created `presets/koss_porta_pro.txt` and `presets/meze_99_classics.txt` as exact-match fixtures (real AutoEQ results) covering decimal Fc, boost/cut shelves, low/high Q, extreme gains, and a near-zero HSC gain; added `pub mod preset;` to `src/lib.rs`.
- Parsing rules implemented exactly per plan: `Preamp:` sets `preamp_db` (default 0.0 if absent); `ON PK/LSC/HSC` → `Band`; `OFF` and unsupported types (e.g. `LP`) → skipped with a line-numbered warning; a supported type with malformed args → hard `bail!` error; all other lines ignored silently.
- Gotcha: the brief's verbatim code block is not byte-identical to `rustfmt` output (line-wrapping of `assert_eq!`/`push(format!(...))` calls) — ran `cargo fmt` after pasting to satisfy the pre-commit hook's `fmt --check`; this is whitespace-only, no semantic change, so it does not violate "match the brief exactly."
- Verified `cargo test` (7/7 preset tests + 0 others, all green), `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` all pass clean before committing.

## Iteration 3

- TDD: wrote `src/dsp.rs` with only the 8-test module first, confirmed RED via `cargo test dsp` (compile errors: `EqChain`/`Filter` undefined), then prepended `db_to_linear`, the `Filter` trait, its `DirectForm1<f64>` impl, `EqChain::{new, from_filters, process, num_bands}`, and the `coefficients` helper; confirmed GREEN (8 passed, 15/15 total).
- biquad-0.6 drift check: verified the installed 0.6.0 source directly — `Coefficients::<f64>::from_params(Type<C>, Hertz<C>, Hertz<C>, C) -> Result<_, Errors>`, `Errors::{OutsideNyquist, NegativeQ, NegativeFrequency}` (derives Debug for `{e:?}`), `ToHertz::hz()`, `DirectForm1::<f64>::new`, `Biquad::run`. The brief's code compiles verbatim; NO adaptation was needed (Fc 30 kHz @ 48 kHz → `OutsideNyquist`, so `rejects_band_above_nyquist` passes).
- RT-safety: `process` and `Filter::run` are alloc/lock/I/O-free (only `chunks_exact_mut`/`iter_mut`/`zip` + f64 arithmetic + register widen/narrow); all boxing/`Vec` allocation happens in `new`/`from_filters`. f64 for all arithmetic/coeffs/state; f32 only at the buffer boundary.
- Gotcha (same as Iteration 2): the brief's verbatim test literals are not byte-identical to `rustfmt` output (struct/`assert_eq!` reflow), so ran `cargo fmt` after pasting to satisfy the hook's `fmt --check`; whitespace-only, no semantic change, 8 tests unchanged behaviorally.
- Verified `cargo test` (15/15 green), `cargo fmt --check`, and `cargo clippy --all-targets -- -D warnings` all pass clean before committing.

## Iteration 4

- `cargo add --dev criterion` (resolved 0.8.2), appended `[[bench]] name = "dsp" harness = false` to `Cargo.toml`; criterion stays dev-only, runtime deps unchanged (cpal/biquad/ringbuf/regex/anyhow).
- Wrote `benches/dsp.rs` (criterion, 10-band fixture via `include_str!("../presets/example.txt")`, black-boxed `chain.process`) and `tests/perf.rs` (ignored-by-default NFR guard: 10 s of 256-frame stereo blocks after 1,000-block warm-up) verbatim from the brief.
- **Measured NFR baseline (this machine):** `cargo bench --bench dsp` → `time: [5.6805 µs 5.6969 µs 5.7156 µs]` (median 5.6969 µs/iter, thrpt ≈ 44.9 Melem/s). `cargo test --release --test perf -- --ignored --nocapture` → `dsp: 0.184% of one core, 9.80 µs per 256-frame block` — PASSED (well under the <1%-of-one-core and <1 ms/block NFR gates; both figures land inside the brief's expected 2–10 µs/block range).
- Confirmed default `cargo test` reports `dsp_meets_cpu_and_latency_budget ... ignored, wall-clock timing: run explicitly with --release` while all 15 other tests stay green — the perf gate never runs in the normal suite/hook.
- Gotcha (same as Iterations 2–3): the brief's verbatim `assert_eq!(parsed.preset.bands.len(), 10, "...")` one-liner in `benches/dsp.rs` exceeds rustfmt's line width and gets reflowed to a multi-line call; ran `cargo fmt` after pasting — whitespace-only, no semantic change.
