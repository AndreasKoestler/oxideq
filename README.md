# OxidEQ

Bit-perfect, system-wide parametric EQ pipeline in Rust. Captures a
virtual sink, applies an Equalizer APO / AutoEQ preset (preamp + Direct
Form 1 biquad cascade), and plays out to your DAC at the source's
native rate. No compression, no limiting, no auto-gain — linear
processing only.

    [ Desktop Audio ] → [ Virtual Sink ] → [ oxideq: preamp → biquads ] → [ DAC ]

## Quick start (Linux / PipeWire)

    cargo install --path .            # or: cargo build --release
    oxideq install-sink               # Tier 1: create the virtual sink
    systemctl --user restart pipewire pipewire-pulse
    # pick "OxidEQ Sink" as the default output device, then:
    oxideq run --preset my_headphones.txt --output "<DAC name>" --auto-link

Presets are standard AutoEQ output ("Equalizer APO parametric" format):
`Preamp:` plus `PK`/`LSC`/`HSC` filter lines. Grab one from
https://github.com/jaakkopasanen/AutoEq for your headphones.

## Routing tiers

1. **Virtual sink (recommended).** `oxideq install-sink` writes
   `~/.config/pipewire/pipewire.conf.d/99-oxideq-sink.conf`. Select
   "OxidEQ Sink" as the desktop output; oxideq EQ's its monitor into
   the DAC.
2. **qpwgraph profiles.** Wire `OxidEQ-Sink:monitor_* → oxideq` and
   `oxideq → <DAC>` visually, then File → Save; enable *Patchbay →
   Activated* so qpwgraph re-pins the links automatically on restart.
3. **Automatic (`--auto-link`).** oxideq runs `pw-link` itself at
   startup, including cutting accidental feedback links back into its
   own sink.

macOS: see [docs/macos.md](docs/macos.md) (BlackHole 2ch as the sink).

## Bit-perfect notes

- oxideq requests the output device at the *input's* current rate and
  warns when the device cannot lock it (the system resampler engages).
- With a null sink, PipeWire still resamples *sources* into the graph
  rate. For true end-to-end rate lock, allow the graph to follow
  sources — e.g. `~/.config/pipewire/pipewire.conf.d/10-rates.conf`:

      context.properties = {
          default.clock.allowed-rates = [ 44100 48000 88200 96000 176400 192000 ]
      }

- The preamp is a constant linear multiplier — the only headroom
  mechanism. oxideq counts clipped samples and warns; it never limits.
- All DSP arithmetic, coefficients, and filter state are 64-bit floats;
  only the device boundary is f32 (the PipeWire/CoreAudio native format).
- A flat preset (0 dB preamp, no bands) is bit-identical passthrough
  (covered by a unit test).

## Performance validation (NFRs)

- **DSP micro-benchmark:** `cargo bench --bench dsp` — ns per 256-frame
  stereo block through the 10-band example preset. Budget: 5.33 ms of
  audio per block ⇒ <1% of one core requires <53 µs.
- **Hard gate:** `cargo test --release --test perf -- --ignored --nocapture`
  asserts <1% of one core and <1 ms per block, printing the measured
  numbers.
- **Whole-process check while playing:** `pw-top` (Linux — also shows
  quantum/rate per node) or `top -pid $(pgrep oxideq)`.
- **Pipeline latency:** printed at startup; defaults (256-frame block,
  2-block prefill) come to ~21 ms at 48 kHz. Tune with `--buffer`;
  underrun warnings mean it's too tight for your system.

Always listen with `--release` builds — debug builds miss real-time
deadlines and crackle (PRD 4.2).

## Roadmap (explicit non-goals for v1)

- Format-shift hot reload (rate changes currently require restart).
- Preset file watching / hot reload.
- systemd user service & launchd wrappers.
