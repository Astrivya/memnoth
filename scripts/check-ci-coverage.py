#!/usr/bin/env python3
"""Statically prove that the CI test matrix covers every workspace member and
every integration test exactly once.

Stdlib only, no network, no PyYAML. A focused YAML reader handles the block
style used by .github/workflows/ci.yml; cargo/fuzz semantics are inferred from
the archive argument tokens. Paths default relative to the CLI repository root
(the parent of scripts/), so this works from CLI/, from the workspace root, or
from any other cwd.

Usage:
    python3 scripts/check-ci-coverage.py [--ci PATH] [--cargo PATH] [--tests DIR]
    python3 scripts/check-ci-coverage.py --self-test

Exit 0 when every check passes, 1 on any FAIL (WARNs are informational).
"""

from __future__ import annotations

import argparse
import re
import shlex
import sys
from dataclasses import dataclass, field
from pathlib import Path

ALL_KINDS = ("lib", "bins", "tests", "examples", "benches")
BLOCK_INDICATORS = ("|-", "|+", ">-", ">+", "|", ">")
WINDOWS_MARKER = "windows"


# --------------------------------------------------------------------------- #
# Focused YAML reader (block mappings / block sequences / block scalars)
# --------------------------------------------------------------------------- #
def _scalar(value: str) -> str:
    v = value.strip()
    if len(v) >= 2 and v[0] == v[-1] and v[0] in ("'", '"'):
        return v[1:-1]
    return v


def _quoted_strings(text: str) -> list[str]:
    return [m.group(0)[1:-1] for m in re.finditer(r"\"[^\"]*\"|'[^']*'", text)]


class _YamlParser:
    def __init__(self, text: str):
        self.raw = text.splitlines()
        self.pos = 0

    def _next_sig(self):
        """Advance past blanks/comments; leave pos on the next significant line."""
        while self.pos < len(self.raw):
            line = self.raw[self.pos]
            stripped = line.strip()
            if not stripped or stripped.startswith("#"):
                self.pos += 1
                continue
            indent = len(line) - len(line.lstrip(" "))
            return indent, stripped
        return None

    def _peek(self):
        # Leave the cursor ON the next significant line (blanks/comments are
        # skipped) so callers' `pos += 1` consumes that line, not a comment.
        return self._next_sig()

    def parse(self):
        sig = self._peek()
        return self._block(sig[0]) if sig else {}

    def _block(self, indent: int):
        sig = self._peek()
        if sig is None or sig[0] < indent:
            return None
        return self._sequence(sig[0]) if sig[1].startswith("- ") else self._mapping(sig[0])

    def _mapping(self, indent: int) -> dict:
        out: dict = {}
        while True:
            sig = self._peek()
            if sig is None or sig[0] != indent or sig[1].startswith("- "):
                break
            self.pos += 1
            key, sep, value = sig[1].partition(":")
            if not sep:
                continue
            out[_scalar(key)] = self._value(indent, value.strip())
        return out

    def _sequence(self, indent: int) -> list:
        out: list = []
        while True:
            sig = self._peek()
            if sig is None or sig[0] != indent or not sig[1].startswith("- "):
                break
            self.pos += 1
            rest = sig[1][2:].strip()
            if not rest:
                out.append(self._block(indent + 1))
            elif ":" in rest:
                # A mapping whose first key sits on the dash line.
                item: dict = {}
                content_indent = indent + 2
                key, value = rest.split(":", 1)
                item[_scalar(key)] = self._value(content_indent, value.strip())
                while True:
                    nxt = self._peek()
                    if nxt is None or nxt[0] != content_indent or nxt[1].startswith("- "):
                        break
                    k2, sep, v2 = nxt[1].partition(":")
                    if not sep:
                        break
                    self.pos += 1
                    item[_scalar(k2)] = self._value(content_indent, v2.strip())
                out.append(item)
            else:
                out.append(_scalar(rest))
        return out

    def _value(self, parent_indent: int, value: str):
        if value in BLOCK_INDICATORS:
            return self._block_scalar(parent_indent, value)
        if value == "":
            return self._block(parent_indent + 1)
        return _scalar(value)

    def _block_scalar(self, parent_indent: int, indicator: str) -> str:
        lines: list[str] = []
        while self.pos < len(self.raw):
            line = self.raw[self.pos]
            if line.strip() == "":
                lines.append("")
                self.pos += 1
                continue
            if (len(line) - len(line.lstrip(" "))) <= parent_indent:
                break
            lines.append(line.strip())
            self.pos += 1
        while lines and lines[-1] == "":
            lines.pop()
        text = "\n".join(lines)
        if indicator.startswith(">"):
            text = re.sub(r"(?<!\n)\n(?!\n)", " ", text)
        if indicator in ("|", ">"):
            text += "\n"
        return text


def parse_yaml(text: str) -> dict:
    result = _YamlParser(text).parse()
    return result if isinstance(result, dict) else {}


def _dig(obj, *keys):
    for key in keys:
        if not isinstance(obj, dict):
            return None
        obj = obj.get(key)
    return obj


# --------------------------------------------------------------------------- #
# Cargo manifest reader
# --------------------------------------------------------------------------- #
def parse_cargo_manifest(text: str) -> tuple[str | None, list[str]]:
    name: str | None = None
    members: list[str] = []
    section: str | None = None
    in_members = False
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip()
            in_members = False
            continue
        if section == "package" and "=" in line:
            key, value = line.split("=", 1)
            if key.strip() == "name":
                name = _scalar(value)
        if section == "workspace":
            if in_members:
                members.extend(_quoted_strings(line))
                if "]" in line:
                    in_members = False
                continue
            if line.startswith("members") and "=" in line:
                rhs = line.split("=", 1)[1].strip()
                if "[" in rhs:
                    members.extend(_quoted_strings(rhs[rhs.index("[") + 1 :]))
                    in_members = "]" not in rhs
    return name, members


def resolve_package_names(
    root_name: str | None,
    members: list[str],
    base_dir: Path,
    overrides: dict[str, str] | None = None,
) -> set[str]:
    packages: set[str] = set()
    if root_name:
        packages.add(root_name)
    overrides = overrides or {}
    for member in members:
        if member in overrides:
            packages.add(overrides[member])
            continue
        manifest = base_dir / member / "Cargo.toml"
        try:
            detected, _ = parse_cargo_manifest(manifest.read_text(encoding="utf-8"))
        except OSError:
            detected = None
        # A path basename is a fallback only; real package names win (fuzz/ ->
        # memnoth-fuzz, for instance).
        packages.add(detected or member.rstrip("/").split("/")[-1])
    return packages


# --------------------------------------------------------------------------- #
# Archive scope model
# --------------------------------------------------------------------------- #
@dataclass
class Scope:
    name: str
    os: str
    archive_args: str
    explicit: set[str] = field(default_factory=set)
    excluded: set[str] = field(default_factory=set)
    workspace: bool = False
    kinds: set[str] = field(default_factory=set)
    test_targets: set[str] = field(default_factory=set)
    unknown: list[str] = field(default_factory=list)

    def covered_packages(self, authoritative: set[str]) -> set[str]:
        if self.workspace:
            return set(authoritative) - self.excluded
        return set(self.explicit) - self.excluded

    @property
    def is_whole_package(self) -> bool:
        return set(self.kinds) == set(ALL_KINDS)


def parse_scope(entry: dict) -> Scope:
    args = str(entry.get("archive_args", ""))
    scope = Scope(str(entry.get("scope", "")), str(entry.get("os", "")), args)
    try:
        tokens = shlex.split(args)
    except ValueError:
        tokens = args.split()
    target_selector_seen = False
    i = 0
    while i < len(tokens):
        token = tokens[i]
        if token in ("-p", "--package"):
            if i + 1 < len(tokens):
                scope.explicit.add(tokens[i + 1])
                i += 2
                continue
        elif token == "--exclude":
            if i + 1 < len(tokens):
                scope.excluded.add(tokens[i + 1])
                i += 2
                continue
        elif token == "--workspace":
            scope.workspace = True
        elif token == "--lib":
            scope.kinds.add("lib")
            target_selector_seen = True
        elif token in ("--bin", "--bins"):
            scope.kinds.add("bins")
            target_selector_seen = True
            if token == "--bin" and i + 1 < len(tokens):
                i += 1
        elif token == "--test":
            scope.kinds.add("tests")
            target_selector_seen = True
            if i + 1 < len(tokens):
                scope.test_targets.add(tokens[i + 1])
                i += 1
        elif token in ("--example", "--examples"):
            scope.kinds.add("examples")
            target_selector_seen = True
            if token == "--example" and i + 1 < len(tokens):
                i += 1
        elif token in ("--bench", "--benches"):
            scope.kinds.add("benches")
            target_selector_seen = True
            if token == "--bench" and i + 1 < len(tokens):
                i += 1
        elif token == "--all-targets":
            scope.kinds.update(ALL_KINDS)
            target_selector_seen = True
        elif token in ("--locked", "--all-features", "--no-default-features"):
            pass
        elif token in ("--archive-file", "--target-dir"):
            if i + 1 < len(tokens):
                i += 1
        elif token.startswith("--"):
            scope.unknown.append(token)
        i += 1
    if not target_selector_seen:
        # No target selector means Cargo selects every target of the chosen
        # packages, including all integration tests.
        scope.kinds = set(ALL_KINDS)
    return scope


# --------------------------------------------------------------------------- #
# Analysis
# --------------------------------------------------------------------------- #
@dataclass
class Check:
    name: str
    rows: list[dict]

    @property
    def ok(self) -> bool:
        return all(row["status"] != "FAIL" for row in self.rows)


@dataclass
class Report:
    checks: list[Check]
    totals: dict

    @property
    def ok(self) -> bool:
        return all(check.ok for check in self.checks)


def row(key: str, status: str, detail: str) -> dict:
    return {"key": key, "status": status, "detail": detail}


def analyze(
    cargo_text: str,
    ci_text: str,
    test_names: list[str],
    *,
    base_dir: Path | None = None,
    member_overrides: dict[str, str] | None = None,
) -> Report:
    root_name, members = parse_cargo_manifest(cargo_text)
    authoritative = resolve_package_names(
        root_name, members, base_dir or Path("."), member_overrides
    )

    data = parse_yaml(ci_text)
    jobs = data.get("jobs", {})
    archive_entries = _dig(jobs, "test-archive", "strategy", "matrix", "include") or []
    test_entries = _dig(jobs, "test", "strategy", "matrix", "include") or []
    scopes = [parse_scope(e) for e in archive_entries if isinstance(e, dict)]
    windows = [s for s in scopes if WINDOWS_MARKER in s.os.lower()]

    # C1 -- authoritative member set.
    c1_rows = []
    if not root_name:
        c1_rows.append(row("Cargo.toml", "FAIL", "no [package] name found"))
    else:
        c1_rows.append(row(root_name, "PASS", "root package"))
        for member in sorted(members):
            c1_rows.append(row(member, "PASS", "workspace member"))
    checks = [Check("C1 authoritative workspace member set", c1_rows)]

    # C2 -- CI matrix parse.
    c2_rows = []
    if not archive_entries:
        c2_rows.append(
            row("test-archive matrix", "FAIL", "no jobs.test-archive.strategy.matrix.include")
        )
    for entry in archive_entries:
        key = f"{entry.get('os', '?')}/{entry.get('scope', '?')}" if isinstance(entry, dict) else "?"
        if not isinstance(entry, dict) or not entry.get("scope") or not entry.get("archive_args"):
            c2_rows.append(row(key, "FAIL", "include entry missing scope or archive_args"))
        else:
            c2_rows.append(row(key, "PASS", str(entry["archive_args"]).replace("\n", " ")))
    checks.append(Check("C2 CI test-archive matrix parsed", c2_rows))

    # C3 -- each authoritative member covered exactly once by a Windows scope.
    cover = {pkg: {kind: [] for kind in ALL_KINDS} for pkg in authoritative}
    wholesale_tests = {pkg: [] for pkg in authoritative}
    extra_packages: dict[str, list[str]] = {}
    for scope in windows:
        for pkg in scope.covered_packages(authoritative):
            if pkg not in cover:
                extra_packages.setdefault(pkg, []).append(scope.name)
                continue
            for kind in scope.kinds:
                cover[pkg][kind].append(scope.name)
            # A scope with --test targets covers only those named tests; C4
            # checks them individually. A scope covering "all tests" (no
            # target selector, or --all-targets) may not be doubled up.
            if "tests" in scope.kinds and not scope.test_targets:
                wholesale_tests[pkg].append(scope.name)
    c3_rows = []
    for pkg in sorted(authoritative):
        by_kind = cover[pkg]
        covering = sorted({name for names in by_kind.values() for name in names})
        if not covering:
            c3_rows.append(row(pkg, "FAIL", "OMITTED from every Windows archive scope"))
            continue
        duplicates = [
            f"{kind}={by_kind[kind]}"
            for kind in ALL_KINDS
            if kind != "tests" and len(by_kind[kind]) > 1
        ]
        whole = wholesale_tests[pkg]
        explicit = [name for name in by_kind["tests"] if name not in whole]
        if len(whole) > 1 or (whole and explicit):
            duplicates.append(f"tests=whole:{whole}+explicit:{explicit}")
        if duplicates:
            c3_rows.append(
                row(pkg, "FAIL", "duplicate: covered by more than one scope for " + ", ".join(duplicates))
            )
            continue
        detail = " ".join(
            f"{kind}:{','.join(by_kind[kind])}" for kind in ALL_KINDS if by_kind[kind]
        )
        c3_rows.append(row(pkg, "PASS", detail))
    for pkg in sorted(extra_packages):
        c3_rows.append(
            row(pkg, "WARN", f"selected by {extra_packages[pkg]} but not in the authoritative member set")
        )
    checks.append(Check("C3 every member covered exactly once by a Windows scope", c3_rows))

    # C4 -- every integration test covered exactly once.
    tests_by_scope: dict[str, list[str]] = {}
    for scope in windows:
        for test in scope.test_targets:
            tests_by_scope.setdefault(test, []).append(scope.name)
    implicit_scopes = sorted(
        s.name
        for s in windows
        if root_name in s.covered_packages(authoritative) and s.is_whole_package
    )
    c4_rows = []
    for test in test_names:
        explicit = tests_by_scope.get(test, [])
        covering = sorted(set(explicit) | set(implicit_scopes))
        if len(covering) == 1:
            case = "explicit --test" if explicit else "whole-package scope"
            c4_rows.append(row(test, "PASS", f"covered exactly once via {case}: {covering[0]}"))
        elif not covering:
            c4_rows.append(
                row(test, "FAIL", "not covered by any Windows scope (expected via --test or whole-package scope)")
            )
        else:
            c4_rows.append(row(test, "FAIL", f"covered by more than one scope: {covering}"))
    for test in sorted(tests_by_scope):
        if test not in test_names:
            c4_rows.append(
                row("--test " + test, "WARN", "no matching tests/<name>.rs here (may belong to another crate)")
            )
    checks.append(Check("C4 every integration test covered exactly once", c4_rows))

    # C5 -- unique scopes and a matching execution entry for every archive.
    c5_rows: list[dict] = []
    seen: dict[tuple[str, str], str] = {}
    for scope in scopes:
        key = (scope.os, scope.name)
        if key in seen:
            c5_rows.append(
                row(f"{scope.os}/{scope.name}", "FAIL", f"duplicate (os, scope); first seen as {seen[key]}")
            )
        else:
            seen[key] = scope.name
    windows_names = [s.name for s in windows]
    for name in sorted({n for n in windows_names if windows_names.count(n) > 1}):
        c5_rows.append(row("windows/" + name, "FAIL", "duplicate Windows scope name"))

    # Two supported execution layouts: one shard fans out over an archives=(...)
    # list, or one dedicated test-job matrix row names each Windows scope.
    test_scope_entries: dict[str, list[str]] = {}
    for entry in test_entries:
        if isinstance(entry, dict) and entry.get("scope"):
            test_scope_entries.setdefault(str(entry["scope"]), []).append(str(entry.get("os", "")))
    match = re.search(r"archives=\(([^)]*)\)", ci_text)
    archive_tokens = (
        set(re.findall(r"nextest-[^\s\]]+\.tar\.zst", match.group(1))) if match else set()
    )
    per_scope = bool(test_scope_entries)
    fanout = match is not None
    if not per_scope and not fanout:
        c5_rows.append(
            row("execution structure", "WARN", "cannot locate per-scope test entries or archives=(...); structure unproven")
        )
    if per_scope and not re.search(
        r"nextest-windows-\$\{\{\s*matrix\.scope\s*\}\}\.tar\.zst", ci_text
    ):
        c5_rows.append(
            row("archive filename template", "WARN", "cannot prove nextest-windows-<scope>.tar.zst from the test job run step")
        )

    for scope in scopes:
        if per_scope:
            if WINDOWS_MARKER not in scope.os.lower():
                os_hits = sum(
                    1
                    for entry in test_entries
                    if isinstance(entry, dict)
                    and str(entry.get("os", "")).lower() == scope.os.lower()
                    and not entry.get("scope")
                )
                if os_hits:
                    c5_rows.append(
                        row(f"nextest-{scope.name}.tar.zst [{scope.os}]", "PASS", f"{os_hits} non-Windows shard entries")
                    )
                else:
                    c5_rows.append(row(scope.os, "FAIL", "no execution entry in the test job matrix"))
                continue
            hits = test_scope_entries.get(scope.name, [])
            matching = [os_name for os_name in hits if WINDOWS_MARKER in os_name.lower()]
            if len(matching) == 1:
                c5_rows.append(
                    row(f"nextest-windows-{scope.name}.tar.zst", "PASS", f"per-scope execution entry for {scope.os}/{scope.name}")
                )
            elif not matching:
                c5_rows.append(row(scope.name, "FAIL", "no execution entry in the test job matrix"))
            else:
                c5_rows.append(row(scope.name, "FAIL", f"duplicate execution entries: {hits}"))
        elif fanout:
            filename = f"nextest-{scope.name}.tar.zst"
            if filename in archive_tokens:
                c5_rows.append(
                    row(filename, "PASS", f"fan-out execution entry for {scope.os}/{scope.name}")
                )
            else:
                c5_rows.append(
                    row(filename, "FAIL", f"no execution entry in archives=(...) for {scope.os}/{scope.name}")
                )
        else:
            c5_rows.append(row(scope.name, "WARN", "cannot prove an execution entry for this scope"))

    if not test_entries:
        c5_rows.append(row("test job matrix", "WARN", "cannot locate jobs.test.strategy.matrix.include"))
    else:
        c5_rows.append(row("test job matrix", "PASS", f"{len(test_entries)} execution entries"))
    if fanout and not per_scope:
        expected_archives = {f"nextest-{s.name}.tar.zst" for s in scopes}
        for extra in sorted(archive_tokens - expected_archives):
            c5_rows.append(row(extra, "WARN", "archive execution entry with no matching test-archive scope"))
    checks.append(Check("C5 unique scopes and matching execution entries", c5_rows))

    totals = {
        "members": len(authoritative),
        "integration_tests": len(test_names),
        "archive_scopes": len(scopes),
        "windows_scopes": len(windows),
        "test_job_entries": len(test_entries),
    }
    checks.append(
        Check(
            "C6 totals",
            [
                row("workspace members", "PASS", str(totals["members"])),
                row("integration tests", "PASS", str(totals["integration_tests"])),
                row("archive scopes", "PASS", str(totals["archive_scopes"])),
                row("windows archive scopes", "PASS", str(totals["windows_scopes"])),
                row("test-job entries", "PASS", str(totals["test_job_entries"])),
            ],
        )
    )
    return Report(checks, totals)


# --------------------------------------------------------------------------- #
# Reporting
# --------------------------------------------------------------------------- #
def print_report(report: Report, ci_label: str, cargo_label: str, tests_label: str) -> None:
    print(f"CI coverage check: {ci_label}")
    print(f"  manifest: {cargo_label}")
    print(f"  tests:    {tests_label}")
    for check in report.checks:
        print()
        print(f"[{ 'PASS' if check.ok else 'FAIL' }] {check.name}")
        for item in check.rows:
            print(f"    {item['status']:<4}  {item['key']:<38}  {item['detail']}")
    t = report.totals
    print()
    print(
        "Totals: members={members}, integration_tests={integration_tests}, "
        "archive_scopes={archive_scopes} (windows={windows_scopes}), "
        "test_job_entries={test_job_entries}".format(**t)
    )
    print()
    print("RESULT: " + ("PASS" if report.ok else "FAIL"))


# --------------------------------------------------------------------------- #
# Self test
# --------------------------------------------------------------------------- #
def run_self_test() -> int:
    cargo = (
        "[package]\n"
        "name = \"demo\"\n"
        "\n"
        "[workspace]\n"
        "members = [\n"
        "    \"crates/a\",\n"
        "]\n"
    )
    overrides = {"crates/a": "demo-a"}
    ci = (
        "jobs:\n"
        "  test-archive:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n"
        "          - os: windows-latest\n"
        "            scope: main\n"
        "            archive_args: -p demo --lib\n"
        "            doctests: false\n"
        "          - os: windows-latest\n"
        "            scope: extra\n"
        "            archive_args: -p demo --bins --test one --test two\n"
        "            doctests: false\n"
        "          - os: windows-latest\n"
        "            scope: rest\n"
        "            archive_args: --workspace --exclude demo\n"
        "            doctests: false\n"
        "  test:\n"
        "    strategy:\n"
        "      matrix:\n"
        "        include:\n"
        "          - os: windows-latest\n"
        "            shard: 1\n"
        "            shards: 1\n"
        "    steps:\n"
        "      - run: |\n"
        "          archives=(nextest-main.tar.zst nextest-extra.tar.zst nextest-rest.tar.zst)\n"
    )
    tests = ["one", "two"]

    def failures(report: Report) -> list[tuple[str, dict]]:
        return [
            (check.name, item)
            for check in report.checks
            for item in check.rows
            if item["status"] == "FAIL"
        ]

    cases: list[tuple[str, bool, str]] = []

    positive = analyze(cargo, ci, tests, member_overrides=overrides)
    cases.append(("positive control passes", not failures(positive), "expected zero FAIL rows"))

    # (a) a package present in Cargo.toml but in no Windows scope.
    cargo_missing = cargo.replace('    "crates/a",\n', '    "crates/a",\n    "crates/ghost",\n')
    overrides_missing = dict(overrides)
    overrides_missing["crates/ghost"] = "demo-ghost"
    ci_missing = ci.replace(
        "archive_args: --workspace --exclude demo\n",
        "archive_args: --workspace --exclude demo --exclude demo-ghost\n",
    )
    missing = analyze(cargo_missing, ci_missing, tests, member_overrides=overrides_missing)
    ok = any(
        item["key"] == "demo-ghost" and "OMITTED" in item["detail"]
        for _, item in failures(missing)
    )
    cases.append(("(a) missing package is detected", ok, "expected demo-ghost OMITTED"))

    # (b) a package selected by two whole-package Windows scopes.
    ci_duplicate = ci.replace(
        "            archive_args: --workspace --exclude demo\n            doctests: false\n",
        "            archive_args: --workspace --exclude demo\n            doctests: false\n"
        "          - os: windows-latest\n"
        "            scope: dup\n"
        "            archive_args: -p demo-a\n"
        "            doctests: false\n",
    ).replace(
        "nextest-rest.tar.zst)",
        "nextest-rest.tar.zst nextest-dup.tar.zst)",
    )
    duplicate = analyze(cargo, ci_duplicate, tests, member_overrides=overrides)
    ok = any(
        item["key"] == "demo-a" and "duplicate" in item["detail"]
        for _, item in failures(duplicate)
    )
    cases.append(("(b) duplicate package is detected", ok, "expected demo-a duplicate"))

    # (c) an integration test file with no --test target and no whole-package scope.
    missing_test = analyze(cargo, ci, tests + ["three"], member_overrides=overrides)
    ok = any(
        item["key"] == "three" and "not covered" in item["detail"]
        for _, item in failures(missing_test)
    )
    cases.append(("(c) missing integration test is detected", ok, "expected three not covered"))

    print("SELF TEST (inline sample data)")
    for label, passed, expectation in cases:
        print(f"  [{'PASS' if passed else 'FAIL'}] {label:<40} {expectation}")
    result = all(passed for _, passed, _ in cases)
    print("SELF TEST RESULT: " + ("PASS" if result else "FAIL"))
    return 0 if result else 1


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #
def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ci", help="path to ci.yml (default: <repo>/.github/workflows/ci.yml)")
    parser.add_argument("--cargo", help="path to Cargo.toml (default: <repo>/Cargo.toml)")
    parser.add_argument("--tests", help="integration test directory (default: <repo>/tests)")
    parser.add_argument("--self-test", action="store_true", help="run inline negative/positive cases")
    args = parser.parse_args(argv)

    if args.self_test:
        return run_self_test()

    root = Path(__file__).resolve().parent.parent
    ci_path = Path(args.ci) if args.ci else root / ".github" / "workflows" / "ci.yml"
    cargo_path = Path(args.cargo) if args.cargo else root / "Cargo.toml"
    tests_dir = Path(args.tests) if args.tests else root / "tests"

    for label, path in (("ci", ci_path), ("cargo", cargo_path)):
        if not path.is_file():
            print(f"ERROR: {label} file not found: {path}", file=sys.stderr)
            return 1
    if not tests_dir.is_dir():
        print(f"ERROR: tests directory not found: {tests_dir}", file=sys.stderr)
        return 1

    test_names = sorted(path.stem for path in tests_dir.glob("*.rs"))
    report = analyze(
        cargo_path.read_text(encoding="utf-8"),
        ci_path.read_text(encoding="utf-8"),
        test_names,
        base_dir=root,
    )
    print_report(report, str(ci_path), str(cargo_path), str(tests_dir))
    return 0 if report.ok else 1


if __name__ == "__main__":
    sys.exit(main())
