#!/usr/bin/env python3
"""Seeded noise model behind the table in docs/verification/g1-g2-readiness-20261009.md. NOT A GATE RESULT.

Usage: sim_g2_noise.py [--trials 20000] [--seed 265]

Reps are lognormal around a 23 s native workload, 5 reps per arm, cowfs true add 0.5 s. Prints, per sigma, how often
compare.g2_decision decides PASS, FAIL or neither, on macOS (1.0 s budget) and Linux (1.5x). Deterministic per seed.
"""
import argparse
import math
import random
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compare  # noqa: E402

SIGMAS = (0.005, 0.01, 0.02, 0.05, 0.1, 0.2)


def rates(rnd, sigma, mac, trials, add=0.5, base=23.0, reps=5):
    """{"PASS", "FAIL", "None"} fractions; a trial whose arm spans over the 2x bound is None, as compare.py refuses it."""
    draw = lambda m: [{"wall_s": m * math.exp(rnd.gauss(0, sigma))} for _ in range(reps)]
    count = {"PASS": 0, "FAIL": 0, "None": 0}
    for _ in range(trials):
        nat, cow = draw(base), draw(base + add)
        if any(max(r["wall_s"] for r in a) > compare.G2_SPREAD_MAX * min(r["wall_s"] for r in a) for a in (nat, cow)):
            count["None"] += 1
        else:
            count[str(compare.g2_decision(nat, cow, mac, 1.0))] += 1
    return {k: v / trials for k, v in count.items()}


def table(trials=20000, seed=265):
    rnd = random.Random(seed)
    out = []
    for s in SIGMAS:
        spread = statistics.mean(max(w) / min(w) for w in ([math.exp(rnd.gauss(0, s)) for _ in range(5)] for _ in range(4000)))
        out.append((s, spread, rates(rnd, s, True, trials), rates(rnd, s, False, trials)))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--trials", type=int, default=20000)
    ap.add_argument("--seed", type=int, default=265)
    a = ap.parse_args()
    print("| sigma | mean 5-rep max/min | macOS PASS | macOS none | Linux PASS | Linux none |")
    print("| --- | --- | --- | --- | --- | --- |")
    for s, sp, m, l in table(a.trials, a.seed):
        print(f"| {s} | {sp:.2f} | {m['PASS']:.1%} | {m['None']:.1%} | {l['PASS']:.1%} | {l['None']:.1%} |")


if __name__ == "__main__":
    main()
