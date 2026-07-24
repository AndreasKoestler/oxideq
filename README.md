# OxidEQ

[![CI](https://github.com/AndreasKoestler/oxideq/actions/workflows/ci.yml/badge.svg)](https://github.com/AndreasKoestler/oxideq/actions/workflows/ci.yml)

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
    oxideq devices                    # find your DAC's name
    oxideq run --preset my_headphones.txt --input OxidEQ-Sink --output "<DAC name>"
    # 3. wire the sink's monitor into oxideq (see "Routing")

## Quick start (macOS)

    brew install blackhole-2ch        # virtual sink (one-time)
    cargo install --path .
    # System Settings → Sound → Output: "BlackHole 2ch"
    oxideq devices                    # find your DAC's name
    oxideq run --preset my_headphones.txt --input BlackHole --output "<DAC name>"

For bit-perfect playback set BlackHole 2ch *and* the DAC to the source
rate in Audio MIDI Setup. Details and limitations: [docs/macos.md](docs/macos.md).

## Selecting a device

`--input`/`--output` match case-insensitively against the device name *or* its
backend id: an exact id match wins (so `hw:CARD=0,DEV=0` never lands on
`plughw:CARD=0,DEV=0`, which contains it); otherwise the first device whose
name or id contains the text. `oxideq devices` shows what you can pick:

    Output devices:
      Hidizs S9Pro, USB Audio
        hw:CARD=0,DEV=0          bit-perfect (locks to the source rate)
        plughw:CARD=0,DEV=0      auto rate/format conversion
      ...
      Routes (OS picks/mixes): jack, pipewire, pulse, default

On Linux one physical DAC is exposed by ALSA under many names — the raw
hardware device, a converting wrapper, and assorted routing plugins. Match on
the **backend id** to pin the exact one; the name alone is ambiguous:

- `hw:CARD=…,DEV=…` — **direct hardware, bit-perfect.** The right pick for this
  tool. Rate-strict: oxideq locks the pipeline to the source rate, and if the
  DAC can't open at it you get a fallback warning or an error (see
  [Bit-perfect notes](#bit-perfect-notes)).

      oxideq run --preset p.txt --input OxidEQ-Sink --output hw:CARD=0,DEV=0

- `plughw:CARD=…,DEV=…` — hardware **with** ALSA's automatic rate/format
  conversion. Use it if `hw:` refuses your rate and you accept resampling.
- `jack` / `pipewire` / `pulse` / `default` — hand off to the sound server; it
  routes and may resample. Convenient, not bit-perfect.

`oxideq devices` shows only hardware devices and these routes by default. Add
`--all` to dump every ALSA PCM (the `surround*`, `iec958`, `sysdefault`,
`front`, `usbstream`, … plugin variants) if you need one of them.

## Presets

Presets are standard AutoEQ output ("Equalizer APO parametric" format):
`Preamp:` plus `PK`/`LSC`/`HSC` filter lines. Grab one from
https://github.com/jaakkopasanen/AutoEq for your headphones. Every
filter line needs an explicit `Q` (AutoEQ always emits one); Q-less
shorthand lines are rejected.

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

By default PipeWire's session manager auto-connects oxideq to your default
devices — pulling your mic into the EQ input and wiring playback back into the
OxidEQ sink. That last one is a feedback loop (the sink is your default output,
which oxideq is also capturing). If you hit it, either disable autoconnect
(strongly recommended — see [Troubleshooting](#troubleshooting)) or cut the
stray links by hand each run:

    pw-link -d oxideq:output_FL OxidEQ-Sink:playback_FL
    pw-link -d oxideq:output_FR OxidEQ-Sink:playback_FR

macOS: see [docs/macos.md](docs/macos.md) (BlackHole 2ch as the sink).

## Troubleshooting

**Symptoms (Linux / PipeWire):** your microphone is audible through the EQ; a
constant `N samples clipped in last 5 s` warning even with nothing playing; or
runaway distortion. All the same cause — PipeWire auto-wired oxideq to the
default source (mic) and default sink, and the playback→sink→monitor→input path
formed a feedback loop that a preset with any net gain drives to full-scale.

**Fix:** stop oxideq's nodes from auto-connecting, so only *your* `pw-link`s
exist. oxideq does not touch `PIPEWIRE_PROPS` itself — set it yourself when
launching (pipewire-alsa reads it; harmless on other backends):

    PIPEWIRE_PROPS='{ node.name=oxideq node.autoconnect=false }' \
        oxideq run --preset presets/koss_porta_pro.txt --input pipewire --output pipewire

`node.name=oxideq` names the node so `pw-link` / qpwgraph can find it (the
`oxideq:` prefix the routing commands above use); `node.autoconnect=false` is
the part that kills the loop and the mic bleed. With autoconnect off, oxideq
starts wired to nothing — you must run the `pw-link` commands above to route it.

**Quiet output even at full volume:** check the DAC's *PipeWire* node volume,
separate from its hardware knob — `wpctl status` then `wpctl set-volume <id> 1.0`.

## Bit-perfect notes

- oxideq runs capture and playback at one common rate, preferring the
  source's native rate. If the output device cannot lock it, the whole
  pipeline falls back to the nearest rate both devices support (the OS
  resamples the capture side — no longer bit-perfect) and warns; with
  no common rate at all it exits with an error instead of drifting.
- With a null sink, PipeWire still resamples *sources* into the graph
  rate. For true end-to-end rate lock, allow the graph to follow
  sources — e.g. `~/.config/pipewire/pipewire.conf.d/10-rates.conf`:

      context.properties = {
          default.clock.allowed-rates = [ 44100 48000 88200 96000 176400 192000 ]
      }

  List only rates your DAC actually supports. On Linux, read them from
  the card's ALSA stream info (no extra tools; replace `card0` with your
  card — `cat /proc/asound/cards` shows the index):

      cat /proc/asound/card0/stream0 | grep -m1 Rates
      # Rates: 44100, 48000, 88200, 96000, 176400, 192000, 352800, 384000, ...

  `aplay -v --dump-hw-params -D hw:CARD=0,DEV=0 /dev/zero` also prints a
  `RATE:` line (needs `alsa-utils`). Keep the list to rates real content
  uses (through 192000, occasionally 384000); a needlessly high graph
  clock just burns CPU on every stream. macOS: Audio MIDI Setup shows the
  device's rates.

- The preamp is a constant linear multiplier — the only headroom
  mechanism. oxideq counts clipped samples and warns; it never limits.
- All DSP arithmetic, coefficients, and filter state are 64-bit floats;
  only the device boundary is f32 (the PipeWire/CoreAudio native format).
- A flat preset (0 dB preamp, no bands) is bit-identical passthrough
  (covered by a unit test).
- `--oversample` with N > 1 is intentionally not bit-perfect — every sample
  is rewritten by the resampling filters; the default (N=1) keeps the
  bit-perfect guarantee.

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

### Oversampling

`--oversample <N>` (N ∈ {1, 2, 4, 8, 16}, default 1) runs the EQ cascade at
N× the device rate behind linear-phase halfband resamplers (Kaiser-windowed
sinc, ~120 dB stopband). This removes the biquad frequency-response
"cramping" near Nyquist — audible as slightly pinched high-frequency EQ
shapes at 44.1/48 kHz. Costs: the halfband FIR resamplers dominate CPU, so
the real cost is well above a naïve N× — measured ≈16× the 1× cost at 4× and
≈42× at 16× (`cargo bench --bench dsp`), though still a small fraction of one
core (≈1.5% at 4×, ≈4% at 16× for a 256-frame stereo block), and adds ~1 ms
of latency, included in the pipeline-latency figure printed at startup. With
the default of 1 no resampler code runs at all and the pipeline stays
bit-perfect; with N > 1 every sample is rewritten by the resampling filters,
so bit-perfectness is intentionally traded for response accuracy.

### Filter backend

`--backend <df1|df2>` (default `df1`) selects the biquad realization: Direct
Form 1 or Direct Form 2 transposed. Both realize the same transfer function;
they differ only in numerical conditioning. DF2 transposed is generally
better-conditioned at very high internal sample rates, so it pairs naturally
with a high `--oversample` factor. The choice is dispatched once per block, so
it has no per-sample cost. `df1` keeps the historical (and bit-perfect at
`--oversample 1`) behavior.

### Measurement report

`tools/measure_report.py` validates a preset end-to-end and renders an HTML
report. It plays a log sine sweep through an isolated PipeWire chain (player →
oxideq → recorder), recovers the transfer function by cross-spectrum, and
checks four things: flat-preset transparency, accuracy against the analytic RBJ
target, oversampling against the analog ideal, and near-Nyquist cramping.

Needs `numpy`, `scipy`, `matplotlib`, and a running PipeWire session with
`pw-cat`/`pw-record`/`pw-link`/`pw-metadata` on `PATH`. From the repo root
after `cargo build --release`:

    python3 tools/measure_report.py --preset presets/koss_porta_pro.txt \
        --backend df1 --rate 48000 --oversample 4 --out eq-report

Writes `eq-report/report.html` and its figures. `tools/measure_capture.sh` is
the single-capture helper it drives.

## Roadmap (explicit non-goals for v1)

- Format-shift hot reload (rate changes currently require restart).
- Preset file watching / hot reload.
- systemd user service & launchd wrappers.
