//! Perf-budget OBSERVATION — the real binary, this host, honest numbers.
//!
//! This is an observation gate for the definition-of-done budgets:
//!
//! | metric | hard budget |
//! |---|---|
//! | cold start (`version`) | < 150 ms |
//! | `--help` | < 80 ms |
//! | recall over 10k beliefs | < 50 ms |
//! | one TUI frame | < 16 ms |
//! | idle RSS | < 200 MB |
//!
//! It spawns the *built binary* with fresh state and every cloud/provider key
//! removed. Process-level metrics (cold start, `--help`, idle RSS) are measured
//! here; recall and TUI frame are measured by the committed criterion bench and
//! lib perf test, so this test marks them `"not_measured"` unless the CI script
//! injects a value it actually measured (`MN_PERF_RECALL_MS` /
//! `MN_PERF_TUI_FRAME_MS`). A number is never invented to satisfy a row.
//!
//! The check is opt-in: set `MN_PERF_OBSERVE=1` (the Bash script does this).
//! A plain `cargo test --workspace` therefore stays fast and deterministic.
//! When enabled, the test FAILS if any measured metric exceeds its hard budget
//! or regresses more than 10% against the committed per-OS baseline. It writes
//! a dated JSON artifact (`schema: mn-perf-budget-observation/1`) recording
//! OS/arch, profile, baseline scope and every metric's status.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

const SCHEMA: &str = "mn-perf-budget-observation/1";
const TOLERANCE: f64 = 0.10;

struct Budget {
    key: &'static str,
    unit: &'static str,
    max: f64,
    note: &'static str,
}

/// The definition-of-done ceilings. Cold start/`--help`/RSS come from
/// `plans/08-hardening.md`; recall and frame are the same table's rows.
const BUDGETS: &[Budget] = &[
    Budget {
        key: "cold_start_ms",
        unit: "ms",
        max: 150.0,
        note: "cold start to first prompt (release, warm cache)",
    },
    Budget {
        key: "help_ms",
        unit: "ms",
        max: 80.0,
        note: "mn --help",
    },
    Budget {
        key: "recall_10k_beliefs_ms",
        unit: "ms",
        max: 50.0,
        note: "vault recall over 10k beliefs",
    },
    Budget {
        key: "tui_frame_ms",
        unit: "ms",
        max: 16.0,
        note: "one live TUI frame",
    },
    Budget {
        key: "idle_rss_mb",
        unit: "MB",
        max: 200.0,
        note: "idle resident memory of a local-only process",
    },
];

fn budget_for(key: &str) -> &'static Budget {
    BUDGETS
        .iter()
        .find(|budget| budget.key == key)
        .expect("every metric has a budget row")
}

// --------------------------------------------------------------------- fixture

struct Fixture {
    // Held for the whole test; dropping it removes the fresh state.
    _dir: tempfile::TempDir,
    home: PathBuf,
    workspace: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("allocate a fresh temp dir");
        let home = dir.path().join("home");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir_all(&home).expect("create fresh HOME");
        std::fs::create_dir_all(&workspace).expect("create fresh workspace");
        Fixture {
            _dir: dir,
            home,
            workspace,
        }
    }
}

/// `MN_PERF_BIN` lets CI point at a specific (e.g. release) binary. Otherwise
/// use the binary Cargo built for this test target.
fn binary() -> String {
    std::env::var("MN_PERF_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_memnoth").to_string())
}

fn command(fx: &Fixture, args: &[&str]) -> Command {
    let mut cmd = Command::new(binary());
    cmd.args(args)
        .current_dir(&fx.workspace)
        // Fresh state: the state dir, vault and graph all live in the fixture.
        .env("MEMNOTH_HOME", &fx.home)
        .env("HOME", &fx.home)
        .env("USERPROFILE", &fx.home)
        .env_remove("MEMNOTH_VAULT")
        .env_remove("MEMNOTH_GRAPH")
        // Offline: no control plane, no provider keys, no inherited identity.
        .env_remove("MEMNOTH_CLOUD_URL")
        .env_remove("MEMNOTH_API_KEY")
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("KILO_API_KEY")
        .env_remove("KILOCODE_API_KEY");
    cmd
}

/// Wall-clock milliseconds of one process start, including spawn. `None` when
/// the process could not spawn or exited non-zero — never a timing we trust.
fn timed(fx: &Fixture, args: &[&str]) -> Option<f64> {
    let start = Instant::now();
    let out = command(fx, args).output().ok()?;
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    out.status.success().then_some(ms)
}

fn median(mut samples: Vec<f64>) -> Option<f64> {
    if samples.is_empty() {
        return None;
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = samples.len() / 2;
    if samples.len() % 2 == 1 {
        samples.get(mid).copied()
    } else {
        samples
            .get(mid.wrapping_sub(1))
            .and_then(|low| samples.get(mid).map(|high| (low + high) / 2.0))
    }
}

fn timed_median(fx: &Fixture, args: &[&str], runs: usize) -> (Option<f64>, Vec<String>) {
    let mut samples = Vec::with_capacity(runs);
    let mut problems = Vec::new();
    for _ in 0..runs {
        match timed(fx, args) {
            Some(ms) => samples.push(ms),
            None => problems.push(format!(
                "{} did not spawn or exited non-zero",
                args.join(" ")
            )),
        }
    }
    (median(samples), problems)
}

// ------------------------------------------------------------------------- RSS

/// Peak resident working set of a single local-only `version` process. The
/// process is sampled while it runs; if the platform has no sampler (or the
/// process leaves before one sample) the metric stays "not measured".
#[cfg(any(target_os = "windows", target_os = "linux"))]
fn idle_rss(fx: &Fixture) -> (Option<f64>, String) {
    let mut child = match command(fx, &["version"]).spawn() {
        Ok(child) => child,
        Err(err) => {
            return (
                None,
                format!("could not spawn binary for RSS sample: {err}"),
            );
        }
    };

    #[cfg(target_os = "windows")]
    let value = windows_rss::peak_mb(&mut child);
    #[cfg(target_os = "linux")]
    let value = linux_rss::peak_mb(&mut child);

    let note = if value.is_some() {
        "peak resident working set sampled during one local-only `version` process \
         (GetProcessMemoryInfo on Windows, /proc VmRSS on Linux)"
            .to_string()
    } else {
        "a sampler exists on this OS but the process could not be sampled \
         (too short-lived or OS call denied); left not measured"
            .to_string()
    };
    (value, note)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
fn idle_rss(_fx: &Fixture) -> (Option<f64>, String) {
    (
        None,
        format!(
            "no RSS sampler for {}/{}; left not measured",
            std::env::consts::OS,
            std::env::consts::ARCH
        ),
    )
}

#[cfg(target_os = "windows")]
mod windows_rss {
    use std::ffi::c_void;
    use std::mem::MaybeUninit;
    use std::process::Child;
    use std::time::Duration;

    #[repr(C)]
    struct ProcessMemoryCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set_size: usize,
        working_set_size: usize,
        quota_peak_paged_pool_usage: usize,
        quota_paged_pool_usage: usize,
        quota_peak_non_paged_pool_usage: usize,
        quota_non_paged_pool_usage: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[link_name = "OpenProcess"]
        fn open_process(desired_access: u32, inherit_handle: i32, process_id: u32) -> *mut c_void;
        #[link_name = "CloseHandle"]
        fn close_handle(handle: *mut c_void) -> i32;
    }

    #[link(name = "psapi")]
    unsafe extern "system" {
        #[link_name = "GetProcessMemoryInfo"]
        fn get_process_memory_info(
            process: *mut c_void,
            counters: *mut ProcessMemoryCounters,
            cb: u32,
        ) -> i32;
    }

    pub fn peak_mb(child: &mut Child) -> Option<f64> {
        const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
        const PROCESS_VM_READ: u32 = 0x0010;
        let handle =
            unsafe { open_process(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, child.id()) };
        if handle.is_null() {
            return None;
        }

        let mut peak: u64 = 0;
        let mut counters = MaybeUninit::<ProcessMemoryCounters>::zeroed();
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
            let ok = unsafe {
                let ptr = counters.as_mut_ptr();
                (*ptr).cb = std::mem::size_of::<ProcessMemoryCounters>() as u32;
                get_process_memory_info(handle, ptr, (*ptr).cb) != 0
            };
            if ok {
                let current = unsafe { (*counters.as_ptr()).working_set_size } as u64;
                peak = peak.max(current);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.wait();
        unsafe {
            close_handle(handle);
        }
        (peak > 0).then(|| peak as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(target_os = "linux")]
mod linux_rss {
    use std::process::Child;
    use std::time::Duration;

    pub fn peak_mb(child: &mut Child) -> Option<f64> {
        let status = format!("/proc/{}/status", child.id());
        let mut peak_kb: u64 = 0;
        loop {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => {}
            }
            if let Ok(text) = std::fs::read_to_string(&status) {
                for line in text.lines() {
                    if let Some(rest) = line.strip_prefix("VmRSS:") {
                        if let Some(kb) = rest
                            .split_whitespace()
                            .next()
                            .and_then(|value| value.parse::<u64>().ok())
                        {
                            peak_kb = peak_kb.max(kb);
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let _ = child.wait();
        (peak_kb > 0).then(|| peak_kb as f64 / 1024.0)
    }
}

// --------------------------------------------------------- injected / baseline

/// A metric this test cannot measure itself. Accept a value only when the CI
/// script supplies one it measured elsewhere; record the provenance.
fn injected(env_key: &str) -> (Option<f64>, String) {
    match std::env::var(env_key) {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(value) if value.is_finite() && value > 0.0 => (
                Some(value),
                format!("{env_key}={value} injected by CI from a real measurement"),
            ),
            Ok(_) => (
                None,
                format!("{env_key} was not a positive finite number; left not measured"),
            ),
            Err(_) => (
                None,
                format!("{env_key} was not parseable; left not measured"),
            ),
        },
        Err(_) => (
            None,
            format!(
                "not measured on this host; measured by another committed gate \
                 (see measured_by), which sets {env_key}"
            ),
        ),
    }
}

fn load_baseline() -> (Option<Value>, String) {
    let path = std::env::var_os("MN_PERF_BASELINE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ci/perf-baseline.json"));
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => (Some(value), path.display().to_string()),
            Err(err) => (
                None,
                format!("unparseable baseline {}: {err}", path.display()),
            ),
        },
        Err(err) => (None, format!("no baseline at {}: {err}", path.display())),
    }
}

/// Baseline key aliases: the committed file predates these exact names.
fn baseline_value(baseline: Option<&Value>, key: &str) -> Option<f64> {
    let baseline = baseline?;
    let alias = match key {
        "recall_10k_beliefs_ms" => "recall_10k_ms",
        "tui_frame_ms" => "tui_frame_ms_budget",
        other => other,
    };
    baseline
        .get(alias)
        .and_then(Value::as_f64)
        .filter(|value| *value > 0.0)
}

// ------------------------------------------------------------------- artifacts

/// UTC `YYYY-MM-DD` and `YYYY-MM-DDTHH:MM:SSZ` without a date crate.
fn utc_now() -> (String, String) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    let date = format!("{year:04}-{month:02}-{day:02}");
    let stamp = format!(
        "{date}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    );
    (stamp, date)
}

fn observation_path(date: &str) -> PathBuf {
    if let Some(path) = std::env::var_os("MN_PERF_ARTIFACT") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("TMPDIR")
        .or_else(|| std::env::var_os("TEMP"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(format!(
        "mn-perf-budget-{}-{date}.json",
        std::env::consts::OS
    ))
}

// --------------------------------------------------------------------- the test

#[test]
fn perf_budget_observation() {
    match std::env::var("MN_PERF_OBSERVE").as_deref() {
        Ok("1") | Ok("true") | Ok("yes") => {}
        _ => {
            eprintln!(
                "perf-budget observation is opt-in: set MN_PERF_OBSERVE=1 \
                 (scripts/perf-budget.sh does this)"
            );
            return;
        }
    }

    let runs = std::env::var("MN_PERF_RUNS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);

    let fx = Fixture::new();
    let (observed_at, date) = utc_now();
    let (baseline, baseline_path) = load_baseline();
    // The committed baseline has no `os` field; its own header records a
    // Windows 2026-11-13 origin. Never compare a different platform against it.
    let baseline_os = baseline
        .as_ref()
        .and_then(|value| value.get("os"))
        .and_then(Value::as_str)
        .unwrap_or("windows");
    let regression_applied = baseline_os == std::env::consts::OS;

    let mut problems: Vec<String> = Vec::new();

    let (cold, cold_problems) = timed_median(&fx, &["version"], runs);
    problems.extend(cold_problems);
    let (help, help_problems) = timed_median(&fx, &["--help"], runs);
    problems.extend(help_problems);
    let (rss, rss_note) = idle_rss(&fx);
    let (recall, recall_note) = injected("MN_PERF_RECALL_MS");
    let (frame, frame_note) = injected("MN_PERF_TUI_FRAME_MS");

    let measurements: Vec<(&Budget, Option<f64>, String)> = vec![
        (
            budget_for("cold_start_ms"),
            cold,
            format!("median of {runs} local starts of `version` against fresh state"),
        ),
        (
            budget_for("help_ms"),
            help,
            format!("median of {runs} local starts of `--help` against fresh state"),
        ),
        (budget_for("recall_10k_beliefs_ms"), recall, recall_note),
        (budget_for("tui_frame_ms"), frame, frame_note),
        (budget_for("idle_rss_mb"), rss, rss_note),
    ];

    let mut metric_rows = Vec::new();
    let mut hard_failures: Vec<String> = Vec::new();
    let mut regression_failures: Vec<String> = Vec::new();
    let mut not_measured: Vec<&str> = Vec::new();

    for (budget, value, note) in measurements {
        let base = baseline_value(baseline.as_ref(), budget.key);
        let (status, regression_pct, result) = match value {
            Some(value) => {
                let mut result = "pass";
                if value > budget.max {
                    hard_failures.push(format!(
                        "{}: {value:.2} {} exceeds hard budget {} {}",
                        budget.key, budget.unit, budget.max, budget.unit
                    ));
                    result = "fail";
                }
                let mut pct = None;
                if regression_applied && let Some(baseline_value) = base {
                    let delta = (value - baseline_value) / baseline_value * 100.0;
                    pct = Some(delta);
                    if delta > TOLERANCE * 100.0 {
                        regression_failures.push(format!(
                            "{}: {value:.2} regressed {delta:.1}% over baseline {baseline_value:.2}",
                            budget.key
                        ));
                        result = "fail";
                    }
                }
                ("measured", pct, result)
            }
            None => {
                not_measured.push(budget.key);
                ("not_measured", None, "not_measured")
            }
        };

        let measured_by = match budget.key {
            "recall_10k_beliefs_ms" => "cargo bench -p memnoth-engine --bench recall",
            "tui_frame_ms" => "cargo test -p memnoth --lib a_frame_stays_inside_the_perf_budget",
            _ => "this test (real binary on this host)",
        };

        metric_rows.push(json!({
            "metric": budget.key,
            "status": status,
            "value": value,
            "unit": budget.unit,
            "hard_budget": budget.max,
            "hard_budget_note": budget.note,
            "measured_by": measured_by,
            "baseline": base,
            "baseline_comparison": if regression_applied { "applied" } else { "skipped_os_mismatch" },
            "regression_pct": regression_pct,
            "result": result,
            "note": note,
        }));
    }

    let gate_pass = hard_failures.is_empty() && regression_failures.is_empty();
    if !gate_pass {
        problems.extend(hard_failures.iter().cloned());
        problems.extend(regression_failures.iter().cloned());
    }

    let hard_budgets: Map<String, Value> = BUDGETS
        .iter()
        .map(|budget| {
            (
                budget.key.to_string(),
                json!({ "max": budget.max, "unit": budget.unit }),
            )
        })
        .collect();

    let artifact = json!({
        "schema": SCHEMA,
        "label": "n>=1 local process starts on THIS host; medians, not a fleet distribution",
        "observed_at": observed_at,
        "context": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "family": std::env::consts::FAMILY,
            "cpu_count": std::thread::available_parallelism().ok().map(|n| n.get()),
            "host": std::env::var("COMPUTERNAME")
                .or_else(|_| std::env::var("HOSTNAME"))
                .ok(),
            "binary": binary(),
            "binary_version": env!("CARGO_PKG_VERSION"),
            "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
            "offline": true,
        },
        "sample": {
            "runs": runs,
            "statistic": "median",
            "fresh_state": true,
            "warm_os_cache": true,
        },
        "budgets": hard_budgets,
        "baseline": {
            "path": baseline_path,
            "detected_os": baseline_os,
            "host_os": std::env::consts::OS,
            "comparison": if regression_applied { "applied" } else { "skipped_os_mismatch" },
            "tolerance": TOLERANCE,
        },
        "gate": {
            "result": if gate_pass { "pass" } else { "fail" },
            "hard_budget_failures": hard_failures.clone(),
            "regression_failures": regression_failures.clone(),
            "not_measured": not_measured.clone(),
        },
        "metrics": metric_rows.clone(),
        "notes": [
            "recall and TUI frame are measured by committed gates outside this test; \
             they stay not_measured unless the CI script injects a value it measured",
            "the committed baseline has no os field; its recorded origin is Windows \
             2026-11-13, so the 10% relative gate is applied only on Windows",
            "cold start and --help budgets are release-profile targets; the artifact \
             records the profile so a debug overage is read in context",
        ],
        "problems": problems.clone(),
    });

    let path = observation_path(&date);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let rendered = serde_json::to_string_pretty(&artifact).expect("render observation artifact");
    if let Err(err) = std::fs::write(&path, &rendered) {
        problems.push(format!(
            "could not write observation artifact to {}: {err}",
            path.display()
        ));
    }
    println!("{rendered}");
    eprintln!("perf-budget observation artifact: {}", path.display());

    assert!(
        gate_pass,
        "perf budget gate failed: {}",
        problems.join("; ")
    );
    assert!(
        problems.is_empty(),
        "perf observation had problems: {}",
        problems.join("; ")
    );
}
