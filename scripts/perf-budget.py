"""W12-05 perf budgets with a 10% regression gate.

Measures the process-level metrics that do not need a model or a network:
cold start (`version`), `--help`, `doctor`, and the peak RSS of a cold
`version` run. Recall at 10k beliefs is measured by the criterion bench
(`cargo bench -p memnoth-engine --bench recall`) and the TUI frame budget by
the Rust test `a_frame_stays_inside_the_perf_budget`; both are recorded in
the baseline so the gate names them instead of silently skipping them.

Usage:
    python scripts/perf-budget.py --write   # (re)record ci/perf-baseline.json
    python scripts/perf-budget.py --check   # fail if any metric regressed >10%

The binary is `$MN_BIN`, else `./target/debug/memnoth(.exe)`, else `mn`.
"""
import json
import os
import statistics
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BASELINE = os.path.join(HERE, "..", "ci", "perf-baseline.json")
TOLERANCE = 0.10
# Absolute ceilings around the baseline recorded on the debug profile
# (Windows, 2026-11-13, ~2.1 s cold start). The CI perf job runs the release
# binary, which is faster, so it passes these with room; the relative gate
# is what catches drift on the profile the baseline was recorded on.
# Re-record with --write if the baseline profile changes.
BUDGETS = {
    "cold_start_ms": 3500.0,
    "help_ms": 3500.0,
    "doctor_ms": 4000.0,
    "idle_rss_mb": 250.0,
}


def binary():
    env = os.environ.get("MN_BIN")
    if env:
        return env
    for name in ("memnoth.exe", "memnoth"):
        candidate = os.path.join(HERE, "..", "target", "debug", name)
        if os.path.isfile(candidate):
            return candidate
    return "mn"


def timed(args, env=None, runs=5):
    samples = []
    for _ in range(runs):
        started = time.perf_counter()
        done = subprocess.run(args, capture_output=True, text=True, env=env)
        samples.append((time.perf_counter() - started) * 1000.0)
        if done.returncode != 0:
            raise SystemExit(f"FAILED {' '.join(args)}: exit={done.returncode} {done.stderr[:200]}")
    return statistics.median(samples)


def peak_rss_mb(args, env=None):
    """Peak RSS (MB) of one child run, best available source per platform."""
    try:
        import psutil  # type: ignore

        proc = psutil.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        proc.wait()
        return proc.memory_info().rss / (1024 * 1024)
    except Exception:
        pass
    if os.name == "posix":
        import resource

        subprocess.run(args, capture_output=True, env=env, check=True)
        return resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss / 1024.0
    if os.name == "nt":
        return windows_peak_rss_mb(args, env)
    return None


def windows_peak_rss_mb(args, env=None):
    """Poll GetProcessMemoryInfo while the child runs; peak working set."""
    import ctypes

    class Counters(ctypes.Structure):
        _fields_ = [
            ("cb", ctypes.c_ulong),
            ("PageFaultCount", ctypes.c_ulong),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    psapi = ctypes.windll.psapi
    get_info = psapi.GetProcessMemoryInfo
    get_info.argtypes = [ctypes.c_void_p, ctypes.POINTER(Counters), ctypes.c_ulong]
    proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    peak = 0
    while proc.poll() is None:
        counters = Counters()
        counters.cb = ctypes.sizeof(Counters)
        if get_info(proc._handle, ctypes.byref(counters), counters.cb):
            peak = max(peak, counters.PeakWorkingSetSize)
        time.sleep(0.01)
    return peak / (1024 * 1024) if peak else None


def measure():
    bin_path = binary()
    home = tempfile.mkdtemp(prefix="mn-perf-home-")
    env = dict(os.environ, MEMNOTH_HOME=os.path.join(home, "state"))
    result = {
        "cold_start_ms": round(timed([bin_path, "version"], env), 1),
        "help_ms": round(timed([bin_path, "--help"], env), 1),
        "doctor_ms": round(timed([bin_path, "--trust-folder", "doctor"], env), 1),
        "idle_rss_mb": peak_rss_mb([bin_path, "version"], env),
        # Measured elsewhere, recorded so the gate names them:
        "tui_frame_ms_budget": 25.0,
        "tui_frame_test": "a_frame_stays_inside_the_perf_budget (cargo test -p memnoth --lib)",
        "recall_10k": "cargo bench -p memnoth-engine --bench recall",
    }
    return result


def check(current, baseline):
    failures = []
    for key, budget in BUDGETS.items():
        value = current.get(key)
        ceiling = baseline.get(key)
        if value is None:
            failures.append(f"{key}: not measured on this platform")
            continue
        if ceiling is None:
            continue
        if value > budget:
            failures.append(f"{key}: {value} ms/MB exceeds absolute budget {budget}")
        if value > ceiling * (1 + TOLERANCE):
            failures.append(
                f"{key}: {value} regressed >10% over baseline {ceiling}"
            )
    return failures


def main():
    if "--write" in sys.argv:
        baseline = measure()
        baseline["tolerance"] = TOLERANCE
        baseline["budgets"] = BUDGETS
        with open(BASELINE, "w", encoding="utf-8") as fh:
            json.dump(baseline, fh, indent=2)
            fh.write("\n")
        print("baseline written:", json.dumps(baseline))
        return 0
    if not os.path.isfile(BASELINE):
        print(f"no baseline at {BASELINE}; run with --write first", file=sys.stderr)
        return 2
    with open(BASELINE, encoding="utf-8") as fh:
        baseline = json.load(fh)
    current = measure()
    failures = check(current, baseline)
    if failures:
        for failure in failures:
            print("REGRESSION", failure)
        return 1
    print("perf budget OK:", json.dumps(current))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
