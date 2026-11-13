#!/usr/bin/env python3
"""Pinned-model CI gate (W12-02).

Runs N=3 paired repetitions of a fixed task set against one or more pinned
(model, effort) configurations, computes a 95% percentile bootstrap confidence
interval on each configuration's mean score, and gates the measured cost per
1k turns against the pinned price table.

Paired: every configuration runs the same tasks under the same seeds, so a
difference between configurations is attributable to the model/effort, not to
task or ordering variance.

The manifest (ci/manifest.json) pins: commit, dataset, seed, price table,
the (model, effort) pairs, N, the bootstrap size, and the CI level.

Usage:
    python ci/eval_bootstrap.py ci/manifest.json

Exit codes:
    0  every configuration is within its price-table gate
    1  any configuration regresses or the run cannot be completed
    2  usage error
"""
from __future__ import annotations

import json
import os
import random
import statistics
import subprocess
import sys
from typing import Any


def load_manifest(path: str) -> dict[str, Any]:
    with open(path, "r", encoding="utf-8") as fh:
        return json.load(fh)


def bootstrap_ci(
    scores: list[float], ci_level: float, n_bootstrap: int, seed: int
) -> tuple[float, float]:
    """Percentile bootstrap 100*ci_level% CI on the mean of `scores`."""
    rng = random.Random(seed)
    n = len(scores)
    if n < 2:
        raise ValueError(f"need at least 2 scores for a bootstrap CI, got {n}")
    means = [statistics.mean(rng.choices(scores, k=n)) for _ in range(n_bootstrap)]
    means.sort()
    alpha = (1.0 - ci_level) / 2.0
    lo_index = max(0, min(n_bootstrap - 1, int(alpha * n_bootstrap)))
    hi_index = max(0, min(n_bootstrap - 1, int((1.0 - alpha) * n_bootstrap)))
    return means[lo_index], means[hi_index]


def run_once(
    binary: list[str], model: str, effort: str | None, prompt: str, seed: int
) -> float:
    """Run one turn and return its score (0..1)."""
    cmd = list(binary) + ["ask", "--model", model]
    if effort is not None:
        cmd += ["--effort", effort]
    cmd += ["--seed", str(seed), "--json", prompt]
    completed = subprocess.run(
        cmd, capture_output=True, text=True, check=True, timeout=300
    )
    envelope = json.loads(completed.stdout)
    return float(envelope["score"])


def evaluate(
    binary: list[str],
    model: str,
    effort: str | None,
    tasks: list[dict[str, Any]],
    seed: int,
) -> tuple[float, float]:
    """Run all tasks once under `seed`; return (mean_score, total_cost_usd)."""
    scores: list[float] = []
    cost_usd = 0.0
    for task in tasks:
        score = run_once(binary, model, effort, task["prompt"], seed)
        scores.append(score)
        cost_usd += float(task.get("cost_usd", 0.0))
    return statistics.mean(scores), cost_usd


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: python ci/eval_bootstrap.py <manifest.json>", file=sys.stderr)
        return 2
    manifest = load_manifest(argv[1])

    commit = manifest["commit"]
    dataset_path = manifest["dataset"]
    seed = int(manifest["seed"])
    n = int(manifest["n"])
    ci_level = float(manifest.get("ci_level", 0.95))
    n_bootstrap = int(manifest.get("n_bootstrap", 20000))
    price_table = manifest["price_table"]
    models = manifest["models"]

    with open(dataset_path, "r", encoding="utf-8") as fh:
        tasks = [json.loads(line) for line in fh if line.strip()]
    if not tasks:
        print("dataset is empty", file=sys.stderr)
        return 1

    # A single built binary for every repetition so only the model/effort
    # varies between measurements.
    binary_env = os.environ.get("MEMNOTH_BIN")
    binary = [binary_env] if binary_env else ["cargo", "run", "--quiet", "--locked", "--bin", "memnoth", "--"]

    failures: list[str] = []
    print(
        f"pinned model CI — commit {commit}, dataset {dataset_path}, "
        f"seed {seed}, N={n}, bootstrap={n_bootstrap}, ci={ci_level}"
    )
    print(
        f"{'model':<26} {'effort':<8} {'mean':<7} {'ci_lo':<7} {'ci_hi':<7} "
        f"{'cost_per_1k':<10}  gate"
    )

    for entry in models:
        model = entry["id"]
        effort = entry.get("effort")
        prices = price_table.get(model)
        if prices is None:
            failures.append(f"{model}: missing price table entry")
            print(
                f"{model:<26} {(effort or '-'):<8} {'N/A':<7} {'N/A':<7} "
                f"{'N/A':<7} {'N/A':<10}  FAIL no price entry"
            )
            continue

        rep_scores: list[float] = []
        rep_costs: list[float] = []
        for rep in range(n):
            rep_seed = seed + rep
            mean_score, cost_usd = evaluate(binary, model, effort, tasks, rep_seed)
            rep_scores.append(mean_score)
            rep_costs.append(cost_usd)

        mean = statistics.mean(rep_scores)
        ci_lo, ci_hi = bootstrap_ci(rep_scores, ci_level, n_bootstrap, seed)
        mean_cost = statistics.mean(rep_costs)
        cost_per_1k = mean_cost * 1000.0

        input_price = float(prices.get("input_per_mtok", 0.0))
        output_price = float(prices.get("output_per_mtok", 0.0))
        price_ceiling = max(input_price, output_price)
        price_ok = (
            cost_per_1k <= price_ceiling if price_ceiling > 0.0 else True
        )

        if not price_ok:
            failures.append(
                f"{model}: cost per 1k turns {cost_per_1k:.4f} exceeds "
                f"price-table ceiling {price_ceiling:.2f}"
            )

        status = "ok" if price_ok else "FAIL price"
        print(
            f"{model:<26} {(effort or '-'):<8} "
            f"{mean:<7.4f} {ci_lo:<7.4f} {ci_hi:<7.4f} "
            f"{cost_per_1k:<10.4f}  {status}"
        )

    if failures:
        print("\nPINNED MODEL CI FAILED:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("\npinned model CI: all configurations within price-table gate")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
