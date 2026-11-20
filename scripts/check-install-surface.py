#!/usr/bin/env python3
"""check-install-surface.py — keep install commands and published artifacts in sync.

One release is described by several independent surfaces: the manifest a static
host serves, the archives and .sha256 sidecars beside it, the Cargo.toml the
binaries were built from, the two installers, the npm dist-tag, and the web
/download page. When any of them drifts, a user finds out — not CI. This script
reads what it can and prints a PASS/FAIL/WARN table, exiting non-zero on FAIL.

It is deliberately stdlib-only Python 3 so it can run on a release runner with
no extra setup, and so the check itself cannot add a dependency to the product.

Modes (one is required):

    check-install-surface.py --local dist/
    check-install-surface.py --live                       # default base URL
    check-install-surface.py --live https://host/dist

Options:

    --cargo PATH          CLI/Cargo.toml (default: ../Cargo.toml)
    --npm-package NAME    npm package whose `latest` tag is checked
                          (default: @astrivya/mcp; pass '' to disable)
    --web-root DIR        web checkout, to check /download fetches the manifest
    --install-sh PATH     override the POSIX installer source
    --install-ps1 PATH    override the Windows installer source
    --require-signature   make a null minisign signature a FAIL
    --timeout SECONDS     bound for each network call (default: 15)

What is asserted, and how hard:

  * manifest version == Cargo.toml [package] version                   FAIL
  * exactly the five target triples, with expected filenames and        FAIL
    64-lowercase-hex digests
  * each target's digest matches its .sha256 sidecar / SHA256SUMS       FAIL
    (local mode only; in live mode the manifest is the source of truth)
  * checksums.sha256 non-empty, 64-hex, with a documented meaning:
      - empty            WARN   (older bundles may omit the legacy alias;
                                 dist.sh now emits it from the verified
                                 x86_64-linux digest, but old manifests exist.)
      - non-hex          FAIL
      - 64-hex, != x86_64-linux digest   WARN  (not the documented legacy alias;
                                 /download falls back to the target digest.)
      - 64-hex, == x86_64-linux digest   PASS
  * install.sh names all five targets; install.ps1 names the Windows target FAIL
  * web /download fetches manifest.json and has no hard-coded version     FAIL
    (only when --web-root is given)
  * npm `latest` dist-tag == manifest version for the stable channel     FAIL
      - non-stable channel: skip equality, emit a WARN
      - registry unreachable: WARN, or FAIL when --live was requested
  * signatures.minisign:
      - null on stable: WARN that `mn update` refuses the release;
        FAIL only with --require-signature (secrets may not be configured)
      - present: PASS

Every network call has a timeout and a byte cap. Offline is not a failure in
local mode; it is a failure in live mode, because a live check with no reachability
cannot check the thing it was asked to check.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import urllib.error
import urllib.request
from pathlib import Path

DEFAULT_LIVE_BASE = "https://memnoth.astrivya.ai/dist"
NPM_REGISTRY = "https://registry.npmjs.org/@astrivya%2fmcp"
DEFAULT_NPM_PACKAGE = "@astrivya/mcp"
LINUX_X86 = "x86_64-unknown-linux-gnu"
WINDOWS = "x86_64-pc-windows-msvc"

# The one list this check owns. It is a *test* of the release workflow matrix
# (release.yml), the installers and the manifest — not a second source used to
# build anything. It must be updated deliberately when a target is added.
EXPECTED_TARGETS: dict[str, str] = {
    "x86_64-unknown-linux-gnu": "memnoth-x86_64-unknown-linux-gnu.tar.gz",
    "aarch64-unknown-linux-gnu": "memnoth-aarch64-unknown-linux-gnu.tar.gz",
    "x86_64-apple-darwin": "memnoth-x86_64-apple-darwin.tar.gz",
    "aarch64-apple-darwin": "memnoth-aarch64-apple-darwin.tar.gz",
    "x86_64-pc-windows-msvc": "memnoth-x86_64-pc-windows-msvc.zip",
}

HEX64 = re.compile(r"^[0-9a-f]{64}$")
VERSION_LITERAL = re.compile(r"\b\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?\b")
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
SCRIPT_DIR = Path(__file__).resolve().parent


class FetchError(Exception):
    """A source (local file or remote URL) could not be read."""


class Report:
    """Collects verdicts, prints an aligned table, and owns the exit code."""

    def __init__(self) -> None:
        self.rows: list[tuple[str, str, str]] = []

    def add(self, status: str, check: str, detail: str) -> None:
        self.rows.append((status, check, detail))

    def ok(self, check: str, detail: str) -> None:
        self.add("PASS", check, detail)

    def fail(self, check: str, detail: str) -> None:
        self.add("FAIL", check, detail)

    def warn(self, check: str, detail: str) -> None:
        self.add("WARN", check, detail)

    def info(self, check: str, detail: str) -> None:
        self.add("INFO", check, detail)

    def skip(self, check: str, detail: str) -> None:
        self.add("SKIP", check, detail)

    def render(self) -> int:
        if not self.rows:
            print("no checks ran")
            return 1
        counts = {name: sum(1 for row in self.rows if row[0] == name)
                  for name in ("PASS", "FAIL", "WARN", "INFO", "SKIP")}
        w1 = max(len("STATUS"), max(len(row[0]) for row in self.rows))
        w2 = max(len("CHECK"), max(len(row[1]) for row in self.rows))
        print(f"{'STATUS':<{w1}}  {'CHECK':<{w2}}  DETAIL")
        print(f"{'-' * w1}  {'-' * w2}  {'-' * 20}")
        for status, check, detail in self.rows:
            print(f"{status:<{w1}}  {check:<{w2}}  {detail}")
        print(
            f"\nRESULT: {counts['PASS']} passed, {counts['FAIL']} failed, "
            f"{counts['WARN']} warnings, {counts['INFO']} info, "
            f"{counts['SKIP']} skipped"
        )
        return 1 if counts["FAIL"] else 0


def http_get_text(url: str, timeout: float) -> str:
    """Fetch a bounded amount of text with a hard timeout. Raises on any error."""
    request = urllib.request.Request(
        url,
        headers={"User-Agent": "memnoth-check-install-surface/1", "Accept": "*/*"},
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        status = getattr(response, "status", 200)
        if status >= 400:  # pragma: no cover - urlopen normally raises first
            raise FetchError(f"HTTP {status} from {url}")
        raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise FetchError(f"response from {url} exceeds {MAX_RESPONSE_BYTES} bytes")
    return raw.decode("utf-8", "replace")


def read_cargo_version(path: Path) -> str:
    """Read [package] version from a Cargo.toml, ignoring dependency versions."""
    section = ""
    for line in path.read_text(encoding="utf-8").splitlines():
        stripped = line.strip()
        if stripped.startswith("[") and stripped.endswith("]"):
            section = stripped[1:-1].strip()
            continue
        if section == "package":
            match = re.match(r'^version\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    raise FetchError(f"no [package] version in {path}")


def load_manifest(args: argparse.Namespace) -> tuple[dict, str, Path | None]:
    """Return (manifest, human-readable source, local dist root or None)."""
    if args.local:
        root = Path(args.local).resolve()
        if not root.is_dir():
            raise FetchError(f"--local {root} is not a directory")
        candidates = [root / "latest" / "manifest.json", root / "manifest.json"]
        candidates.extend(sorted(root.glob("*/manifest.json")))
        for candidate in candidates:
            if candidate.is_file():
                data = json.loads(candidate.read_text(encoding="utf-8"))
                return data, str(candidate), root
        raise FetchError(f"no manifest.json under {root} (looked in latest/, . and */)")
    base = args.live.rstrip("/")
    url = f"{base}/latest/manifest.json"
    return json.loads(http_get_text(url, args.timeout)), url, None


def first_token(path: Path) -> str:
    """Read a digest from a `.sha256` file: first whitespace-separated token."""
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        parts = line.split()
        if parts:
            return parts[0].lower()
    return ""


def check_version(report: Report, manifest: dict, cargo: Path) -> None:
    try:
        cargo_version = read_cargo_version(cargo)
    except (OSError, FetchError) as exc:
        report.fail("version", f"cannot read Cargo version: {exc}")
        return
    manifest_version = manifest.get("version")
    if manifest_version == cargo_version:
        report.ok("version", f"manifest {manifest_version} == Cargo.toml {cargo_version}")
    else:
        report.fail("version", f"manifest {manifest_version!r} != Cargo.toml {cargo_version!r}")


def check_targets(report: Report, manifest: dict) -> dict[str, dict]:
    """Validate the target list exactly; return a triple -> target map."""
    targets = manifest.get("targets")
    if not isinstance(targets, list):
        report.fail("targets", f"manifest targets is {type(targets).__name__}, expected list")
        return {}
    by_triple: dict[str, dict] = {}
    for target in targets:
        if isinstance(target, dict) and isinstance(target.get("triple"), str):
            by_triple[target["triple"]] = target
        else:
            report.fail("targets", f"malformed target entry: {target!r}")

    missing = sorted(set(EXPECTED_TARGETS) - set(by_triple))
    extra = sorted(set(by_triple) - set(EXPECTED_TARGETS))
    if len(targets) == len(EXPECTED_TARGETS) and not missing and not extra:
        report.ok("targets", f"exactly {len(targets)} expected triples present")
    else:
        detail = [f"found {len(targets)} targets, expected {len(EXPECTED_TARGETS)}"]
        if missing:
            detail.append(f"missing: {', '.join(missing)}")
        if extra:
            detail.append(f"unexpected: {', '.join(extra)}")
        report.fail("targets", "; ".join(detail))

    for triple, expected_name in EXPECTED_TARGETS.items():
        target = by_triple.get(triple)
        if target is None:
            continue
        filename = target.get("filename")
        digest = target.get("sha256")
        if filename != expected_name:
            report.fail(f"target {triple}", f"filename {filename!r}, expected {expected_name!r}")
        else:
            report.ok(f"target {triple}", f"filename {filename}")
        if isinstance(digest, str) and HEX64.match(digest):
            report.ok(f"digest {triple}", digest)
        else:
            report.fail(f"digest {triple}", f"not a 64-lowercase-hex sha256: {digest!r}")
    return by_triple


def find_sidecar(dist_root: Path, filename: str) -> Path | None:
    """Find <filename>.sha256 anywhere under the dist bundle."""
    direct = dist_root / f"{filename}.sha256"
    if direct.is_file():
        return direct
    for found in sorted(dist_root.rglob(f"{filename}.sha256")):
        if found.is_file():
            return found
    return None


def check_sidecars(report: Report, manifest: dict, dist_root: Path | None,
                   by_triple: dict[str, dict]) -> None:
    if dist_root is None:
        report.skip("sidecars", "live mode: local sidecar bytes unavailable; manifest is authoritative")
        return

    sums: dict[str, str] = {}
    sums_path = dist_root / "SHA256SUMS"
    if sums_path.is_file():
        for line in sums_path.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.split()
            if len(parts) >= 2:
                sums[Path(parts[1]).name] = parts[0].lower()
        report.ok("SHA256SUMS", f"read {len(sums)} entries from {sums_path.name}")

    for triple, target in by_triple.items():
        filename = target.get("filename")
        expected = target.get("sha256")
        if not isinstance(filename, str) or not isinstance(expected, str):
            continue
        sidecar = find_sidecar(dist_root, filename)
        if sidecar is not None:
            actual = first_token(sidecar)
            source = sidecar.relative_to(dist_root).as_posix()
        elif filename in sums:
            actual = sums[filename]
            source = "SHA256SUMS"
        else:
            report.fail(f"sidecar {triple}", f"no .sha256 and no SHA256SUMS entry for {filename}")
            continue
        if actual == expected:
            report.ok(f"sidecar {triple}", f"{filename} matches ({source})")
        else:
            report.fail(f"sidecar {triple}",
                        f"{filename} mismatch: manifest {expected}, {source} {actual}")


def check_checksums_field(report: Report, manifest: dict,
                          by_triple: dict[str, dict]) -> None:
    value = (manifest.get("checksums") or {}).get("sha256")
    if not value:
        report.warn(
            "checksums.sha256",
            "empty; older bundles omit the legacy alias. dist.sh now emits the "
            "verified x86_64-linux digest here; /download falls back to that "
            "target digest when the field is missing. Not failing the release.",
        )
        return
    if not HEX64.match(value):
        report.fail("checksums.sha256", f"not 64-lowercase-hex: {value!r}")
        return
    linux = (by_triple.get(LINUX_X86) or {}).get("sha256")
    if linux and value == linux:
        report.ok("checksums.sha256", f"matches {LINUX_X86} digest (the /download meaning)")
    else:
        report.warn(
            "checksums.sha256",
            f"64-hex but not the {LINUX_X86} digest ({linux}); the documented alias "
            "is the x86_64-linux archive digest, so verify this value before trusting it",
        )


def resolve_script(explicit: str | None, dist_root: Path | None, name: str) -> Path:
    if explicit:
        return Path(explicit)
    if dist_root is not None:
        published = dist_root / name
        if published.is_file():
            return published
    return SCRIPT_DIR / name


def check_installers(report: Report, args: argparse.Namespace,
                     dist_root: Path | None) -> None:
    posix = resolve_script(args.install_sh, dist_root, "install.sh")
    windows = resolve_script(args.install_ps1, dist_root, "install.ps1")
    try:
        posix_text = posix.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        report.fail("install.sh", f"cannot read {posix}: {exc}")
        posix_text = ""
    try:
        windows_text = windows.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        report.fail("install.ps1", f"cannot read {windows}: {exc}")
        windows_text = ""

    for triple in EXPECTED_TARGETS:
        if triple in posix_text:
            report.ok(f"install.sh {triple}", "named")
        else:
            report.fail(f"install.sh {triple}", f"missing target triple {triple}")
    if WINDOWS in windows_text:
        report.ok("install.ps1 windows", f"names {WINDOWS}")
    else:
        report.fail("install.ps1 windows", f"missing target triple {WINDOWS}")


def check_web(report: Report, args: argparse.Namespace) -> None:
    if not args.web_root:
        report.skip("web /download", "no --web-root given")
        return
    root = Path(args.web_root)
    preferred = root / "src" / "routes" / "download.tsx"
    candidates = [preferred] if preferred.is_file() else sorted(root.rglob("download.tsx"))
    path = next((c for c in candidates if c.is_file()), None)
    if path is None:
        report.fail("web /download", f"no download.tsx under {root}")
        return
    text = path.read_text(encoding="utf-8", errors="replace")
    if "manifest.json" in text and re.search(r"\bfetch\s*\(", text):
        report.ok("web /download", f"fetches manifest.json ({path.relative_to(root).as_posix()})")
    else:
        report.fail("web /download", f"{path} does not fetch manifest.json")
    literal = VERSION_LITERAL.search(text)
    if literal:
        report.fail("web /download version", f"hard-coded version literal {literal.group()!r}")
    else:
        report.ok("web /download version", "no hard-coded version literal")


def check_npm(report: Report, args: argparse.Namespace, manifest: dict) -> None:
    if not args.npm_package:
        report.skip("npm dist-tag", "--npm-package is empty")
        return
    try:
        url = NPM_REGISTRY if args.npm_package == DEFAULT_NPM_PACKAGE else (
            "https://registry.npmjs.org/" + args.npm_package.replace("/", "%2f")
        )
        data = json.loads(http_get_text(url, args.timeout))
    except (OSError, ValueError, FetchError, urllib.error.URLError) as exc:
        message = f"npm registry unreachable ({exc}); cannot check dist-tag"
        if args.live:
            report.fail("npm dist-tag", message + " (--live)")
        else:
            report.warn("npm dist-tag", message)
        return

    latest = (data.get("dist-tags") or {}).get("latest")
    version = manifest.get("version")
    channel = manifest.get("channel")
    if channel == "stable":
        if latest == version:
            report.ok("npm dist-tag", f"latest == {version} == manifest version")
        else:
            report.fail("npm dist-tag", f"registry latest={latest!r}, manifest version={version!r}")
    else:
        report.warn("npm dist-tag",
                    f"channel={channel!r}; registry latest={latest!r} is the stable tag, "
                    f"not asserting equality with {version!r}")


def check_signature(report: Report, manifest: dict, require: bool) -> None:
    signatures = manifest.get("signatures") or {}
    signature = signatures.get("minisign")
    channel = manifest.get("channel")
    if signature:
        report.ok("signature", f"minisign present (channel={channel!r})")
    else:
        message = (f"signatures.minisign is null on {channel!r} channel; "
                   "'mn update' refuses an unsigned release")
        if require:
            report.fail("signature", message + " (--require-signature)")
        else:
            report.warn("signature", message + "; passing without --require-signature")
    if not signatures.get("cosign"):
        report.info("signature", "cosign is null (not configured); not a self-update blocker")


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Check that install commands and published artifacts agree.",
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--local", metavar="DIST_DIR", help="dist bundle directory to check")
    mode.add_argument("--live", nargs="?", const=DEFAULT_LIVE_BASE, metavar="BASE_URL",
                      help=f"live base URL (default: {DEFAULT_LIVE_BASE})")
    parser.add_argument("--cargo", default=str(SCRIPT_DIR.parent / "Cargo.toml"),
                        metavar="PATH", help="CLI/Cargo.toml (default: ../Cargo.toml)")
    parser.add_argument("--npm-package", default=DEFAULT_NPM_PACKAGE, metavar="NAME",
                        help=f"npm package to check (default: {DEFAULT_NPM_PACKAGE}; '' disables)")
    parser.add_argument("--web-root", metavar="DIR", help="web checkout to check /download")
    parser.add_argument("--install-sh", metavar="PATH", help="override POSIX installer source")
    parser.add_argument("--install-ps1", metavar="PATH", help="override Windows installer source")
    parser.add_argument("--require-signature", action="store_true",
                        help="make a null minisign signature a FAIL")
    parser.add_argument("--timeout", type=float, default=15.0, metavar="SECONDS",
                        help="per-request network timeout (default: 15)")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    report = Report()

    try:
        manifest, source, dist_root = load_manifest(args)
    except (OSError, ValueError, FetchError, urllib.error.URLError) as exc:
        report.fail("manifest", f"cannot load manifest: {exc}")
        return report.render()
    report.ok("manifest", f"read from {source}")

    check_version(report, manifest, Path(args.cargo))
    by_triple = check_targets(report, manifest)
    check_sidecars(report, manifest, dist_root, by_triple)
    check_checksums_field(report, manifest, by_triple)
    check_installers(report, args, dist_root)
    check_web(report, args)
    check_npm(report, args, manifest)
    check_signature(report, manifest, args.require_signature)
    return report.render()


if __name__ == "__main__":
    raise SystemExit(main())
