#!/usr/bin/env python3
"""Golden test vectors for oxideq's resampler and EQ.

Independent reimplementation of the DSP in numpy/scipy. Run offline
from the repo root (never during `cargo test`):

    python3 tools/gen_fixtures.py

Requires numpy and scipy:  python3 -m pip install --user numpy scipy
Output: tests/data/*.f64 — raw little-endian float64.
"""

from pathlib import Path

import numpy as np
from scipy import signal

OUT = Path("tests/data")
ATTEN_DB = 120.0


def halfband_taps(fs_in: float, fp: float = 20_000.0) -> np.ndarray:
    """Mirror of src/resample.rs::halfband_taps, via scipy.firwin."""
    fp = min(fp, 0.45 * fs_in)
    dw = np.pi * (1.0 - 2.0 * fp / fs_in)
    n = int(np.ceil((ATTEN_DB - 7.95) / (2.285 * dw) + 1.0))
    while n % 4 != 3:
        n += 1
    beta = 0.1102 * (ATTEN_DB - 8.7)
    # cutoff 0.5 of Nyquist at the output rate == fs_out/4. firwin's
    # default scale=True normalizes DC gain to exactly 1, matching Rust.
    return signal.firwin(n, 0.5, window=("kaiser", beta))


def rbj_peaking(fs: float, f0: float, gain_db: float, q: float):
    """RBJ cookbook peaking EQ, normalized (a0 == 1)."""
    a = 10.0 ** (gain_db / 40.0)
    w0 = 2.0 * np.pi * f0 / fs
    alpha = np.sin(w0) / (2.0 * q)
    b = np.array([1.0 + alpha * a, -2.0 * np.cos(w0), 1.0 - alpha * a])
    den = np.array([1.0 + alpha / a, -2.0 * np.cos(w0), 1.0 - alpha / a])
    return b / den[0], den / den[0]


def up2x(taps: np.ndarray, x: np.ndarray) -> np.ndarray:
    """Streaming-truncated 2x interpolation: first 2*len(x) samples."""
    return signal.upfirdn(2.0 * taps, x, up=2)[: 2 * len(x)]


def down2x(taps: np.ndarray, v: np.ndarray) -> np.ndarray:
    """Streaming-truncated 2x decimation: first len(v)//2 samples."""
    return signal.upfirdn(taps, v, down=2)[: len(v) // 2]


def write(name: str, arr: np.ndarray) -> None:
    (OUT / name).write_bytes(np.asarray(arr, dtype="<f8").tobytes())
    print(f"  {name}: {len(arr)} samples")


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)

    # -- taps ---------------------------------------------------------
    for fs in (44_100.0, 48_000.0, 88_200.0, 96_000.0):
        write(f"hb_taps_{int(fs)}.f64", halfband_taps(fs))

    # -- single-stage 2x upsampler waveform @ 48 k --------------------
    rng = np.random.default_rng(0xB1A5)
    x = rng.uniform(-1.0, 1.0, 1024)
    write("up2x_in.f64", x)
    write("up2x_out_48000.f64", up2x(halfband_taps(48_000.0), x))

    # -- EQ waveform, factor 1 @ 48 k ---------------------------------
    # Preset: preamp -3 dB; Peaking 1 kHz +6 dB Q 1; Peaking 8 kHz
    # -4 dB Q 2. Peaking-only: shelf formulas differ between cookbook
    # variants, and shelves are covered by the transfer-function
    # proptest instead.
    eq_in = rng.uniform(-1.0, 1.0, 4096).astype(np.float32)
    write("eq_in.f64", eq_in.astype(np.float64))

    def run_eq(sig64: np.ndarray, fs: float) -> np.ndarray:
        z = sig64 * 10.0 ** (-3.0 / 20.0)  # preamp first, like EqChain
        for f0, g, q in ((1_000.0, 6.0, 1.0), (8_000.0, -4.0, 2.0)):
            b, a = rbj_peaking(fs, f0, g, q)
            z = signal.lfilter(b, a, z)
        return z

    out_1x = run_eq(eq_in.astype(np.float64), 48_000.0)
    write("eq_out_48000_1x.f64", out_1x.astype(np.float32).astype(np.float64))

    # -- full 4x-oversampled EQ chain @ 44.1 k ------------------------
    t1 = halfband_taps(44_100.0)
    t2 = halfband_taps(88_200.0)
    z = eq_in.astype(np.float64) * 10.0 ** (-3.0 / 20.0)
    z = up2x(t1, z)
    z = up2x(t2, z)
    for f0, g, q in ((1_000.0, 6.0, 1.0), (8_000.0, -4.0, 2.0)):
        b, a = rbj_peaking(4 * 44_100.0, f0, g, q)
        z = signal.lfilter(b, a, z)
    z = down2x(t2, z)
    z = down2x(t1, z)
    write("eq_out_44100_4x.f64", z.astype(np.float32).astype(np.float64))


if __name__ == "__main__":
    main()
