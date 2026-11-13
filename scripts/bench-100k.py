"""Measure the code graph index at 100k files: full rebuild and one-file incremental.

Reproduces the W7-14 targets (full < 60 s, incremental < 200 ms) against a
synthetic corpus, so the numbers in the plan are a command and not a memory.
Usage: python -u scripts/bench-100k.py [--root DIR] [--vault PATH]
"""
import json, os, subprocess, sys, time

ROOT = os.environ.get("BENCH_ROOT", os.path.join(os.environ.get("TEMP", "."), "mn-bench-100k"))
VAULT = os.environ.get("BENCH_VAULT", os.path.join(os.environ.get("TEMP", "."), "mn-bench-vault.db"))
FILES = int(os.environ.get("BENCH_FILES", "100000"))
BIN = os.environ.get("MN_BIN", "mn")

def say(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", flush=True)

def build_corpus():
    body = "pub fn f{n}(x: i64) -> i64 {{ let y = x + {n}; y }}\npub struct S{n} {{ pub v: i64 }}\n"
    started = time.time()
    for d in range(FILES // 100):
        sub = os.path.join(ROOT, f"m{d:04d}")
        os.makedirs(sub, exist_ok=True)
        for f in range(100):
            n = d * 100 + f
            with open(os.path.join(sub, f"f{n:06d}.rs"), "w", encoding="utf-8") as fh:
                fh.write(body.format(n=n))
    say(f"corpus: {FILES} files in {time.time() - started:.1f}s")

def index(label, extra):
    args = [BIN, "--trust-folder", "kg", "index", "--no-embed", "--root", ROOT,
            "--vault", VAULT, "--json"] + extra
    started = time.perf_counter()
    done = subprocess.run(args, capture_output=True, text=True)
    elapsed = time.perf_counter() - started
    say(f"{label}: {elapsed:.2f}s exit={done.returncode}")
    if done.returncode != 0:
        # A failed run must never be recorded as a fast one: an argument error
        # returns in milliseconds and would read as a spectacular result.
        say(f"FAILED {label}: the binary refused the call, so this is not a measurement")
        raise SystemExit(2)
    for stream in (done.stdout, done.stderr):
        if stream.strip():
            say(stream.strip()[:300].replace("\n", " "))
    return elapsed

if __name__ == "__main__":
    if not os.path.isdir(ROOT):
        build_corpus()
    else:
        say(f"corpus present at {ROOT}")
    if os.path.exists(VAULT):
        os.remove(VAULT)
    result = {"files": FILES, "root": ROOT, "binary": BIN}
    result["full_index_secs"] = round(index("full 100k", ["--full"]), 3)
    last_dir = f"m{FILES // 100 - 1:04d}"
    warm = f"{last_dir}/f{FILES - 1:06d}.rs"
    for label, sub, tag in [("incremental cold", "m0000/f000000.rs", "touched_a"),
                            ("incremental warm", warm, "touched_b")]:
        with open(os.path.join(ROOT, sub), "a", encoding="utf-8") as fh:
            fh.write(f"pub fn {tag}() -> i64 {{ 7 }}\n")
        result[label.replace(" ", "_") + "_secs"] = round(index(label, []), 3)
    result["targets"] = {"full_secs": 60, "incremental_secs": 0.2}
    say("RESULT " + json.dumps(result))
    say("DONE")
