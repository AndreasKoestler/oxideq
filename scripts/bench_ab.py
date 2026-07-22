#!/usr/bin/env python3
"""A/B DSP benchmark harness for oxideq.

Runs `cargo bench --bench dsp` as an *outer* loop: N warmup invocations
(discarded) then M measured invocations, per tree. Each invocation's criterion
point-estimate per bench-id is one sample; we average the M samples. Averaging
whole invocations cancels the run-to-run drift a single before/after delta can't.

Two trees are measured:
  * baseline  -- a git ref (default: main), checked out into a throwaway
                 worktree so its *committed* state is measured in isolation.
  * current   -- the working tree in place, INCLUDING uncommitted changes
                 (this is where the optimization under test lives).

Only bench-ids present in BOTH trees are compared; others are still printed.

Usage:
  python3 bench_ab.py                         # main vs working tree, defaults
  python3 bench_ab.py --warmup 3 --reps 20
  python3 bench_ab.py --baseline-ref HEAD     # committed HEAD vs working tree
  python3 bench_ab.py --measurement-time 2 --sample-size 20
"""

from __future__ import annotations

import argparse
import re
import statistics
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

# criterion emits, per bench:
#   dsp/process_256frame_stereo_10band_4x
#                           time:   [56.510 us 56.598 us 56.690 us]
# (a `change:` block may follow with its own `time:` line — we skip it by only
#  taking the FIRST time: after each bench-id line.)
BENCH_ID_RE = re.compile(r"^(?P<id>[\w/]+/[\w]+)\s*$")
TIME_RE = re.compile(
    r"time:\s*\[\s*[\d.]+\s+\w+\s+(?P<mid>[\d.]+)\s+(?P<unit>\w+)\s+[\d.]+\s+\w+\s*\]"
)
UNIT_TO_US = {"ns": 1e-3, "us": 1.0, "µs": 1.0, "ms": 1e3, "s": 1e6}


def to_us(value: float, unit: str) -> float:
    if unit not in UNIT_TO_US:
        raise ValueError(f"unknown criterion time unit: {unit!r}")
    return value * UNIT_TO_US[unit]


def parse_invocation(stdout: str) -> dict[str, float]:
    """One cargo-bench invocation -> {bench_id: microseconds}."""
    results: dict[str, float] = {}
    pending_id: str | None = None
    for line in stdout.splitlines():
        stripped = line.strip()
        m_id = BENCH_ID_RE.match(stripped)
        if m_id and not stripped.startswith(("time:", "change:")):
            pending_id = m_id.group("id")
            continue
        if pending_id is not None:
            m_t = TIME_RE.search(line)
            if m_t:
                results[pending_id] = to_us(float(m_t.group("mid")), m_t.group("unit"))
                pending_id = None  # ignore the change: time: that may follow
    return results


@dataclass
class TreeStats:
    label: str
    # bench_id -> list of per-invocation microsecond samples
    samples: dict[str, list[float]] = field(default_factory=dict)

    def add(self, invocation: dict[str, float]) -> None:
        for bid, us in invocation.items():
            self.samples.setdefault(bid, []).append(us)

    def mean(self, bid: str) -> float:
        return statistics.fmean(self.samples[bid])

    def stdev(self, bid: str) -> float:
        xs = self.samples[bid]
        return statistics.stdev(xs) if len(xs) > 1 else 0.0


def run_bench(cwd: Path, crit_args: list[str], env_target: str | None) -> str:
    cmd = ["cargo", "bench", "--bench", "dsp", "--", *crit_args]
    env = None
    if env_target:
        import os

        env = {**os.environ, "CARGO_TARGET_DIR": env_target}
    proc = subprocess.run(
        cmd, cwd=cwd, capture_output=True, text=True, env=env, check=False
    )
    if proc.returncode != 0:
        sys.exit(
            f"cargo bench failed in {cwd}:\n{proc.stdout[-2000:]}\n{proc.stderr[-2000:]}"
        )
    return proc.stdout


def build(cwd: Path, env_target: str | None) -> None:
    import os

    env = {**os.environ, "CARGO_TARGET_DIR": env_target} if env_target else None
    print(f"  building {cwd} ...", flush=True)
    proc = subprocess.run(
        ["cargo", "bench", "--bench", "dsp", "--no-run"],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"build failed in {cwd}:\n{proc.stderr[-3000:]}")


def measure_tree(
    label: str, cwd: Path, warmup: int, reps: int, crit_args: list[str], target: str | None
) -> TreeStats:
    print(f"\n== {label}: {cwd} ==", flush=True)
    build(cwd, target)
    for i in range(warmup):
        print(f"  warmup {i + 1}/{warmup} (discarded)", flush=True)
        run_bench(cwd, crit_args, target)
    stats = TreeStats(label)
    for i in range(reps):
        out = run_bench(cwd, crit_args, target)
        inv = parse_invocation(out)
        if not inv:
            sys.exit(f"parsed no bench results from an invocation:\n{out[-2000:]}")
        stats.add(inv)
        summary = "  ".join(f"{k.split('_')[-1]}={v:.2f}us" for k, v in inv.items())
        print(f"  rep {i + 1}/{reps}: {summary}", flush=True)
    return stats


def report(baseline: TreeStats, current: TreeStats, reps: int) -> None:
    ids = sorted(set(baseline.samples) | set(current.samples))
    print("\n" + "=" * 78)
    print(f"RESULTS  (mean +/- stdev over m={reps} measured invocations, us)")
    print("=" * 78)
    hdr = f"{'bench id':<12}{'baseline':>18}{'current':>18}{'delta':>10}"
    print(hdr)
    print("-" * 78)
    for bid in ids:
        short = bid.replace("dsp/process_256frame_stereo_10band_", "")
        b = f"{baseline.mean(bid):.2f}+/-{baseline.stdev(bid):.2f}" if bid in baseline.samples else "--"
        c = f"{current.mean(bid):.2f}+/-{current.stdev(bid):.2f}" if bid in current.samples else "--"
        if bid in baseline.samples and bid in current.samples:
            delta = (current.mean(bid) - baseline.mean(bid)) / baseline.mean(bid) * 100.0
            d = f"{delta:+.1f}%"
        else:
            d = "n/a"
        print(f"{short:<12}{b:>18}{c:>18}{d:>10}")
    print("-" * 78)
    print("negative delta = current is faster. 1x is the bit-exact NFR path")
    print("(<53.3us budget); 4x/16x are informational oversampled paths.")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--warmup", type=int, default=3, help="discarded outer invocations")
    ap.add_argument("--reps", type=int, default=15, help="measured outer invocations")
    ap.add_argument("--baseline-ref", default="main", help="git ref for baseline tree")
    ap.add_argument("--warm-up-time", type=float, default=0.5, help="criterion per-run warmup s")
    ap.add_argument("--measurement-time", type=float, default=1.5, help="criterion per-run measure s")
    ap.add_argument("--sample-size", type=int, default=10, help="criterion samples per run (>=10)")
    args = ap.parse_args()

    repo = Path(
        subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
    )

    crit_args = [
        "--warm-up-time", str(args.warm_up_time),
        "--measurement-time", str(args.measurement_time),
        "--sample-size", str(args.sample_size),
    ]

    print(f"repo: {repo}")
    print(f"baseline ref: {args.baseline_ref}   warmup: {args.warmup}   reps: {args.reps}")
    print(f"criterion per-run: {crit_args}")

    tmp = Path(tempfile.mkdtemp(prefix="oxideq-bench-baseline-"))
    wt = tmp / "wt"
    target = str(tmp / "target")
    try:
        subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt), args.baseline_ref],
            cwd=repo, capture_output=True, text=True, check=True,
        )
        baseline = measure_tree(
            f"BASELINE ({args.baseline_ref})", wt, args.warmup, args.reps, crit_args, target
        )
        current = measure_tree(
            "CURRENT (working tree)", repo, args.warmup, args.reps, crit_args, None
        )
        report(baseline, current, args.reps)
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", str(wt)],
            cwd=repo, capture_output=True, text=True, check=False,
        )


if __name__ == "__main__":
    main()
