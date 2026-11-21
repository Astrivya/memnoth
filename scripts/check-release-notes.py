#!/usr/bin/env python3
"""Gate against release-note and version-surface drift.

A release can ship with a correct binary and a wrong story: the changelog stops
at an older version, the site's copy of the changelog diverges, the generated
release-note config loses its categories, or the docs that explain the release
go missing. This check makes those drifts fail CI instead of being noticed on
the download page after the tag is public.

Read-only by design. Nothing here rewrites a file it is checking -- a gate that
fixes its own input can never fail.

Usage:
    python3 scripts/check-release-notes.py [--cargo PATH] [--changelog PATH]
                                           [--site-copy PATH]
                                           [--web-contract DIR]
                                           [--web-root DIR] [--root DIR]
                                           [--strict] [--self-test]

Exit: 0 when no check FAILs (and, with --strict, no WARN either), else 1.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

# This file lives in <cli-root>/scripts/, so its parent's parent is the CLI
# repository root whether the script is run from there, from CI, or by hand.
CLI_ROOT = Path(__file__).resolve().parent.parent

PASS = "PASS"
FAIL = "FAIL"
WARN = "WARN"

# The release-note template contract: these headings, in this order.
REQUIRED_HEADINGS = (
    "Highlights",
    "What's new",
    "What's fixed",
    "Breaking changes",
    "Known issues",
    "Upgrade notes",
    "What's next",
    "How to verify",
)

# Docs that live in the workspace root's docs/ tree, outside the CLI git repo.
# They are supporting material, not release blockers, so they WARN when absent.
WORKSPACE_DOCS = (
    Path("docs") / "operations" / "release.md",
    Path("docs") / "operations" / "known-issues.md",
    Path("docs") / "operations" / "whats-next.md",
)

# `## [1.2.3]`, `## [1.2.3] - 2026-11-21`, `## [Unreleased]`.
HEADING_RE = re.compile(r"^##\s+\[([^\]]+)\](?:\s*[-–—]\s*(.*?))?\s*$")


@dataclass(frozen=True)
class Result:
    key: str
    name: str
    status: str
    detail: str


@dataclass
class Config:
    """Every path the checks read, resolved once so main and self-test agree."""

    cli_root: Path
    cargo: Path
    changelog: Path
    web_root: Path
    web_contract: Path  # directory holding contract/changelog.md
    site_copy: Path  # web/content/docs/changelog.mdx
    root: Path  # workspace root, for docs/operations/*
    release_cfg: Path
    template: Path

    @classmethod
    def defaults(
        cls,
        cli_root: Path = CLI_ROOT,
        root: Path | None = None,
        web_root: Path | None = None,
    ) -> "Config":
        root = root if root is not None else cli_root.parent
        web_root = web_root if web_root is not None else cli_root.parent / "web"
        return cls(
            cli_root=cli_root,
            cargo=cli_root / "Cargo.toml",
            changelog=cli_root / "CHANGELOG.md",
            web_root=web_root,
            web_contract=web_root / "contract",
            site_copy=web_root / "content" / "docs" / "changelog.mdx",
            root=root,
            release_cfg=cli_root / ".github" / "release.yml",
            template=cli_root / ".github" / "RELEASE_NOTES_TEMPLATE.md",
        )


def short(path: Path, base: Path) -> str:
    """Render a path relative to a known base when possible."""
    try:
        return str(path.relative_to(base))
    except ValueError:
        return str(path)


def read_cargo_version(path: Path) -> str | None:
    """Read `version = "..."` from the first `[package]` table in Cargo.toml."""
    if not path.is_file():
        return None
    in_package = False
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    return None


def changelog_headings(text: str) -> list[tuple[str, str]]:
    """Return (label, date) for every `## [...] changelog heading, in order."""
    headings: list[tuple[str, str]] = []
    for line in text.splitlines():
        match = HEADING_RE.match(line.strip())
        if match:
            headings.append((match.group(1).strip(), (match.group(2) or "").strip()))
    return headings


def first_diff(left: bytes, right: bytes) -> int | None:
    """Byte offset of the first difference, or None when the blobs are equal."""
    for index, (a, b) in enumerate(zip(left, right)):
        if a != b:
            return index
    if len(left) != len(right):
        return min(len(left), len(right))
    return None


def release_has_categories(text: str) -> bool:
    """True when a `changelog:` block contains a nested `categories:` key."""
    in_changelog = False
    base_indent = 0
    for line in text.splitlines():
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        indent = len(line) - len(line.lstrip())
        if re.match(r"^\s*changelog\s*:\s*$", line):
            in_changelog = True
            base_indent = indent
            continue
        if in_changelog:
            if indent <= base_indent:
                in_changelog = False
                continue
            if re.match(r"^\s*categories\s*:", line):
                return True
    return False


def template_headings(text: str) -> tuple[bool, str]:
    """Check that all eight required headings appear, in order, as headings."""
    positions: list[int] = []
    for heading in REQUIRED_HEADINGS:
        pattern = re.compile(
            r"^#{1,6}\s*" + re.escape(heading) + r"\s*$",
            re.IGNORECASE | re.MULTILINE,
        )
        match = pattern.search(text)
        if match is None:
            return False, f"missing heading '{heading}'"
        positions.append(match.start())
    if positions != sorted(positions):
        return False, "headings are out of order"
    return True, f"all {len(REQUIRED_HEADINGS)} headings in order"


def run_checks(cfg: Config) -> list[Result]:
    """Run every check against a resolved Config. Shared with the self-test."""
    results: list[Result] = []

    version = read_cargo_version(cfg.cargo)
    changelog_ok = cfg.changelog.is_file()
    changelog_text = (
        cfg.changelog.read_text(encoding="utf-8", errors="replace")
        if changelog_ok
        else ""
    )
    changelog_bytes = cfg.changelog.read_bytes() if changelog_ok else b""
    headings = changelog_headings(changelog_text) if changelog_ok else []
    labels = [label for label, _ in headings]
    version_labels = [label for label in labels if label.lower() != "unreleased"]
    has_unreleased = any(label.lower() == "unreleased" for label in labels)
    newest = version_labels[0] if version_labels else None

    # 1. Cargo version is readable.
    if version is None:
        results.append(
            Result(
                "cargo_version",
                "Cargo version parsed",
                FAIL,
                f"no [package] version in {short(cfg.cargo, cfg.cli_root)}",
            )
        )
    else:
        results.append(
            Result("cargo_version", "Cargo version parsed", PASS, f"version = {version}")
        )

    # 2. A matching changelog heading exists for that exact version.
    if version is None:
        detail = "cannot check without a Cargo version"
        results.append(Result("changelog_heading", "Changelog heading for version", FAIL, detail))
    elif not changelog_ok:
        detail = f"{short(cfg.changelog, cfg.cli_root)} is missing"
        results.append(Result("changelog_heading", "Changelog heading for version", FAIL, detail))
    elif version not in version_labels:
        detail = f"no '## [{version}]' heading in {short(cfg.changelog, cfg.cli_root)}"
        results.append(Result("changelog_heading", "Changelog heading for version", FAIL, detail))
    else:
        detail = f"'## [{version}]' present"
        results.append(Result("changelog_heading", "Changelog heading for version", PASS, detail))

    # 3. Unreleased section exists and the newest heading is not stale: the
    #    Cargo version must have its own heading.
    if not has_unreleased:
        detail = "missing '## [Unreleased]' heading"
        results.append(Result("unreleased", "Unreleased / newest heading", FAIL, detail))
    elif version is not None and version not in version_labels:
        newest_text = f"'{newest}'" if newest else "none"
        detail = f"stale: newest is {newest_text}, but Cargo v{version} has no heading"
        results.append(Result("unreleased", "Unreleased / newest heading", FAIL, detail))
    else:
        newest_text = f"'{newest}'" if newest else "none"
        detail = f"Unreleased present; newest = {newest_text}"
        results.append(Result("unreleased", "Unreleased / newest heading", PASS, detail))

    # 4. The documented generated copy in the web checkout is byte-identical.
    #    A CLI-only checkout (CI) has no sibling web repo; WARN with the reason
    #    instead of failing a gate on a checkout that never had the file. The
    #    web repo's own check:docs gate verifies the copy where it exists.
    contract_file = cfg.web_contract / "changelog.md"
    if not cfg.web_root.is_dir():
        detail = f"web checkout absent at {cfg.web_root}; contract copy not checked"
        results.append(Result("contract_copy", "Contract changelog copy", WARN, detail))
    elif not contract_file.is_file():
        detail = f"missing {short(contract_file, cfg.web_root)} (generated copy of the changelog)"
        results.append(Result("contract_copy", "Contract changelog copy", FAIL, detail))
    elif not changelog_ok:
        detail = f"{short(cfg.changelog, cfg.cli_root)} missing; cannot compare"
        results.append(Result("contract_copy", "Contract changelog copy", FAIL, detail))
    else:
        contract_bytes = contract_file.read_bytes()
        offset = first_diff(changelog_bytes, contract_bytes)
        if offset is None:
            detail = f"identical to {short(contract_file, cfg.web_root)} ({len(contract_bytes)} bytes)"
            results.append(Result("contract_copy", "Contract changelog copy", PASS, detail))
        else:
            detail = (
                f"divergent at byte {offset} "
                f"({len(changelog_bytes)} CLI vs {len(contract_bytes)} contract bytes)"
            )
            results.append(Result("contract_copy", "Contract changelog copy", FAIL, detail))

    # 5. The site generator's committed output mentions the current version.
    #    A CLI-only checkout (CI, a fresh clone) has no sibling web repo: WARN
    #    and say why rather than failing a gate on a checkout that never had it.
    if not cfg.web_root.is_dir():
        detail = f"web checkout absent at {cfg.web_root}; site copy not checked"
        results.append(Result("site_copy", "Site changelog copy", WARN, detail))
    elif not cfg.site_copy.is_file():
        detail = f"missing {short(cfg.site_copy, cfg.web_root)}"
        results.append(Result("site_copy", "Site changelog copy", FAIL, detail))
    elif version is None:
        detail = "cannot check without a Cargo version"
        results.append(Result("site_copy", "Site changelog copy", FAIL, detail))
    else:
        site_text = cfg.site_copy.read_text(encoding="utf-8", errors="replace")
        if version in site_text:
            detail = f"{short(cfg.site_copy, cfg.web_root)} mentions v{version}"
            results.append(Result("site_copy", "Site changelog copy", PASS, detail))
        else:
            detail = f"{short(cfg.site_copy, cfg.web_root)} does not mention v{version}"
            results.append(Result("site_copy", "Site changelog copy", FAIL, detail))

    # 6. Release configuration and the release-note template contract.
    problems: list[str] = []
    if not cfg.release_cfg.is_file():
        problems.append(f"missing {short(cfg.release_cfg, cfg.cli_root)}")
    else:
        release_text = cfg.release_cfg.read_text(encoding="utf-8", errors="replace")
        if not release_has_categories(release_text):
            problems.append(f"{short(cfg.release_cfg, cfg.cli_root)} has no changelog.categories")
    if not cfg.template.is_file():
        problems.append(f"missing {short(cfg.template, cfg.cli_root)}")
    else:
        template_text = cfg.template.read_text(encoding="utf-8", errors="replace")
        ok, why = template_headings(template_text)
        if not ok:
            problems.append(f"{short(cfg.template, cfg.cli_root)}: {why}")
    if problems:
        results.append(
            Result("release_config", "Release config + template", FAIL, "; ".join(problems))
        )
    else:
        results.append(
            Result(
                "release_config",
                "Release config + template",
                PASS,
                "changelog.categories present; 8 template headings in order",
            )
        )

    # 7. Workspace release docs. These live outside the CLI git repo and are
    #    supporting material, so absence is a WARN, not a release blocker.
    doc_paths = [cfg.root / rel for rel in WORKSPACE_DOCS]
    missing_docs = [path for path in doc_paths if not path.is_file()]
    if missing_docs:
        names = ", ".join(short(path, cfg.root) for path in missing_docs)
        results.append(
            Result(
                "workspace_docs",
                "Workspace release docs",
                WARN,
                f"missing {names} (docs, not release blockers)",
            )
        )
    else:
        results.append(
            Result("workspace_docs", "Workspace release docs", PASS, "release/known-issues/whats-next present")
        )

    return results


def print_table(cfg: Config, results: list[Result]) -> None:
    """Print the PASS/FAIL table and the resolved paths it was run against."""
    print("Release notes consistency")
    print(f"  CLI root:   {cfg.cli_root}")
    print(f"  cargo:      {cfg.cargo}")
    print(f"  changelog:  {cfg.changelog}")
    print(f"  web root:   {cfg.web_root}")
    print(f"  workspace:  {cfg.root}")
    print()
    width = max(len(result.name) for result in results)
    for number, result in enumerate(results, 1):
        print(
            f"  {number}. {result.status:<4}  "
            f"{result.name:<{width}}  {result.detail}"
        )


# --- Self-test fixtures ---------------------------------------------------

FIXTURE_CARGO = '[package]\nname = "demo"\nversion = "1.2.3"\n'
FIXTURE_CHANGELOG = """# Changelog

## [Unreleased]

## [1.2.3] - 2026-11-21

### Added

- A change that this release shipped.
"""
FIXTURE_SITE_COPY = """---
title: "Changelog"
---

## Unreleased

## 1.2.3 - 2026-11-21

- A change that this release shipped.
"""
FIXTURE_RELEASE_GOOD = """name: release
changelog:
  categories:
    - title: Features
      labels: [enhancement]
"""
FIXTURE_RELEASE_BAD = """name: release
changelog:
  exclude:
    labels: [wontfix]
"""
FIXTURE_TEMPLATE = "\n".join(f"## {heading}" for heading in REQUIRED_HEADINGS) + "\n"


def make_fixture(
    base: Path,
    name: str,
    *,
    stale: bool = False,
    diverge: bool = False,
    no_categories: bool = False,
) -> Config:
    """Build a complete fake workspace; the flags inject one drift each."""
    root = base / name
    cli = root / "CLI"
    web = root / "web"
    (cli / ".github").mkdir(parents=True)
    (web / "contract").mkdir(parents=True)
    (web / "content" / "docs").mkdir(parents=True)
    (root / "docs" / "operations").mkdir(parents=True)

    (cli / "Cargo.toml").write_text(FIXTURE_CARGO, encoding="utf-8")
    changelog = FIXTURE_CHANGELOG.replace("[1.2.3]", "[9.9.9]") if stale else FIXTURE_CHANGELOG
    (cli / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    contract = "DIVERGED - not the changelog\n" if diverge else changelog
    (web / "contract" / "changelog.md").write_text(contract, encoding="utf-8")
    (web / "content" / "docs" / "changelog.mdx").write_text(FIXTURE_SITE_COPY, encoding="utf-8")
    release_cfg = FIXTURE_RELEASE_BAD if no_categories else FIXTURE_RELEASE_GOOD
    (cli / ".github" / "release.yml").write_text(release_cfg, encoding="utf-8")
    (cli / ".github" / "RELEASE_NOTES_TEMPLATE.md").write_text(FIXTURE_TEMPLATE, encoding="utf-8")
    for rel in WORKSPACE_DOCS:
        (root / rel).write_text("Supporting documentation.\n", encoding="utf-8")

    return Config(
        cli_root=cli,
        cargo=cli / "Cargo.toml",
        changelog=cli / "CHANGELOG.md",
        web_root=web,
        web_contract=web / "contract",
        site_copy=web / "content" / "docs" / "changelog.mdx",
        root=root,
        release_cfg=cli / ".github" / "release.yml",
        template=cli / ".github" / "RELEASE_NOTES_TEMPLATE.md",
    )


def self_test() -> int:
    """Prove the four drift cases with inline fixtures; exit 0 only if all pass."""
    cases: list[tuple[str, dict[str, bool], dict[str, str], str]] = [
        ("good fixture passes", {}, {}, "every check PASS"),
        ("stale changelog fails", {"stale": True}, {"changelog_heading": FAIL}, "wrong version heading FAILs"),
        ("divergent site copy fails", {"diverge": True}, {"contract_copy": FAIL}, "byte divergence FAILs"),
        ("missing categories fails", {"no_categories": True}, {"release_config": FAIL}, "no categories FAILs"),
    ]
    passed = 0
    with tempfile.TemporaryDirectory(prefix="check-release-notes-") as temp:
        base = Path(temp)
        for index, (label, flags, expected, note) in enumerate(cases):
            cfg = make_fixture(base, f"case-{index}", **flags)
            results = run_checks(cfg)
            by_key = {result.key: result.status for result in results}

            ok = True
            detail: list[str] = []
            if not flags:
                bad = {r.key: r.status for r in results if r.status != PASS}
                if bad:
                    ok = False
                    detail.append(f"expected all PASS, got {bad}")
            else:
                for key, want in expected.items():
                    got = by_key.get(key)
                    if got != want:
                        ok = False
                        detail.append(f"{key}: expected {want}, got {got}")

            status = PASS if ok else FAIL
            print(f"  {status}  {label}" + (f" -- {'; '.join(detail)}" if detail else f" -- {note}"))
            if ok:
                passed += 1

    total = len(cases)
    print(f"Self-test: {passed}/{total} cases passed")
    return 0 if passed == total else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Fail when release-note or version surfaces drift."
    )
    parser.add_argument("--cargo", type=Path, help="path to CLI/Cargo.toml")
    parser.add_argument("--changelog", type=Path, help="path to CLI/CHANGELOG.md")
    parser.add_argument("--site-copy", type=Path, help="path to web/content/docs/changelog.mdx")
    parser.add_argument(
        "--web-contract", type=Path, help="directory holding the contract changelog.md"
    )
    parser.add_argument(
        "--web-root", type=Path, help="sibling web checkout (default: ../web)"
    )
    parser.add_argument("--root", type=Path, help="workspace root for docs/ (default: CLI parent)")
    parser.add_argument("--strict", action="store_true", help="treat WARNs as failures")
    parser.add_argument(
        "--self-test", action="store_true", help="run inline fixtures and exit"
    )
    args = parser.parse_args(argv)

    if args.self_test:
        return self_test()

    cfg = Config.defaults(CLI_ROOT, root=args.root, web_root=args.web_root)
    if args.cargo is not None:
        cfg.cargo = args.cargo
    if args.changelog is not None:
        cfg.changelog = args.changelog
    if args.site_copy is not None:
        cfg.site_copy = args.site_copy
    if args.web_contract is not None:
        cfg.web_contract = args.web_contract

    results = run_checks(cfg)
    print_table(cfg, results)

    failures = [r for r in results if r.status == FAIL]
    warnings = [r for r in results if r.status == WARN]
    bad = bool(failures) or (args.strict and bool(warnings))
    verdict = FAIL if bad else PASS
    strict_note = " [strict]" if args.strict else ""
    print()
    print(
        f"  Verdict: {verdict}  "
        f"({len(failures)} fail, {len(warnings)} warn){strict_note}"
    )
    if failures:
        print("  Failed checks: " + ", ".join(r.name for r in failures))
    if warnings:
        print("  Warnings: " + ", ".join(r.name for r in warnings))
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
