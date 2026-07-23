#!/usr/bin/env bash
# Isolated single-capture helper for measure_report.py.
#
# Builds a fully isolated PipeWire chain:  sweep player -> oxideq -> recorder,
# with nothing else attached, so other audio playing on the system cannot leak
# into the measurement. The caller (measure_report.py) forces the PipeWire graph
# rate/quantum before invoking this and restores them afterwards.
#
# Usage:
#   measure_capture.sh <oxideq-bin> <preset> <sweep.wav> <backend> <oversample> <rate> <out.wav>
set -u
OX=$1; PRESET=$2; SWEEP=$3; BACKEND=$4; OS=$5; RATE=$6; OUT=$7
NODE=oxideq_meas

# Poll until a port with the exact name exists on the given side (~8 s timeout;
# the sleep makes the window wall-clock-bound, not spawn-speed-bound).
wait_port() { for _ in $(seq 1 400); do pw-link "$1" 2>/dev/null | grep -q "^$2\$" && return 0; sleep 0.02; done; return 1; }

PIPEWIRE_PROPS="{ node.name=$NODE node.autoconnect=false }" \
  "$OX" run --preset "$PRESET" --input pipewire --output pipewire --buffer 2048 \
  --backend "$BACKEND" --oversample "$OS" >/dev/null 2>&1 &
OXPID=$!
RECPID=""; PLAYPID=""
cleanup() { kill $OXPID $RECPID $PLAYPID 2>/dev/null; wait 2>/dev/null; }
trap cleanup EXIT

wait_port -i "$NODE:input_FL" || { echo "measure_capture: oxideq input port never appeared" >&2; exit 1; }

pw-record --target 0 -P node.name=measrec --channels 2 --channel-map Stereo \
  --rate "$RATE" --format f32 --latency 300ms "$OUT" >/dev/null 2>&1 &
RECPID=$!
wait_port -i "measrec:input_FL" || { echo "measure_capture: recorder port never appeared" >&2; exit 1; }

# Player starts unlinked; the sweep's silent lead-in covers the link moment.
pw-cat --playback --target 0 -P node.name=measplay "$SWEEP" >/dev/null 2>&1 &
PLAYPID=$!
wait_port -o "measplay:output_FL" || { echo "measure_capture: player port never appeared" >&2; exit 1; }
pw-link measplay:output_FL "$NODE:input_FL" 2>/dev/null
pw-link measplay:output_FR "$NODE:input_FR" 2>/dev/null

# Linking the input activates oxideq's output stream, so its output ports appear now.
wait_port -o "$NODE:output_FL" || { echo "measure_capture: oxideq output port never appeared" >&2; exit 1; }
pw-link "$NODE:output_FL" measrec:input_FL 2>/dev/null
pw-link "$NODE:output_FR" measrec:input_FR 2>/dev/null

wait $PLAYPID   # play the whole sweep; trap tears everything down on exit
