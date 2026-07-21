# OxidEQ

Bit-perfect, system-wide parametric EQ pipeline in Rust. Captures a
virtual sink, applies an Equalizer APO / AutoEQ preset (preamp + Direct
Form 1 biquad cascade), and plays out to your DAC at the source's
native rate. No compression, no limiting, no auto-gain — linear
processing only.

    [ Desktop Audio ] → [ Virtual Sink ] → [ oxideq: preamp → biquads ] → [ DAC ]

## Quick start (Linux / PipeWire)

    cargo install --path .            # or: cargo build --release
    # 1. create the virtual sink (one-time, see "Routing" below)
    # 2. pick "OxidEQ Sink" as the default output device, then:
    oxideq run --preset my_headphones.txt --input OxidEQ-Sink --output "<DAC name>"
    # 3. wire the sink's monitor into oxideq (see "Routing")

Presets are standard AutoEQ output ("Equalizer APO parametric" format):
`Preamp:` plus `PK`/`LSC`/`HSC` filter lines. Grab one from
https://github.com/jaakkopasanen/AutoEq for your headphones.

`oxideq` names its PipeWire node `oxideq` (ports `oxideq:input_*` /
`oxideq:output_*`), so the routing tools below can find it.

## Routing

OxidEQ does no graph management of its own — it is a plain capture→EQ→playback
client. Set up the sink and links with standard PipeWire tooling (this used to
be built in as `install-sink`/`--auto-link`; it is now external so the binary
stays a pure processor).

### 1. Create the virtual sink (recommended)

Write `~/.config/pipewire/pipewire.conf.d/99-oxideq-sink.conf`:

    context.objects = [
        {   factory = adapter
            args = {
                factory.name     = support.null-audio-sink
                node.name        = "OxidEQ-Sink"
                node.description = "OxidEQ Sink"
                media.class      = Audio/Sink
                audio.position   = [ FL FR ]
                monitor.channel-volumes = true
            }
        }
    ]

Then `systemctl --user restart pipewire pipewire-pulse` and select
"OxidEQ Sink" as the desktop's default output. Delete the file to uninstall.

### 2. Wire it up

**qpwgraph (GUI):** wire `OxidEQ-Sink:monitor_* → oxideq:input_*` and
`oxideq:output_* → <DAC>:playback_*`, then File → Save and enable
*Patchbay → Activated* so the links re-pin on restart.

**pw-link (CLI):** list ports with `pw-link -o` / `pw-link -i`, then:

    pw-link OxidEQ-Sink:monitor_FL oxideq:input_FL
    pw-link OxidEQ-Sink:monitor_FR oxideq:input_FR
    pw-link oxideq:output_FL "<DAC>:playback_FL"
    pw-link oxideq:output_FR "<DAC>:playback_FR"

If PipeWire auto-connected oxideq's playback back into the OxidEQ sink
(a feedback loop, since it may be your default output), cut it:

    pw-link -d oxideq:output_FL OxidEQ-Sink:playback_FL
    pw-link -d oxideq:output_FR OxidEQ-Sink:playback_FR

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
deadlines and crackle.

## Roadmap (explicit non-goals for v1)

- Format-shift hot reload (rate changes currently require restart).
- Preset file watching / hot reload.
- systemd user service & launchd wrappers.
