# OxidEQ on macOS (Apple Silicon)

cpal uses CoreAudio natively on `aarch64-apple-darwin` — no code or
feature-flag changes. macOS has no user-space virtual sinks, so the
loopback device comes from [BlackHole](https://github.com/ExistentialAudio/BlackHole):

    brew install blackhole-2ch

## Route

1. System Settings → Sound → Output: **BlackHole 2ch** (this is the
   "virtual sink"; system volume keys won't affect the DAC anymore).
2. Run OxidEQ, capturing BlackHole and playing to the real device:

       oxideq run --preset presets/example.txt \
                  --input BlackHole --output "MacBook Pro Speakers"

   Substitute any DAC name from `oxideq devices`.

## Sample rates

CoreAudio devices follow their configured rate in Audio MIDI Setup.
For bit-perfect operation, set BlackHole 2ch *and* the DAC to the
source rate there; OxidEQ locks its output stream to whatever rate the
input device reports and warns when the DAC cannot match it.

## Known limitations

- OxidEQ does no graph management: on Linux the virtual sink and links
  are set up with PipeWire tooling (see the README's "Routing"); on
  macOS, BlackHole is the sink and CoreAudio handles the default-output
  routing.
- Per-app routing needs third-party tools; OxidEQ EQ's whatever reaches
  BlackHole.
