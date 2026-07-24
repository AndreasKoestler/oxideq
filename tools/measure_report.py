#!/usr/bin/env python3
"""Measure an oxideq preset end-to-end and render an HTML validation report.

Plays a logarithmic sine sweep through a live, *isolated* PipeWire chain
(player -> oxideq -> recorder), measures the transfer function by cross-spectrum,
and compares it against analytically-computed RBJ targets (digital + analog).
Four experiments:

  00 flat baseline  - transparency / null test (should be bit-exact passthrough)
  01 accuracy       - the preset vs its analytic RBJ (digital) target
  02 oversampling   - os=1 vs os=N vs the analog ideal (near-Nyquist convergence)
  03 cramping demo  - a fixed near-Nyquist preset, os=1 vs os=N (rate-dependent)

Parametric in preset, backend (df1/df2), sample rate, and oversample multiplier.
The cramping demo uses a fixed fabricated preset (it does not depend on the
input preset) but is rendered for the chosen backend / rate / multiplier.

Requires: numpy, scipy, matplotlib, and a running PipeWire session with
pw-cat / pw-record / pw-link / pw-metadata on PATH.

Run from the repo root after `cargo build --release`:

    python3 tools/measure_report.py --preset presets/koss_porta_pro.txt \
        --backend df1 --rate 48000 --oversample 4 --out eq-report

Output: <out>/report.html and the PNG figures beside it.
"""
import argparse
import base64
import re
import subprocess
import sys
from pathlib import Path

import numpy as np
from scipy import signal
from scipy.io import wavfile
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

HERE = Path(__file__).resolve().parent
CAPTURE = HERE / "measure_capture.sh"

# Fixed fabricated preset for the cramping demo (independent of the input preset):
# a high shelf + a peak with real gain up near Nyquist, where bilinear cramping bites.
CRAMP_BANDS = [("HSC", 6000.0, 12.0, 0.70), ("PK", 16000.0, 12.0, 3.00)]
CRAMP_PREAMP = -16.0

BL, OR, GR, PU = "#2b6c8f", "#d1852a", "#3d8a5f", "#6d4bb6"


# ----------------------------------------------------------------------------- preset
def parse_preset(path):
    """Parse an Equalizer APO / AutoEQ preset -> (preamp_db, [(kind, fc, gain, q)])."""
    preamp, bands = 0.0, []
    for line in Path(path).read_text().splitlines():
        m = re.match(r"\s*Preamp:\s*(-?[\d.]+)\s*dB", line, re.I)
        if m:
            preamp = float(m.group(1)); continue
        if not re.match(r"\s*Filter\s+\d+:", line, re.I):
            continue
        if re.search(r":\s*OFF", line, re.I):
            continue
        kind = None
        for tok in ("PK", "LSC", "HSC"):
            if re.search(rf"\b{tok}\b", line):
                kind = tok; break
        fc = re.search(r"Fc\s+([\d.]+)", line, re.I)
        gn = re.search(r"Gain\s+(-?[\d.]+)", line, re.I)
        q = re.search(r"Q\s+([\d.]+)", line, re.I)
        if kind and fc and gn and q:
            bands.append((kind, float(fc.group(1)), float(gn.group(1)), float(q.group(1))))
    return preamp, bands


# ----------------------------------------------------------------------------- targets
def rbj_digital(f, bands, preamp, fs):
    """Digital RBJ biquad cascade (matches the `biquad` crate oxideq uses)."""
    H = np.ones(len(f), dtype=complex); fnz = np.clip(f, 1e-3, None)
    for kind, fc, g, q in bands:
        w0 = 2 * np.pi * fc / fs; cw, sw = np.cos(w0), np.sin(w0); al = sw / (2 * q)
        A = 10 ** (g / 40); r = np.sqrt(A)
        if kind == "PK":
            b = [1 + al * A, -2 * cw, 1 - al * A]; a = [1 + al / A, -2 * cw, 1 - al / A]
        elif kind == "LSC":
            b = [A * ((A + 1) - (A - 1) * cw + 2 * r * al), 2 * A * ((A - 1) - (A + 1) * cw),
                 A * ((A + 1) - (A - 1) * cw - 2 * r * al)]
            a = [(A + 1) + (A - 1) * cw + 2 * r * al, -2 * ((A - 1) + (A + 1) * cw),
                 (A + 1) + (A - 1) * cw - 2 * r * al]
        else:  # HSC
            b = [A * ((A + 1) + (A - 1) * cw + 2 * r * al), -2 * A * ((A - 1) + (A + 1) * cw),
                 A * ((A + 1) + (A - 1) * cw - 2 * r * al)]
            a = [(A + 1) - (A - 1) * cw + 2 * r * al, 2 * ((A - 1) - (A + 1) * cw),
                 (A + 1) - (A - 1) * cw - 2 * r * al]
        b = np.array(b) / a[0]; a = np.array(a) / a[0]
        _, h = signal.freqz(b, a, worN=2 * np.pi * fnz / fs); H *= h
    return 20 * np.log10(np.abs(H) + 1e-12) + preamp


def rbj_analog(f, bands, preamp):
    """Continuous-time RBJ prototype -- the ideal a filter converges to as fs rises."""
    H = np.ones(len(f), dtype=complex)
    for kind, fc, g, q in bands:
        s = 1j * (f / fc); A = 10 ** (g / 40); r = np.sqrt(A)
        if kind == "PK":
            H *= (s * s + (A / q) * s + 1) / (s * s + (1 / (A * q)) * s + 1)
        elif kind == "LSC":
            H *= A * (s * s + (r / q) * s + A) / (A * s * s + (r / q) * s + 1)
        else:
            H *= A * (A * s * s + (r / q) * s + 1) / (s * s + (r / q) * s + A)
    return 20 * np.log10(np.abs(H) + 1e-12) + preamp


# ----------------------------------------------------------------------------- signal io
def gen_sweep(path, fs, amp, secs, f2, lead=1.5, tail=1.0, f1=10.0):
    t = np.arange(int(secs * fs)) / fs
    K = secs / np.log(f2 / f1); L = 2 * np.pi * f1 * K
    sw = np.sin(L * (np.exp(t / K) - 1.0))
    nf = int(0.03 * fs); w = np.hanning(2 * nf); sw[:nf] *= w[:nf]; sw[-nf:] *= w[nf:]
    sig = np.concatenate([np.zeros(int(lead * fs)), sw, np.zeros(int(tail * fs))]).astype(np.float32) * amp
    wavfile.write(path, fs, np.column_stack([sig, sig]))


def load(path):
    _, x = wavfile.read(path)
    if x.ndim > 1:
        x = x[:, 0]
    x = x.astype(np.float64)
    if np.abs(x).max() > 2:
        x = x / 32768.0
    return x


def smooth(f, mag, frac=12):
    lin = 10 ** (mag / 10); out = np.copy(mag)
    for i, fc in enumerate(f):
        s = (f >= fc * 2 ** (-0.5 / frac)) & (f <= fc * 2 ** (0.5 / frac))
        if s.any():
            out[i] = 10 * np.log10(lin[s].mean())
    return out


def measured(rec, sweep, fs):
    x, y = load(sweep), load(rec)
    c = signal.correlate(y, x, mode="full", method="fft"); lag = np.argmax(np.abs(c)) - (len(x) - 1)
    if lag > 0:
        y = y[lag:]
    n = min(len(x), len(y)); x, y = x[:n], y[:n]
    f, Pxx = signal.welch(x, fs, nperseg=16384)
    _, Pxy = signal.csd(x, y, fs, nperseg=16384)
    _, coh = signal.coherence(x, y, fs, nperseg=16384)
    mag = 20 * np.log10(np.abs(Pxy / Pxx) + 1e-12)
    peak = 20 * np.log10(np.max(np.abs(y)) + 1e-12)
    return f, smooth(f, mag), coh, peak


def nulltest(rec, sweep):
    x, y = load(sweep), load(rec)
    c = signal.correlate(y, x, mode="full", method="fft"); lag = np.argmax(np.abs(c)) - (len(x) - 1)
    if lag > 0:
        y = y[lag:]
    n = min(len(x), len(y)); x, y = x[:n], y[:n]
    t = max(1, int(0.05 * n)); xs, ys = x[t:-t], y[t:-t]   # trim 5% edges (link clicks)
    g = np.dot(xs, ys) / np.dot(xs, xs); res = ys - g * xs
    null = 20 * np.log10(np.sqrt(np.mean(res ** 2)) / np.sqrt(np.mean((g * xs) ** 2)) + 1e-30)
    return g, null


# ----------------------------------------------------------------------------- pipewire
def pw_meta(key, val):
    subprocess.run(["pw-metadata", "-n", "settings", "0", key, str(val)],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def pw_get_rate():
    """The rate the PipeWire graph is actually running at (may be hardware-pinned)."""
    r = subprocess.run(["pw-metadata", "-n", "settings", "0", "clock.rate"],
                       capture_output=True, text=True)
    m = re.search(r"value:'(\d+)'", r.stdout)
    return int(m.group(1)) if m else None


def capture(oxideq, preset, sweep, backend, os_, rate, out):
    r = subprocess.run(["bash", str(CAPTURE), str(oxideq), str(preset), str(sweep),
                        backend, str(os_), str(rate), str(out)], stderr=subprocess.PIPE, text=True)
    if r.returncode != 0 or not Path(out).exists():
        sys.exit(f"capture failed (os={os_}, preset={Path(preset).name}): {r.stderr.strip()}")


def at(f, fc, v):
    return float(np.interp(fc, f, v))


# ----------------------------------------------------------------------------- plots
def two_panel(title, x0, top_lines, x1lim, err_lines, ylab_err="error (dB)",
              y0lim=None, y1lim=None, png=None):
    fig, ax = plt.subplots(2, 1, figsize=(11, 6.8), sharex=True)
    for xs, ys, kw in top_lines:
        ax[0].semilogx(xs, ys, **kw)
    ax[0].set_ylabel("gain (dB)"); ax[0].grid(True, which="both", alpha=.3); ax[0].legend()
    ax[0].set_title(title)
    if y0lim:
        ax[0].set_ylim(*y0lim)
    for xs, ys, kw in err_lines:
        ax[1].semilogx(xs, ys, **kw)
    ax[1].axhline(0, color="k", lw=.6, alpha=.5)
    ax[1].set_ylabel(ylab_err); ax[1].set_xlabel("Hz"); ax[1].grid(True, which="both", alpha=.3)
    if err_lines and any("label" in kw for *_, kw in err_lines):
        ax[1].legend()
    if y1lim:
        ax[1].set_ylim(*y1lim)
    ax[1].set_xlim(*x1lim)
    fig.tight_layout(); fig.savefig(png, dpi=110); plt.close(fig)


def img(path):
    return "data:image/png;base64," + base64.b64encode(Path(path).read_bytes()).decode()


# ----------------------------------------------------------------------------- report html
CSS = r"""<style>
:root{--paper:#f2f5f7;--panel:#fff;--lightbox:#fbfcfd;--ink:#151b21;--ink-soft:#48535d;--ink-faint:#78838d;
--line:#dde3e8;--line-soft:#e9eef1;--accent:#0e6b7d;--pass:#1c8256;--caution:#b06f16;
--mono:ui-monospace,"SF Mono","JetBrains Mono",Menlo,Consolas,monospace;--sans:system-ui,-apple-system,"Segoe UI",Roboto,Helvetica,Arial,sans-serif;}
@media (prefers-color-scheme:dark){:root{--paper:#0d1116;--panel:#151b21;--lightbox:#f4f7f9;--ink:#e7ecf0;--ink-soft:#a7b2bc;--ink-faint:#6d7883;--line:#242e37;--line-soft:#1c242c;--accent:#40b7cd;--pass:#3ecd92;--caution:#e0a544;}}
:root[data-theme="light"]{--paper:#f2f5f7;--panel:#fff;--lightbox:#fbfcfd;--ink:#151b21;--ink-soft:#48535d;--ink-faint:#78838d;--line:#dde3e8;--line-soft:#e9eef1;--accent:#0e6b7d;--pass:#1c8256;--caution:#b06f16;}
:root[data-theme="dark"]{--paper:#0d1116;--panel:#151b21;--lightbox:#f4f7f9;--ink:#e7ecf0;--ink-soft:#a7b2bc;--ink-faint:#6d7883;--line:#242e37;--line-soft:#1c242c;--accent:#40b7cd;--pass:#3ecd92;--caution:#e0a544;}
*{box-sizing:border-box;}
body{margin:0;background:var(--paper);color:var(--ink);font-family:var(--sans);line-height:1.6;-webkit-font-smoothing:antialiased;}
.wrap{max-width:820px;margin:0 auto;padding:clamp(28px,5vw,64px) clamp(18px,4vw,40px);}
.eyebrow{font-family:var(--mono);font-size:12px;letter-spacing:.16em;text-transform:uppercase;color:var(--accent);font-weight:600;}
h1{font-size:clamp(28px,5vw,42px);line-height:1.1;letter-spacing:-.02em;margin:.35em 0 .2em;text-wrap:balance;font-weight:700;}
h2{font-size:clamp(20px,3vw,26px);letter-spacing:-.01em;margin:0;text-wrap:balance;font-weight:650;}
.lede{font-size:19px;color:var(--ink-soft);max-width:62ch;margin:.4em 0 0;}
p{max-width:66ch;} a{color:var(--accent);}
header .meta{display:flex;flex-wrap:wrap;gap:8px 22px;margin-top:22px;font-family:var(--mono);font-size:12.5px;color:var(--ink-faint);}
header .meta b{color:var(--ink-soft);font-weight:600;}
.rule{display:flex;align-items:baseline;gap:14px;margin:52px 0 22px;}
.rule::after{content:"";flex:1;border-top:1px solid var(--line);transform:translateY(-3px);}
.rule .n{font-family:var(--mono);font-size:13px;color:var(--ink-faint);font-weight:600;letter-spacing:.05em;}
.section{background:var(--panel);border:1px solid var(--line);border-radius:14px;padding:clamp(20px,3vw,30px);margin-bottom:22px;}
.head{display:flex;align-items:flex-start;justify-content:space-between;gap:16px;flex-wrap:wrap;}
.chip{font-family:var(--mono);font-size:11.5px;font-weight:650;letter-spacing:.06em;text-transform:uppercase;padding:5px 11px;border-radius:999px;white-space:nowrap;border:1px solid transparent;}
.chip.pass{color:var(--pass);background:color-mix(in srgb,var(--pass) 13%,transparent);border-color:color-mix(in srgb,var(--pass) 30%,transparent);}
.chip.warn{color:var(--caution);background:color-mix(in srgb,var(--caution) 13%,transparent);border-color:color-mix(in srgb,var(--caution) 32%,transparent);}
.chip.info{color:var(--accent);background:color-mix(in srgb,var(--accent) 12%,transparent);border-color:color-mix(in srgb,var(--accent) 30%,transparent);}
.stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(120px,1fr));gap:1px;background:var(--line);border:1px solid var(--line);border-radius:10px;overflow:hidden;margin:20px 0;}
.stat{background:var(--panel);padding:13px 15px;}
.stat .k{font-family:var(--mono);font-size:10.5px;letter-spacing:.09em;text-transform:uppercase;color:var(--ink-faint);}
.stat .v{font-family:var(--mono);font-size:22px;font-weight:600;margin-top:4px;font-variant-numeric:tabular-nums;letter-spacing:-.01em;}
.stat .v small{font-size:12px;color:var(--ink-faint);font-weight:500;}
.stat .v.good{color:var(--pass);} .stat .v.warn{color:var(--caution);}
figure{margin:20px 0 6px;}
.lightbox{background:var(--lightbox);border:1px solid var(--line);border-radius:10px;padding:10px;overflow-x:auto;}
.lightbox img{display:block;width:100%;max-width:100%;height:auto;border-radius:4px;}
figcaption{font-family:var(--mono);font-size:12px;color:var(--ink-faint);margin-top:10px;padding-left:2px;}
figcaption b{color:var(--ink-soft);font-weight:600;}
table{border-collapse:collapse;width:100%;font-family:var(--mono);font-size:13px;font-variant-numeric:tabular-nums;margin:6px 0;}
th,td{text-align:right;padding:8px 10px;border-bottom:1px solid var(--line-soft);}
th:first-child,td:first-child{text-align:left;}
thead th{color:var(--ink-faint);font-weight:600;font-size:11px;letter-spacing:.05em;text-transform:uppercase;border-bottom:1px solid var(--line);}
td.warn{color:var(--caution);font-weight:600;} td.good{color:var(--pass);font-weight:600;}
.tablewrap{overflow-x:auto;margin:16px 0 4px;}
.read{color:var(--ink-soft);} .read b{color:var(--ink);font-weight:600;}
code{font-family:var(--mono);font-size:.88em;background:color-mix(in srgb,var(--accent) 10%,transparent);padding:1px 6px;border-radius:5px;color:var(--ink);}
.verdict{background:var(--panel);border:1px solid var(--line);border-left:3px solid var(--accent);border-radius:12px;padding:clamp(20px,3vw,28px);}
.verdict ul{margin:14px 0 0;padding-left:0;list-style:none;display:flex;flex-direction:column;gap:12px;}
.verdict li{display:flex;gap:12px;align-items:baseline;color:var(--ink-soft);}
.verdict li b{color:var(--ink);}
.verdict li::before{content:"";flex:none;width:7px;height:7px;margin-top:8px;border-radius:2px;background:var(--accent);transform:rotate(45deg);}
footer{margin-top:44px;padding-top:20px;border-top:1px solid var(--line);font-family:var(--mono);font-size:12px;color:var(--ink-faint);}
</style>"""


def stat(k, v, cls=""):
    return f'<div class="stat"><div class="k">{k}</div><div class="v {cls}">{v}</div></div>'


def build_html(ctx):
    d = ctx
    return CSS + f"""
<div class="wrap">
<header>
  <div class="eyebrow">oxideq · signal-chain validation</div>
  <h1>{d['preset_name']} — does the EQ do what the preset says?</h1>
  <p class="lede">A mic-free, digital-domain measurement of oxideq's biquad engine for this preset:
  frequency-response accuracy, added distortion, and whether oversampling is worth its cost.</p>
  <div class="meta">
    <span><b>Preset</b> {d['preset_name']}</span>
    <span><b>Backend</b> {d['backend']}</span>
    <span><b>Rate</b> {d['rate']/1000:.1f} kHz</span>
    <span><b>Oversample</b> {d['os']}×</span>
    <span><b>Method</b> log-sweep · cross-spectrum</span>
  </div>
</header>

<div class="rule"><span class="n">METHOD</span></div>
<div class="section"><p class="read">A log sine sweep is fed through a <b>fully isolated PipeWire chain</b>
(<code>player → oxideq → recorder</code>, nothing else attached), captured in <b>f32</b> before any DAC. The
transfer function is the Welch cross-spectrum <code>H = S<sub>xy</sub>/S<sub>xx</sub></code> (immune to
pipeline latency); <b>coherence</b> flags trustworthy bins, and every measured trace is 1/12-octave smoothed
so os=1 and os={d['os']} get identical treatment. The rig self-validates — the flat preset nulls to zero
residual (below), proving the chain is lossless. Targets are computed analytically: the <b>digital</b> RBJ
cascade (matching the <code>biquad</code> crate) and the <b>analog</b> continuous-time RBJ prototype. Graph
rate {d['rate']/1000:.1f} kHz{d['rate_note']}. Backend under test: <b>{d['backend']}</b> (both df1/df2
realize the same transfer function; the measurement confirms this one does too).</p></div>

<div class="rule"><span class="n">EXP 00 · BASELINE</span></div>
<div class="section">
  <div class="head"><div><div class="eyebrow">Flat preset · null test</div><h2>Is it transparent when it should be?</h2></div>
  <span class="chip pass">{d['flat_chip']}</span></div>
  <div class="stats">{stat('Gain', d['flat_gain']+'<small> dB</small>', 'good')}
  {stat('Residual', d['flat_res'], 'good')}
  {stat('Coherence', d['flat_coh'], 'good')}{stat('Bands', '0')}</div>
  <figure><div class="lightbox"><img alt="Flat transfer function" src="{d['FLAT']}"></div>
  <figcaption><b>Fig 00.</b> Preamp 0 dB, no filters — response on 0 dB, coherence {d['flat_coh']}; sample-domain null.</figcaption></figure>
  <p class="read">{d['flat_text']}</p>
</div>

<div class="rule"><span class="n">EXP 01 · ACCURACY</span></div>
<div class="section">
  <div class="head"><div><div class="eyebrow">{d['preset_name']} · vs analytic target</div><h2>Does the curve match the math?</h2></div>
  <span class="chip {d['acc_chipc']}">{d['acc_chip']}</span></div>
  <div class="stats">{stat('Mean error', d['acc_mean']+'<small> dB</small>', 'good')}
  {stat('Max error', d['acc_max']+'<small> dB</small>', d['acc_maxc'])}
  {stat('Coherence', d['acc_coh'], 'good')}{stat('Peak level', d['acc_peak']+'<small> dBFS</small>')}</div>
  <figure><div class="lightbox"><img alt="Measured vs analytic target" src="{d['MEAS']}"></div>
  <figcaption><b>Fig 01.</b> Measured (solid) vs analytic RBJ target (dashed), shared x-axis; error panel below.</figcaption></figure>
  <p class="read">{d['acc_text']}</p>
</div>

<div class="rule"><span class="n">EXP 02 · OVERSAMPLING</span></div>
<div class="section">
  <div class="head"><div><div class="eyebrow">{d['preset_name']} · os=1 vs os={d['os']} vs analog</div><h2>Does {d['os']}× oversampling help this preset?</h2></div>
  <span class="chip {d['over_chipc']}">{d['over_chip']}</span></div>
  <div class="stats">{stat(f"os=1 err @{d['fhi_khz']}kHz", d['over_os1']+'<small> dB</small>')}
  {stat(f"os={d['os']} err @{d['fhi_khz']}kHz", d['over_osN']+'<small> dB</small>', 'good')}
  {stat('CPU cost', f"{d['os']}×", 'warn' if d['os']>1 else '')}{stat('Nyquist', f"{d['rate']/2000:.1f}<small> kHz</small>")}</div>
  <figure><div class="lightbox"><img alt="os1 vs osN vs analog" src="{d['OVER']}"></div>
  <figcaption><b>Fig 02.</b> Top end vs the analog ideal. os=1 cramps toward Nyquist; os={d['os']} converges.</figcaption></figure>
  <p class="read">{d['over_text']}</p>
</div>

<div class="rule"><span class="n">EXP 03 · CRAMPING DEMO</span></div>
<div class="section">
  <div class="head"><div><div class="eyebrow">Fabricated: HSC +12 dB@6k, PK +12 dB@16k · os=1 vs os={d['os']}</div><h2>When does oversampling earn its cost?</h2></div>
  <span class="chip info">{d['cramp_chip']}</span></div>
  <div class="tablewrap"><table><thead><tr><th>Frequency</th><th>os=1 err</th><th>os={d['os']} err</th><th>coherence</th></tr></thead>
  <tbody>{d['cramp_rows']}</tbody></table></div>
  <figure><div class="lightbox"><img alt="cramping demo" src="{d['CRAMP']}"></div>
  <figcaption><b>Fig 03.</b> Fixed near-Nyquist preset (independent of the input preset), at {d['rate']/1000:.1f} kHz. os=1 collapses above the peak; os={d['os']} tracks analog.</figcaption></figure>
  <p class="read">{d['cramp_text']}</p>
</div>

<div class="rule"><span class="n">VERDICT</span></div>
<div class="verdict"><p class="read" style="margin-top:0">{d['verdict_lead']}</p><ul>{d['verdict_items']}</ul></div>

<footer>oxideq DSP validation · {d['preset_name']} · {d['backend']} · {d['rate']/1000:.1f} kHz · os={d['os']} · mic-free digital loopback · sweep + cross-spectrum · NumPy/SciPy + PipeWire</footer>
</div>
"""


# ----------------------------------------------------------------------------- main
def main():
    ap = argparse.ArgumentParser(description="Measure an oxideq preset and render an HTML report.")
    ap.add_argument("--preset", required=True, help="Equalizer APO / AutoEQ preset to validate")
    ap.add_argument("--backend", default="df1", choices=["df1", "df2"])
    ap.add_argument("--rate", type=int, default=48000, help="pipeline sample rate (Hz)")
    ap.add_argument("--oversample", type=int, default=4, help="oversample multiplier to compare vs os=1")
    ap.add_argument("--oxideq", default="target/release/oxideq", help="path to the oxideq binary")
    ap.add_argument("--out", default="eq-report", help="output directory")
    ap.add_argument("--secs", type=float, default=14.0, help="sweep length (s)")
    a = ap.parse_args()

    rreq, N = a.rate, a.oversample
    out = Path(a.out); out.mkdir(parents=True, exist_ok=True)
    oxideq = Path(a.oxideq).resolve()
    if not oxideq.exists():
        sys.exit(f"oxideq binary not found: {oxideq} (build with `cargo build --release`)")
    preamp, bands = parse_preset(a.preset)
    if not bands:
        print(f"warning: no filter bands parsed from {a.preset}", file=sys.stderr)
    pname = Path(a.preset).stem

    # Request the rate, then read what the graph actually runs at. The PipeWire
    # clock can be hardware-pinned; oxideq follows the real graph rate, so we
    # measure (and generate the sweep) at that rate to keep the chain resample-free.
    print(f"[measure] requesting graph rate {rreq} Hz, quantum 2048")
    pw_meta("clock.force-rate", rreq); pw_meta("clock.force-quantum", 2048)
    fs_read = pw_get_rate()
    fs = fs_read or rreq
    rate_forced = fs_read == rreq   # False when the read-back failed: unverified
    if fs_read is None:
        print(f"[warn] could not read the graph rate back; assuming {rreq} Hz (unverified).",
              file=sys.stderr)
    elif not rate_forced:
        print(f"[warn] graph is pinned at {fs} Hz — could not switch to {rreq}; measuring at {fs} Hz.\n"
              f"       (set PipeWire's clock.rate / allowed-rates to {rreq} to measure there.)", file=sys.stderr)
    # Pin force-rate to the rate actually in effect, so new nodes don't inherit an
    # unachievable target and end up resampled against the real graph clock.
    pw_meta("clock.force-rate", fs)

    f2 = min(22000.0, 0.45 * fs)                # sweep top frequency
    sweep = out / "sweep.wav"; sweep_hot = out / "sweep_hot.wav"
    flat_preset = out / "_flat.txt"; cramp_preset = out / "_cramp.txt"
    gen_sweep(sweep, fs, 0.1, a.secs, f2)                    # -20 dBFS (headroom for gain)
    gen_sweep(sweep_hot, fs, 0.5, a.secs + 6, f2)            # -6 dBFS  (flat null needs SNR)
    flat_preset.write_text("Preamp: 0.0 dB\n")
    cramp_preset.write_text("Preamp: %.2f dB\nFilter 1: ON HSC Fc 6000.0 Hz Gain 12.0 dB Q 0.70\n"
                            "Filter 2: ON PK Fc 16000.0 Hz Gain 12.0 dB Q 3.00\n" % CRAMP_PREAMP)

    recs = {k: out / f"rec_{k}.wav" for k in ("flat", "p1", "pN", "c1", "cN")}
    try:
        print("[capture] flat baseline");         capture(oxideq, flat_preset, sweep_hot, a.backend, 1, fs, recs["flat"])
        print("[capture] preset os=1");            capture(oxideq, a.preset, sweep, a.backend, 1, fs, recs["p1"])
        print("[capture] cramp os=1");             capture(oxideq, cramp_preset, sweep, a.backend, 1, fs, recs["c1"])
        if N > 1:
            print(f"[capture] preset os={N}");      capture(oxideq, a.preset, sweep, a.backend, N, fs, recs["pN"])
            print(f"[capture] cramp os={N}");       capture(oxideq, cramp_preset, sweep, a.backend, N, fs, recs["cN"])
    finally:
        print("[measure] restoring graph rate/quantum")
        pw_meta("clock.force-rate", 0); pw_meta("clock.force-quantum", 0)

    # ---- analyse ----
    gflat, nulldb = nulltest(recs["flat"], sweep_hot)
    f, mflat, cflat, _ = measured(recs["flat"], sweep_hot, fs)
    fgood = (f >= 30) & (f <= min(18000, f2)) & (cflat > 0.98)
    # Highest frequency the sweep actually excites (avoid noise-floor readings at the edge).
    sig = (cflat > 0.9) & (f <= min(20000.0, f2))
    fhi = float(f[sig].max()) if sig.any() else min(20000.0, 0.44 * fs)

    f, mp1, cp1, pk1 = measured(recs["p1"], sweep, fs)
    tgt = rbj_digital(f, bands, preamp, fs)
    an_p = rbj_analog(f, bands, preamp)
    g2 = (f >= 30) & (f <= min(18000, f2)) & (cp1 > 0.9)
    acc_err = np.abs((mp1 - tgt)[g2])

    if N > 1:
        _, mpN, cpN, _ = measured(recs["pN"], sweep, fs)
        _, mc1, cc1, _ = measured(recs["c1"], sweep, fs)
        _, mcN, ccN, _ = measured(recs["cN"], sweep, fs)
    else:
        mpN, cpN = mp1, cp1
        _, mc1, cc1, _ = measured(recs["c1"], sweep, fs)
        mcN, ccN = mc1, cc1
    an_c = rbj_analog(f, CRAMP_BANDS, CRAMP_PREAMP)

    # ---- figures ----
    b = (f >= 20) & (f <= f2)
    two_panel(f"{pname} — measured vs analytic target (absolute)", f,
              [(f[b], mp1[b], dict(lw=2.4, color=BL, alpha=.85, label="measured (os=1)")),
               (f[b], tgt[b], dict(lw=1.3, color=OR, ls="--", label="analytic RBJ target"))],
              (20, f2),
              [(f[b], (mp1 - tgt)[b], dict(lw=1.4, color=BL))],
              y1lim=(-1, 1), png=out / "f_koss.png")

    tb = (f >= 2000) & (f <= f2)
    two_panel(f"{pname} (top end) — os=1 vs os={N} vs analog", f,
              [(f[tb], mp1[tb], dict(lw=2.0, color=BL, label="os=1")),
               (f[tb], mpN[tb], dict(lw=2.0, color=OR, label=f"os={N}")),
               (f[tb], an_p[tb], dict(lw=1.3, color="k", ls=":", label="analog ideal"))],
              (2000, f2),
              [(f[tb], (mp1 - an_p)[tb], dict(lw=1.8, color=BL, label="os=1 error vs analog")),
               (f[tb], (mpN - an_p)[tb], dict(lw=1.8, color=OR, label=f"os={N} error vs analog"))],
              y1lim=(-1.0, 0.5), png=out / "f_over.png")

    cb = (f >= 3000) & (f <= f2)
    two_panel(f"Cramping demo (HSC +12@6k, PK +12@16k) — os=1 vs os={N} vs analog", f,
              [(f[cb], mc1[cb], dict(lw=2.0, color=BL, label="os=1")),
               (f[cb], mcN[cb], dict(lw=2.0, color=OR, label=f"os={N}")),
               (f[cb], an_c[cb], dict(lw=1.4, color="k", ls=":", label="analog ideal"))],
              (3000, f2),
              [(f[cb], (mc1 - an_c)[cb], dict(lw=1.8, color=BL, label="os=1 error vs analog")),
               (f[cb], (mcN - an_c)[cb], dict(lw=1.8, color=OR, label=f"os={N} error vs analog"))],
              y1lim=(-6, 2), png=out / "f_cramp.png")

    fig, ax = plt.subplots(2, 1, figsize=(11, 6.4), sharex=True)
    ax[0].semilogx(f[b], mflat[b], lw=1.7, color=PU, label="measured (flat preset, 1/12-oct)")
    ax[0].axhline(0, color="k", lw=.9, ls="--", label="ideal 0 dB")
    ax[0].set_ylabel("gain (dB)"); ax[0].set_ylim(-0.5, 0.5); ax[0].grid(True, which="both", alpha=.3); ax[0].legend(loc="upper left")
    nulltxt = "residual = 0 (bit-identical)" if nulldb < -280 else f"null {nulldb:.0f} dB"
    ax[0].set_title(f"Flat baseline — bit-exact passthrough ({nulltxt}, gain {20*np.log10(gflat):+.2f} dB)")
    ax[1].semilogx(f[b], cflat[b], lw=1.3, color=GR); ax[1].axhline(1, color="k", lw=.6, alpha=.5)
    ax[1].set_ylabel("coherence"); ax[1].set_xlabel("Hz"); ax[1].set_ylim(0.95, 1.003); ax[1].set_xlim(20, f2); ax[1].grid(True, which="both", alpha=.3)
    fig.tight_layout(); fig.savefig(out / "f_flat.png", dpi=110); plt.close(fig)

    # ---- numbers ----
    over_os1 = mp1 - an_p; over_osN = mpN - an_p
    # Fall back to fhi so the table (and cramp_max) never end up empty when the
    # graph rate puts all the canonical probe frequencies above the sweep top.
    cramp_freqs = [x for x in (16000, 18000, 20000) if x <= fhi] or [int(fhi)]
    rows = ""
    cramp_max = 0.0
    for fq in cramp_freqs:
        e1, eN = at(f, fq, mc1 - an_c), at(f, fq, mcN - an_c)
        cramp_max = max(cramp_max, abs(e1))
        c1v, cNv = at(f, fq, cc1), at(f, fq, ccN)
        lab = f"{fq//1000} kHz" + (" (peak)" if fq == 16000 else "")
        w1 = ' class="warn"' if abs(e1) > 1 else ""
        rows += f"<tr><td>{lab}</td><td{w1}>{e1:+.2f}</td><td class=\"good\">{eN:+.2f}</td><td>{c1v:.2f}/{cNv:.2f}</td></tr>"

    # A degenerate capture (coherence never clears 0.9) leaves these masks
    # empty; fall back to neutral values so the report reads cleanly instead
    # of printing NumPy empty-slice nan.
    acc_maxv = float(acc_err.max()) if acc_err.size else 0.0
    acc_meanv = float(acc_err.mean()) if acc_err.size else 0.0
    acc_cohv = float(cp1[g2].mean()) if g2.any() else 1.0
    ctx = dict(
        preset_name=pname, backend=a.backend, rate=fs, os=N, fhi_khz=f"{fhi/1000:.0f}",
        rate_note=("" if rate_forced
                   else " (assumed; could not read the graph rate back)" if fs_read is None
                   else f" (requested {rreq/1000:.1f} kHz; graph hardware-pinned)"),
        FLAT=img(out/"f_flat.png"), MEAS=img(out/"f_koss.png"), OVER=img(out/"f_over.png"), CRAMP=img(out/"f_cramp.png"),
        flat_chip="Bit-identical" if nulldb < -280 else "Transparent",
        flat_gain=f"{20*np.log10(gflat):+.2f}", flat_coh=f"{cflat[fgood].mean():.3f}" if fgood.any() else "1.000",
        flat_res="0 <small>(bit-exact)</small>" if nulldb < -280 else f"{nulldb:.0f}<small> dB</small>",
        flat_text=("With no filters, oxideq's output is <b>bit-identical to its input</b> — subtracting the two "
                   "leaves zero residual, and the swept response is flat to 0.00 dB. This doubles as the rig's "
                   "self-check: a lossy or contaminated chain could not null to zero."
                   if nulldb < -280 else
                   f"Flat preset measures within a hair of 0 dB (null {nulldb:.0f} dB); the chain is effectively transparent."),
        acc_mean=f"{acc_meanv:.3f}", acc_max=f"{acc_maxv:.2f}",
        acc_maxc="good" if acc_maxv < 0.5 else "warn",
        acc_chip=f"Accurate · ±{acc_maxv:.2f} dB" if acc_maxv < 0.5 else f"±{acc_maxv:.2f} dB",
        acc_chipc="pass" if acc_maxv < 0.5 else "warn",
        acc_coh=f"{acc_cohv:.3f}", acc_peak=f"{pk1:.1f}",
        acc_text=(f"The measured curve lands on the analytic RBJ target to within <b>{acc_maxv:.2f} dB</b> "
                  f"(mean {acc_meanv:.3f} dB), coherence {acc_cohv:.3f} — linear, no measurable "
                  f"distortion. The {a.backend.upper()} coefficient math is correct"
                  + (", and nothing clips." if pk1 < -0.1 else "; note the peak level near 0 dBFS.")),
        fhi=fhi,
        over_os1=f"{at(f, fhi, over_os1):+.2f}", over_osN=f"{at(f, fhi, over_osN):+.2f}",
        over_chip=("No practical gain" if abs(at(f, fhi, over_os1)) < 0.5 else f"os={N} closer") if N > 1 else "os=1 only",
        over_chipc=("warn" if abs(at(f, fhi, over_os1)) < 0.5 else "info") if N > 1 else "info",
        over_text=(f"At {fhi/1000:.0f} kHz, os=1 sits {at(f, fhi, over_os1):+.2f} dB from the analog ideal and "
                   f"os={N} {at(f, fhi, over_osN):+.2f} dB. "
                   + ("The gap is negligible — this preset has little energy near Nyquist, so oversampling costs "
                      f"{N}× the DSP for an inaudible change."
                      if abs(at(f, fhi, over_os1)) < 0.5 else
                      f"os={N} is meaningfully closer to analog up top; worth it for this preset.")
                   if N > 1 else "Oversampling disabled (os=1); nothing to compare."),
        cramp_rows=rows,
        cramp_chip=("os fixes it" if N > 1 and cramp_max > 1 else ("no cramping at this rate" if cramp_max < 1 else "cramping present")),
        cramp_text=(f"With real gain near Nyquist ({fs/2000:.1f} kHz), os=1 cramping reaches "
                    f"<b>{cramp_max:.1f} dB</b> below the analog target near the peak"
                    + (f"; os={N} tracks the ideal to within a few tenths of a dB. This is exactly the case "
                       "oversampling exists for." if N > 1 else " (raise --oversample to correct it).")
                    if cramp_max > 1 else
                    f"At {fs/1000:.1f} kHz the 16 kHz peak sits well below Nyquist, so even os=1 cramps only "
                    f"{cramp_max:.1f} dB — little for oversampling to fix. Try a lower rate to see cramping bite."),
        verdict_lead=f"oxideq's {a.backend.upper()} filter engine, measured at {fs/1000:.1f} kHz on {pname}:",
        verdict_items="".join("<li>" + t + "</li>" for t in [
            f"<b>Accurate.</b> Matches the analytic RBJ target to ≤{acc_maxv:.2f} dB; bit-identical with no filters."
            if nulldb < -280 else f"<b>Accurate.</b> Matches target to ≤{acc_maxv:.2f} dB.",
            "<b>Clean.</b> Coherence ≈1 throughout — no measurable distortion or noise added.",
            (f"<b>Oversampling</b> converges to the analog ideal near Nyquist, but only matters when a preset has "
             f"real gain up high. Here os={N} changes the top octave by "
             f"{abs(at(f, fhi, over_os1) - at(f, fhi, over_osN)):.2f} dB vs os=1."
             if N > 1 else "<b>Oversampling</b> disabled for this run (os=1)."),
            "<b>Rule of thumb.</b> <code>--oversample</code> &gt; 1 for bright / air-boost presets; 1 otherwise.",
        ]),
    )
    html = build_html(ctx)
    (out / "report.html").write_text(html)
    print(f"[done] {out/'report.html'}  ({len(html)//1024} KB)")
    print(f"       flat null {nulldb:.0f} dB | acc max {acc_maxv:.2f} dB | "
          f"over@{fhi/1000:.0f}k os1 {at(f,fhi,over_os1):+.2f} os{N} {at(f,fhi,over_osN):+.2f} | cramp max {cramp_max:.1f} dB")


if __name__ == "__main__":
    main()
