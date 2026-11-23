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


# --- Claim-fence helpers -------------------------------------------------

# The shipping version is whichever version Cargo declares. Every claim below
# is scoped to that version's block: an honest "Not team memory" note in an
# older section is not a claim made by the shipping release.
CLAIM_VERSION_FALLBACK = "0.1.2"

# The forbidden phrases come from plans/positioning-2026-11.md §4 (claim fence):
# no exclusivity, no signed/attested authenticity, no team memory before its
# gates, no official-benchmark scores from derived diagnostics.
FORBIDDEN_CLAIM_PATTERNS: dict[str, re.Pattern[str]] = {
    "team memory": re.compile(r"\bteam\s+memory\b", re.IGNORECASE),
    "signed": re.compile(r"\bsigned\b", re.IGNORECASE),
    "attested": re.compile(r"\battested\b", re.IGNORECASE),
    "official benchmark": re.compile(r"\bofficial\s+benchmarks?\b", re.IGNORECASE),
    "nobody does this": re.compile(r"\bnobody\s+does\s+this\b", re.IGNORECASE),
    "verified by cloud": re.compile(r"\bverified\s+by\s+(?:the\s+)?cloud\b", re.IGNORECASE),
}

# Truthful, named attestations are allowed. The fence targets unqualified
# claims of authenticity ("signed by ...", "attested"), not the verifiable
# "minisign-signed release" / "signed with the repository's minisign key".
MINISIGN_SIGNED_RE = re.compile(r"minisign[- ]signed", re.IGNORECASE)
SIGNED_WITH_KEY_RE = re.compile(
    r"signed\s+with\s+(?:the\s+)?[^.;]{0,120}?\bminisign\b", re.IGNORECASE
)

NEGATION_RE = re.compile(
    r"\b(?:not|no|never|nor|without|unsigned|unverified|conditional|caveat|"
    r"diagnostic|dry[- ]run|does not|doesn't|isn't|aren't|cannot|can't|"
    r"remains?|still|partial|open)\b",
    re.IGNORECASE,
)
CLOSURE_VERB_RE = re.compile(
    r"\b(?:fixed|resolved|closed|addressed|completed|shipped|done|"
    r"no longer|now works?)\b",
    re.IGNORECASE,
)

KNOWN_ISSUE_HEADING_RE = re.compile(r"^##\s+\d+\.\s+(.*?)\s*$", re.IGNORECASE)
CLOSED_MARKER_RE = re.compile(
    r"(?:\*\*\s*status\s*\*\*\s*:\s*(?:closed|resolved|fixed)\b)"
    r"|(?:^\s*>\s*\*\*(?:closed|resolved|fixed)\b)",
    re.IGNORECASE | re.MULTILINE,
)

CLAIM_STOPWORDS = frozenset(
    """the a an and or of to in on for with that this is are was were be been being
    it its as at by from no not cannot will would shall should do does did have has had
    which who whom whose when where how why but if then than so such more most other
    another into out up down about above below under over again further once here there
    all any some each few own same only very can may might must""".split()
)


def read_cargo_versions(path: Path) -> tuple[str | None, str | None]:
    """Return (root [package] version, [workspace.package] version)."""
    if not path.is_file():
        return None, None
    package: str | None = None
    workspace: str | None = None
    current: str | None = None
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            current = stripped
            continue
        match = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
        if match:
            if current == "[package]" and package is None:
                package = match.group(1)
            elif current == "[workspace.package]" and workspace is None:
                workspace = match.group(1)
    return package, workspace


def crate_versions(cli_root: Path) -> list[tuple[str, str]]:
    """(crate name, version) for each explicit-version crate manifest."""
    crates = cli_root / "crates"
    if not crates.is_dir():
        return []
    found: list[tuple[str, str]] = []
    for manifest in sorted(crates.glob("*/Cargo.toml")):
        package, _ = read_cargo_versions(manifest)
        if package is not None:
            found.append((manifest.parent.name, package))
    return found


def version_block(text: str, version: str | None) -> str:
    """The `## [...]` section for one version, up to the next `## ` heading."""
    if not version or not text:
        return ""
    heading = re.compile(
        r"^##\s+\[?" + re.escape(version) + r"\]?(?:\s|$|[-–—])",
        re.IGNORECASE,
    )
    lines = text.splitlines()
    start: int | None = None
    for index, line in enumerate(lines):
        if heading.match(line.strip()):
            start = index + 1
            break
    if start is None:
        return ""
    end = len(lines)
    for index in range(start, len(lines)):
        if re.match(r"^##\s+", lines[index].strip()):
            end = index
            break
    return "\n".join(lines[start:end])


def flatten(text: str) -> str:
    return re.sub(r"\s+", " ", text).strip()


def forbidden_claims(block: str) -> list[str]:
    """Forbidden claim phrases present as affirmative claims in a block."""
    if not block:
        return []
    scrubbed = MINISIGN_SIGNED_RE.sub(" ", flatten(block))
    scrubbed = SIGNED_WITH_KEY_RE.sub(" ", scrubbed)
    hits: list[str] = []
    for label, pattern in FORBIDDEN_CLAIM_PATTERNS.items():
        for match in pattern.finditer(scrubbed):
            window = scrubbed[max(0, match.start() - 90) : match.end() + 90]
            if NEGATION_RE.search(window):
                continue
            hits.append(label)
            break
    return hits


def open_known_issues(text: str) -> list[dict[str, str]]:
    """Numbered known-issue entries that are not explicitly marked closed."""
    entries: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in text.splitlines():
        match = KNOWN_ISSUE_HEADING_RE.match(line.strip())
        if match:
            current = {"title": match.group(1).strip(), "body": ""}
            entries.append(current)
        elif current is not None:
            current["body"] += line + "\n"
    return [entry for entry in entries if not CLOSED_MARKER_RE.search(entry["body"])]


def _distinctive_tokens(title: str) -> list[str]:
    words = re.findall(r"[a-z0-9][a-z0-9._-]*", title.lower())
    return [word for word in words if len(word) >= 3 and word not in CLAIM_STOPWORDS]


def _closure_positions(flat: str) -> list[int]:
    positions: list[int] = []
    for match in CLOSURE_VERB_RE.finditer(flat):
        window = flat[max(0, match.start() - 120) : match.end() + 120]
        if NEGATION_RE.search(window):
            continue
        positions.append(match.start())
    return positions


def closure_claims(block: str, open_entries: list[dict[str, str]]) -> list[str]:
    """Closure claims about open known issues found in a release-notes block."""
    if not block or not open_entries:
        return []
    flat = flatten(block)
    lower = flat.lower()
    hits: list[str] = []

    if re.search(r"\bno\s+known\s+issues?\b", flat, re.IGNORECASE):
        hits.append("blanket 'no known issues' claim")

    positions = _closure_positions(flat)
    for position in positions:
        window = flat[max(0, position - 160) : position + 160]
        if re.search(r"\b(?:all|every)\s+known\s+issues?\b", window, re.IGNORECASE):
            hits.append("blanket 'all known issues resolved' claim")
            break
    for position in positions:
        window = flat[max(0, position - 160) : position + 160]
        if re.search(
            r"(?:known[- ]issues?\s*(?:section\s*)?[§#]?\s*\d+|issue\s*#?\d+|KI-\d+)\b",
            window,
            re.IGNORECASE,
        ):
            hits.append("numbered known-issue closure claim")
            break
    for entry in open_entries:
        tokens = _distinctive_tokens(entry["title"])
        if not tokens:
            continue
        for position in positions:
            window = lower[max(0, position - 200) : position + 200]
            if all(token in window for token in tokens):
                hits.append(f"closure claim for open issue '{entry['title']}'")
                break

    seen: set[str] = set()
    unique: list[str] = []
    for hit in hits:
        if hit not in seen:
            seen.add(hit)
            unique.append(hit)
    return unique


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
    # Read the web copy once so the site check and the claim-fence checks agree.
    site_text = (
        cfg.site_copy.read_text(encoding="utf-8", errors="replace")
        if cfg.site_copy.is_file()
        else None
    )
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
        if site_text is not None and version in site_text:
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

    # 8. An open known issue must not be claimed closed in the shipping notes.
    #    known-issues.md lives in the workspace docs tree, outside the CLI git
    #    repo, so a CLI-only checkout WARNS instead of failing the gate.
    claim_version = version or CLAIM_VERSION_FALLBACK
    known_issues_path = cfg.root / "docs" / "operations" / "known-issues.md"
    if not known_issues_path.is_file():
        results.append(
            Result(
                "known_issues_open",
                "Open known issues not claimed closed",
                WARN,
                f"{short(known_issues_path, cfg.root)} absent; open-issue claims not checked",
            )
        )
    else:
        open_entries = open_known_issues(
            known_issues_path.read_text(encoding="utf-8", errors="replace")
        )
        sources = [
            short(cfg.changelog, cfg.cli_root),
            short(cfg.site_copy, cfg.web_root) if site_text is not None else "web copy absent",
        ]
        if not open_entries:
            results.append(
                Result(
                    "known_issues_open",
                    "Open known issues not claimed closed",
                    PASS,
                    "no open entries in known-issues.md",
                )
            )
        else:
            claims: list[str] = []
            for label, text in (
                ("changelog", changelog_text if changelog_ok else ""),
                ("site copy", site_text or ""),
            ):
                for claim in closure_claims(version_block(text, claim_version), open_entries):
                    claims.append(f"{label}: {claim}")
            if claims:
                results.append(
                    Result(
                        "known_issues_open",
                        "Open known issues not claimed closed",
                        FAIL,
                        f"{len(open_entries)} open; false closure claims: " + "; ".join(claims),
                    )
                )
            else:
                results.append(
                    Result(
                        "known_issues_open",
                        "Open known issues not claimed closed",
                        PASS,
                        f"{len(open_entries)} open entries; no closure claim in v{claim_version} "
                        f"({', '.join(sources)})",
                    )
                )

    # 9. Claim fence on the shipping version's notes: no forbidden phrases.
    #    The web copy is optional; the CLI changelog is always checked.
    fence_hits: list[str] = []
    if changelog_ok:
        for phrase in forbidden_claims(version_block(changelog_text, claim_version)):
            fence_hits.append(f"changelog: '{phrase}'")
    if site_text is not None:
        for phrase in forbidden_claims(version_block(site_text, claim_version)):
            fence_hits.append(f"site copy: '{phrase}'")
    if fence_hits:
        results.append(
            Result(
                "claim_fence",
                "No forbidden claims in shipping notes",
                FAIL,
                "forbidden claims: " + "; ".join(fence_hits),
            )
        )
    else:
        detail = f"v{claim_version} block free of the six forbidden claim phrases"
        if site_text is None:
            detail += " (web copy absent; CLI changelog only)"
        results.append(
            Result("claim_fence", "No forbidden claims in shipping notes", PASS, detail)
        )

    # 10. Version surfaces: root package, workspace package, crate manifests
    #     and the newest changelog heading must all state one version.
    pkg_version, ws_version = read_cargo_versions(cfg.cargo)
    version_problems: list[str] = []
    if pkg_version is None:
        version_problems.append("no [package] version in Cargo.toml")
    elif ws_version is None:
        version_problems.append("no [workspace.package] version in Cargo.toml")
    elif pkg_version != ws_version:
        version_problems.append(
            f"[package] {pkg_version} != [workspace.package] {ws_version}"
        )
    crate_list = crate_versions(cfg.cli_root)
    if crate_list and pkg_version is not None:
        bad_crates = [(name, value) for name, value in crate_list if value != pkg_version]
        if bad_crates:
            sample = ", ".join(f"{name}={value}" for name, value in bad_crates[:3])
            version_problems.append(
                f"{len(bad_crates)} crate manifest(s) disagree with {pkg_version}: {sample}"
            )
    if version_labels and pkg_version is not None:
        if version_labels[0] != pkg_version:
            version_problems.append(
                f"newest changelog heading {version_labels[0]} != Cargo {pkg_version}"
            )
    if version_problems:
        results.append(
            Result(
                "version_surfaces",
                "Version surfaces consistent",
                FAIL,
                "; ".join(version_problems),
            )
        )
    else:
        crate_note = f", {len(crate_list)} crates" if crate_list else ""
        results.append(
            Result(
                "version_surfaces",
                "Version surfaces consistent",
                PASS,
                f"package/workspace/newest heading all {pkg_version}{crate_note}",
            )
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

FIXTURE_CARGO = (
    '[package]\nname = "demo"\nversion = "1.2.3"\n'
    '\n[workspace.package]\nversion = "1.2.3"\n'
)
FIXTURE_KNOWN_ISSUES = """# Known issues

> Status: every entry below is open as of the status banner date.

## 1. The published 0.1.1 manifest is unsigned; mn update will not apply an update

- **Symptom:** `mn update` refuses an unsigned manifest.
- **Tracked at:** Plan 22, P22-003.

## 2. D33 paired backup and restore is not live

- **Symptom:** cloud backups are disabled and no live restore has happened.
- **Tracked at:** Plan 22, P22-005.
"""
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
    known_issue_claim: bool = False,
    forbidden_claim: bool = False,
    version_drift: bool = False,
    known_issues_missing: bool = False,
    no_web: bool = False,
) -> Config:
    """Build a complete fake workspace; the flags inject one drift each."""
    root = base / name
    cli = root / "CLI"
    web = root / "web"
    (cli / ".github").mkdir(parents=True)
    (root / "docs" / "operations").mkdir(parents=True)
    if not no_web:
        (web / "contract").mkdir(parents=True)
        (web / "content" / "docs").mkdir(parents=True)

    cargo_text = FIXTURE_CARGO
    if version_drift:
        cargo_text = cargo_text.replace(
            '[workspace.package]\nversion = "1.2.3"',
            '[workspace.package]\nversion = "9.8.7"',
        )
    (cli / "Cargo.toml").write_text(cargo_text, encoding="utf-8")

    changelog = FIXTURE_CHANGELOG.replace("[1.2.3]", "[9.9.9]") if stale else FIXTURE_CHANGELOG
    if known_issue_claim:
        # Appended inside the `## [1.2.3]` block (the last heading in the fixture).
        changelog += "\nAll known issues are now resolved and closed.\n"
    if forbidden_claim:
        changelog += (
            "\nTeam memory is now live. The release is signed and attested. "
            "Our official benchmark proves nobody does this better, verified by cloud.\n"
        )
    (cli / "CHANGELOG.md").write_text(changelog, encoding="utf-8")
    if not no_web:
        contract = "DIVERGED - not the changelog\n" if diverge else changelog
        (web / "contract" / "changelog.md").write_text(contract, encoding="utf-8")
        (web / "content" / "docs" / "changelog.mdx").write_text(FIXTURE_SITE_COPY, encoding="utf-8")
    release_cfg = FIXTURE_RELEASE_BAD if no_categories else FIXTURE_RELEASE_GOOD
    (cli / ".github" / "release.yml").write_text(release_cfg, encoding="utf-8")
    (cli / ".github" / "RELEASE_NOTES_TEMPLATE.md").write_text(FIXTURE_TEMPLATE, encoding="utf-8")
    for rel in WORKSPACE_DOCS:
        if rel.name == "known-issues.md" and known_issues_missing:
            continue
        content = FIXTURE_KNOWN_ISSUES if rel.name == "known-issues.md" else "Supporting documentation.\n"
        (root / rel).write_text(content, encoding="utf-8")

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
    """Prove every drift case with inline fixtures; exit 0 only if all pass."""
    cases: list[tuple[str, dict[str, bool], dict[str, str], str]] = [
        ("good fixture passes", {}, {}, "every check PASS"),
        ("stale changelog fails", {"stale": True}, {"changelog_heading": FAIL}, "wrong version heading FAILs"),
        ("divergent site copy fails", {"diverge": True}, {"contract_copy": FAIL}, "byte divergence FAILs"),
        ("missing categories fails", {"no_categories": True}, {"release_config": FAIL}, "no categories FAILs"),
        (
            "open issue claimed closed fails",
            {"known_issue_claim": True},
            {"known_issues_open": FAIL},
            "false closure claim FAILs",
        ),
        (
            "forbidden v-release claim fails",
            {"forbidden_claim": True},
            {"claim_fence": FAIL},
            "claim-fence phrase FAILs",
        ),
        (
            "version surface drift fails",
            {"version_drift": True},
            {"version_surfaces": FAIL},
            "workspace/package mismatch FAILs",
        ),
        (
            "missing known-issues doc only warns",
            {"known_issues_missing": True},
            {"known_issues_open": WARN},
            "absent workspace doc WARNs, not FAILs",
        ),
        (
            "CLI-only checkout warns but does not fail",
            {"no_web": True},
            {"contract_copy": WARN, "site_copy": WARN, "claim_fence": PASS, "version_surfaces": PASS},
            "absent web checkout keeps CLI-only gates passing",
        ),
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
