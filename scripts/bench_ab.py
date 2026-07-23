#!/usr/bin/env python3
"""A/B DSP benchmark harness for oxideq.

Runs `cargo bench --bench dsp` as an *outer* loop: N warmup rounds
(discarded) then M measured rounds. Each round runs ONE invocation per tree,
interleaved A,B,A,B — so slow machine drift (thermal ramp, background load)
hits both trees roughly equally and cancels out of the delta, instead of
biasing it the way "all A, then all B" phasing does. Each invocation's
criterion point-estimate per bench-id is one sample; we average the M
samples per tree.

Two trees are measured:
  * baseline  -- a git ref (default: main), checked out into a throwaway
                 worktree so its *committed* state is measured in isolation.
  * current   -- the working tree in place, INCLUDING uncommitted changes
                 (this is where the optimization under test lives).

Only bench-ids present in BOTH trees are compared; others are still printed.

Usage:
  python3 scripts/bench_ab.py                  # main vs working tree, defaults
  python3 scripts/bench_ab.py --warmup 3 --reps 20
  python3 scripts/bench_ab.py --baseline-ref HEAD   # committed HEAD vs working tree
  python3 scripts/bench_ab.py --measurement-time 2 --sample-size 20
"""

from __future__ import annotations

import argparse
import os
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
#  taking the FIRST time: after each bench-id line. `time:`/`change:` lines
#  themselves can't match BENCH_ID_RE: the colon isn't in \w.)
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
        m_id = BENCH_ID_RE.match(line.strip())
        if m_id:
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


@dataclass
class Tree:
    """One side of the A/B: where to run and which cargo target dir to use."""

    label: str
    cwd: Path
    target: str | None  # CARGO_TARGET_DIR override; None = the tree's default

    def env(self) -> dict[str, str] | None:
        if self.target is None:
            return None
        return {**os.environ, "CARGO_TARGET_DIR": self.target}


def run_bench(tree: Tree, crit_args: list[str]) -> str:
    cmd = ["cargo", "bench", "--bench", "dsp", "--", *crit_args]
    proc = subprocess.run(
        cmd, cwd=tree.cwd, capture_output=True, text=True, env=tree.env(), check=False
    )
    if proc.returncode != 0:
        sys.exit(
            f"cargo bench failed in {tree.cwd}:\n"
            f"{proc.stdout[-2000:]}\n{proc.stderr[-2000:]}"
        )
    return proc.stdout


def build(tree: Tree) -> None:
    print(f"  building {tree.label}: {tree.cwd} ...", flush=True)
    proc = subprocess.run(
        ["cargo", "bench", "--bench", "dsp", "--no-run"],
        cwd=tree.cwd,
        capture_output=True,
        text=True,
        env=tree.env(),
        check=False,
    )
    if proc.returncode != 0:
        sys.exit(f"build failed in {tree.cwd}:\n{proc.stderr[-3000:]}")


def measured_invocation(tree: Tree, crit_args: list[str]) -> dict[str, float]:
    inv = parse_invocation(run_bench(tree, crit_args))
    if not inv:
        sys.exit(f"parsed no bench results from an invocation in {tree.cwd}")
    return inv


def summarize(inv: dict[str, float]) -> str:
    return " ".join(f"{k.split('_')[-1]}={v:.2f}" for k, v in sorted(inv.items()))


def report(baseline: TreeStats, current: TreeStats, reps: int) -> None:
    ids = sorted(set(baseline.samples) | set(current.samples))
    print("\n" + "=" * 78)
    print(f"RESULTS  (mean +/- stdev over m={reps} interleaved rounds, us)")
    print("=" * 78)
    print(f"{'bench id':<12}{'baseline':>18}{'current':>18}{'delta':>10}")
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
    ap.add_argument("--warmup", type=int, default=3, help="discarded warmup rounds")
    ap.add_argument("--reps", type=int, default=15, help="measured rounds")
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
    try:
        subprocess.run(
            ["git", "worktree", "add", "--detach", str(wt), args.baseline_ref],
            cwd=repo, capture_output=True, text=True, check=True,
        )
        base_tree = Tree(f"BASELINE ({args.baseline_ref})", wt, str(tmp / "target"))
        cur_tree = Tree("CURRENT (working tree)", repo, None)

        # Build both up front so compile time never lands inside a measured
        # round; then interleave every round A,B so drift cancels.
        build(base_tree)
        build(cur_tree)

        for i in range(args.warmup):
            print(f"  warmup {i + 1}/{args.warmup} (discarded, A+B)", flush=True)
            run_bench(base_tree, crit_args)
            run_bench(cur_tree, crit_args)

        baseline = TreeStats(base_tree.label)
        current = TreeStats(cur_tree.label)
        for i in range(args.reps):
            inv_b = measured_invocation(base_tree, crit_args)
            inv_c = measured_invocation(cur_tree, crit_args)
            baseline.add(inv_b)
            current.add(inv_c)
            print(
                f"  round {i + 1}/{args.reps}: "
                f"base[{summarize(inv_b)}]  cur[{summarize(inv_c)}]",
                flush=True,
            )
        report(baseline, current, args.reps)
    finally:
        subprocess.run(
            ["git", "worktree", "remove", "--force", str(wt)],
            cwd=repo, capture_output=True, text=True, check=False,
        )


if __name__ == "__main__":
    main()
