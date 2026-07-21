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
