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
