#!/usr/bin/env python3
"""Verify a filled paired production upload → purge → restore → recall evidence file.

Stdlib only; no network, no secret access, no writes. The verifier checks that the
operator-filled JSON is complete, internally consistent, and no stronger than the
steps actually observed. It refuses to mark the gate closed when the run is a CI
job or a unit test.

The verifier does NOT perform the observation and does NOT close the gate in the
plan. It only validates the evidence record produced by a human operator.

Usage:
    python3 scripts/paired-restore-evidence/verify-evidence.py <filled-evidence.json>
    python3 scripts/paired-restore-evidence/verify-evidence.py <file> --json
    python3 scripts/paired-restore-evidence/verify-evidence.py --self-test

Exit 0 when the evidence is complete and consistent, 1 otherwise. A gate whose
status_claim is "closed" is only accepted from an operator-manual session.
"""

from __future__ import annotations

import argparse
import copy
import json
import os
import re
import sys
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

TEMPLATE_NAME = "evidence-template.json"

STEP_IDS = (
    "server_blob_inspection",
    "wrong_key_wrong_project_refusal",
    "tamper_refusal",
    "purge_replay_tombstone",
    "staged_restore_stopped_stack",
    "recall_clean_checkout",
)

# Presence of any of these makes the run non-interactive for gate-closure purposes.
CI_MARKERS = (
    "CI",
    "GITHUB_ACTIONS",
    "CIRCLECI",
    "CIRCLE_NODE_INDEX",
    "GITLAB_CI",
    "TF_BUILD",
    "JENKINS_URL",
    "BUILDKITE",
    "TRAVIS",
    "APPVEYOR",
    "BITBUCKET_BUILD_NUMBER",
    "CODEBUILD_BUILD_ID",
    "TEAMCITY_VERSION",
    "HEROKU_TEST_RUN_ID",
    "DRONE",
)
UNIT_TEST_MARKERS = (
    "PYTEST_CURRENT_TEST",
    "UNITTEST_CURRENT_TEST",
    "MEMNOTH_EVIDENCE_UNIT_TEST",
)
ALL_MARKERS = CI_MARKERS + UNIT_TEST_MARKERS

PLACEHOLDER_RE = re.compile(r"<\s*FILL\b|<\s*FILL\s*:", re.IGNORECASE)
BARE_TODO = {"", "todo", "tbd", "n/a", "na", "none", "null", "<...>", "placeholder"}
DATE_RE = re.compile(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z")
SHA256_RE = re.compile(r"[0-9a-f]{64}")

SECRET_KEY_DENY = {
    "recovery_phrase",
    "password",
    "secret",
    "token",
    "api_key",
    "private_key",
    "credential",
    "credentials",
    "access_key",
    "session_token",
    "authorization",
}
SECRET_VALUE_PATTERNS = (
    ("private-key block", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("age secret key", re.compile(r"AGE-SECRET-KEY-[0-9A-Z]+", re.IGNORECASE)),
    ("JWT", re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.")),
    ("GitHub token", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}\b")),
    ("AWS access key", re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("Slack token", re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b")),
    ("sk-style key", re.compile(r"\bsk-[A-Za-z0-9_-]{20,}\b")),
)


@dataclass
class Evaluation:
    problems: list[str] = field(default_factory=list)
    gate_status: str = "unverified"
    runtime: str = "operator-manual"

    @property
    def ok(self) -> bool:
        return not self.problems


# --------------------------------------------------------------------------- #
# Environment / placeholder / secret helpers
# --------------------------------------------------------------------------- #
def _env_truthy(name: str) -> bool:
    value = os.environ.get(name)
    return value is not None and value.strip().lower() not in ("", "0", "false", "no", "off")


def detect_runtime() -> str:
    """Return 'ci', 'unit-test', or 'operator-manual' from the current process."""
    if any(_env_truthy(name) for name in UNIT_TEST_MARKERS):
        return "unit-test"
    if any(_env_truthy(name) for name in CI_MARKERS):
        return "ci"
    return "operator-manual"


@contextmanager
def runtime_env(markers: tuple[str, ...] = ()):
    """Temporarily clear all detection markers and set the named ones (self-test)."""
    saved = {name: os.environ.get(name) for name in ALL_MARKERS}
    for name in ALL_MARKERS:
        os.environ.pop(name, None)
    for name in markers:
        os.environ[name] = "1"
    try:
        yield
    finally:
        for name in ALL_MARKERS:
            os.environ.pop(name, None)
        for name, value in saved.items():
            if value is not None:
                os.environ[name] = value


def is_placeholder(value: Any) -> bool:
    if not isinstance(value, str):
        return False
    text = value.strip()
    return bool(PLACEHOLDER_RE.search(text)) or text.lower() in BARE_TODO


def _dotted(path: tuple[str, ...]) -> str:
    return ".".join(path) or "<root>"


def _is_inactive(path: tuple[str, ...], inactive: set[tuple[str, ...]]) -> bool:
    return any(path[: len(prefix)] == prefix for prefix in inactive)


def inactive_prefixes(evidence: dict) -> set[tuple[str, ...]]:
    """Detail fields of an observation left unobserved may stay as placeholders."""
    prefixes: set[tuple[str, ...]] = set()
    observations = evidence.get("observations")
    if isinstance(observations, dict):
        for step in STEP_IDS:
            node = observations.get(step)
            if isinstance(node, dict) and node.get("observed") is False:
                prefixes.add(("observations", step))
    return prefixes


# --------------------------------------------------------------------------- #
# Structural checks driven by the template itself
# --------------------------------------------------------------------------- #
def check_structure(
    template: Any,
    evidence: Any,
    inactive: set[tuple[str, ...]],
    problems: list[str],
    path: tuple[str, ...] = (),
) -> None:
    if _is_inactive(path, inactive):
        return
    here = _dotted(path)
    if isinstance(template, dict):
        if not isinstance(evidence, dict):
            problems.append(f"{here}: expected an object")
            return
        for key, tval in template.items():
            child = path + (str(key),)
            if key not in evidence:
                problems.append(f"missing required field: {_dotted(child)}")
                continue
            check_structure(tval, evidence[key], inactive, problems, child)
        return
    if isinstance(template, list):
        if not isinstance(evidence, list):
            problems.append(f"{here}: expected an array")
            return
        if template:
            for index, item in enumerate(evidence):
                check_structure(template[0], item, inactive, problems, path + (f"[{index}]",))
        return
    if isinstance(template, bool):
        if not isinstance(evidence, bool):
            problems.append(f"{here}: expected a boolean")
        return
    if isinstance(template, (int, float)):
        if isinstance(evidence, bool) or not isinstance(evidence, (int, float)):
            problems.append(f"{here}: expected a number")
        return
    if not isinstance(evidence, str):
        problems.append(f"{here}: expected a string")
        return
    if is_placeholder(evidence):
        problems.append(f"{here}: unfilled placeholder")


def scan_secrets(
    evidence: Any,
    inactive: set[tuple[str, ...]],
    problems: list[str],
    path: tuple[str, ...] = (),
) -> None:
    if _is_inactive(path, inactive):
        return
    if isinstance(evidence, dict):
        for key, value in evidence.items():
            key_text = str(key).strip().lower()
            if key_text in SECRET_KEY_DENY:
                problems.append(f"secret-bearing field name at {_dotted(path + (key_text,))}")
            scan_secrets(value, inactive, problems, path + (str(key),))
        return
    if isinstance(evidence, list):
        for index, item in enumerate(evidence):
            scan_secrets(item, inactive, problems, path + (f"[{index}]",))
        return
    if isinstance(evidence, str):
        for label, pattern in SECRET_VALUE_PATTERNS:
            if pattern.search(evidence):
                problems.append(f"possible {label} at {_dotted(path)}")


# --------------------------------------------------------------------------- #
# Semantic checks: outcomes must never exceed recorded observations
# --------------------------------------------------------------------------- #
def _expect(node: dict, field: str, allowed: set[str], step: str, problems: list[str]) -> None:
    value = node.get(field)
    if not isinstance(value, str) or value not in allowed:
        problems.append(
            f"observations.{step}.{field} must be one of {sorted(allowed)} (got {value!r})"
        )


def validate_semantics(evidence: dict, problems: list[str]) -> tuple[str, set[str]]:
    detected = detect_runtime()
    context = evidence.get("context")
    if not isinstance(context, dict):
        problems.append("context: expected an object")
        context = {}

    runtime_claim = context.get("runtime_claim")
    if runtime_claim != detected:
        problems.append(
            f"context.runtime_claim {runtime_claim!r} disagrees with detected runtime {detected!r}"
        )

    operator = context.get("operator")
    if not isinstance(operator, dict):
        problems.append("context.operator: expected an object")
    else:
        email = operator.get("email")
        if isinstance(email, str) and "@" not in email:
            problems.append("context.operator.email does not look like an email address")

    observed_on = context.get("observed_on_utc")
    if not isinstance(observed_on, str) or not DATE_RE.fullmatch(observed_on):
        problems.append("context.observed_on_utc must be YYYY-MM-DDTHH:MM:SSZ")

    environment = context.get("environment")
    if not isinstance(environment, dict):
        problems.append("context.environment: expected an object")
    else:
        url = environment.get("cloud_base_url")
        if isinstance(url, str):
            if not url.startswith("https://") or any(
                host in url.lower() for host in ("localhost", "127.0.0.1")
            ):
                problems.append("context.environment.cloud_base_url must be a production https origin")

    steps = evidence.get("observed_steps")
    if not isinstance(steps, list) or not all(isinstance(item, str) for item in steps):
        problems.append("observed_steps must be an array of step ids")
        steps = []
    else:
        if len(steps) != len(set(steps)):
            problems.append("observed_steps contains duplicate step ids")
        unknown = [item for item in steps if item not in STEP_IDS]
        if unknown:
            problems.append(f"observed_steps contains unknown step ids: {unknown}")
    observed = set(steps)

    observations = evidence.get("observations")
    if not isinstance(observations, dict):
        problems.append("observations: expected an object")
        observations = {}

    for step in STEP_IDS:
        node = observations.get(step)
        if not isinstance(node, dict):
            problems.append(f"observations.{step}: expected an object")
            continue
        flag = node.get("observed")
        if flag is True and step not in observed:
            problems.append(f"observations.{step}.observed=true but step is absent from observed_steps")
        elif flag is False and step in observed:
            problems.append(f"observations.{step}.observed=false but step is listed in observed_steps")
        if flag is not True:
            continue

        if step == "server_blob_inspection":
            algorithm = node.get("digest_algorithm")
            if not isinstance(algorithm, str) or algorithm.lower().replace("-", "") != "sha256":
                problems.append("observations.server_blob_inspection.digest_algorithm must be sha256")
            for name in ("upload_digest", "server_digest"):
                digest = node.get(name)
                if not isinstance(digest, str) or not SHA256_RE.fullmatch(digest):
                    problems.append(
                        f"observations.server_blob_inspection.{name} must be 64 lowercase hex digits"
                    )
            upload = node.get("upload_digest")
            server = node.get("server_digest")
            if isinstance(upload, str) and isinstance(server, str) and upload != server:
                problems.append("observations.server_blob_inspection: upload and server digests differ")
            if node.get("digest_match") is not True:
                problems.append("observations.server_blob_inspection.digest_match must be true")
            _expect(node, "plaintext_result", {"unreadable"}, step, problems)
        elif step == "wrong_key_wrong_project_refusal":
            _expect(node, "wrong_key_result", {"refused"}, step, problems)
            _expect(node, "wrong_project_result", {"refused"}, step, problems)
        elif step == "tamper_refusal":
            _expect(node, "verification_result", {"refused"}, step, problems)
        elif step == "purge_replay_tombstone":
            _expect(node, "tombstone_result", {"tombstoned"}, step, problems)
        elif step == "staged_restore_stopped_stack":
            _expect(node, "stack_state_before", {"stopped"}, step, problems)
            _expect(node, "digest_verification_result", {"verified"}, step, problems)
            _expect(node, "restore_result", {"restored"}, step, problems)
        elif step == "recall_clean_checkout":
            _expect(node, "checkout_state", {"clean"}, step, problems)
            _expect(node, "recall_result", {"recalled"}, step, problems)

    claims = evidence.get("claims")
    if not isinstance(claims, list):
        problems.append("claims must be an array")
        claims = []
    for index, claim in enumerate(claims):
        if not isinstance(claim, dict):
            problems.append(f"claims[{index}]: expected an object")
            continue
        if claim.get("strength") != "observed":
            problems.append(f"claims[{index}].strength must be 'observed'")
        support = claim.get("supporting_observation_ids")
        if not isinstance(support, list) or not support or not all(
            isinstance(item, str) for item in support
        ):
            problems.append(
                f"claims[{index}].supporting_observation_ids must be a non-empty array of step ids"
            )
            continue
        unsupported = [item for item in support if item not in observed]
        if unsupported:
            problems.append(f"claims[{index}] claims support from unobserved steps: {unsupported}")

    attestation = evidence.get("secrets_attestation")
    if not isinstance(attestation, dict):
        problems.append("secrets_attestation: expected an object")
    elif attestation.get("no_credentials_recorded") is not True:
        problems.append("secrets_attestation.no_credentials_recorded must be true")

    return detected, observed


def validate_gate(
    evidence: dict,
    observed: set[str],
    detected: str,
    problems: list[str],
) -> str:
    gate = evidence.get("gate")
    if not isinstance(gate, dict):
        problems.append("gate: expected an object")
        return "unverified"
    status = gate.get("status_claim")
    if status not in ("open", "closed"):
        problems.append("gate.status_claim must be 'open' or 'closed'")
        status = "unverified"
    requested = gate.get("closure_requested")
    if not isinstance(requested, bool):
        problems.append("gate.closure_requested must be a boolean")
        requested = None
    basis = gate.get("closure_basis_observation_ids")
    if not isinstance(basis, list) or not all(isinstance(item, str) for item in basis):
        problems.append("gate.closure_basis_observation_ids must be an array of step ids")
        basis = None

    required = set(STEP_IDS)
    if status == "closed":
        if requested is not True:
            problems.append("gate status is 'closed' but closure_requested is not true")
        if basis is None or len(basis) != len(required) or set(basis) != required:
            problems.append(
                "gate status is 'closed' but closure_basis_observation_ids is not exactly "
                "the full set of required steps"
            )
        missing = sorted(required - observed)
        if missing:
            problems.append(f"gate status is 'closed' but required steps were not observed: {missing}")
        if detected != "operator-manual":
            problems.append(
                f"gate closure refused: detected runtime is '{detected}', "
                "not an operator-manual session"
            )
    else:
        if requested is True:
            problems.append("gate.closure_requested=true while status_claim is not 'closed'")
        if basis:
            problems.append("gate.closure_basis_observation_ids must be empty while the gate is open")
    return status


def evaluate(template: dict, evidence: dict) -> Evaluation:
    problems: list[str] = []
    inactive = inactive_prefixes(evidence)
    check_structure(template, evidence, inactive, problems)
    scan_secrets(evidence, inactive, problems)
    detected, observed = validate_semantics(evidence, problems)
    status = validate_gate(evidence, observed, detected, problems)
    return Evaluation(problems=problems, gate_status=status, runtime=detected)


# --------------------------------------------------------------------------- #
# Self test (synthetic fixtures only — no production data, no secrets)
# --------------------------------------------------------------------------- #
def _synthetic_evidence(template: dict) -> dict:
    evidence = copy.deepcopy(template)
    evidence["gate"]["closure_note"] = "synthetic positive control"
    evidence["context"]["operator"] = {
        "name": "Synthetic Operator",
        "email": "operator@example.invalid",
        "role": "synthetic fixture",
    }
    evidence["context"]["observed_on_utc"] = "2026-11-21T12:00:00Z"
    environment = evidence["context"]["environment"]
    environment["cloud_base_url"] = "https://cloud.example.invalid"
    environment["org_id"] = "synthetic-org"
    environment["project_id"] = "synthetic-project"
    environment["cli_revision"] = "synthetic"
    environment["cloud_revision"] = "synthetic"
    environment["stopped_stack_id"] = "synthetic-stack"

    evidence["observed_steps"] = list(STEP_IDS)
    for step in STEP_IDS:
        node = evidence["observations"][step]
        node["observed"] = True
        for key, value in node.items():
            if is_placeholder(value):
                node[key] = "synthetic-value"

    blob = evidence["observations"]["server_blob_inspection"]
    blob["digest_algorithm"] = "sha256"
    blob["upload_digest"] = "a" * 64
    blob["server_digest"] = "a" * 64
    blob["digest_match"] = True
    blob["plaintext_result"] = "unreadable"
    evidence["observations"]["wrong_key_wrong_project_refusal"]["wrong_key_result"] = "refused"
    evidence["observations"]["wrong_key_wrong_project_refusal"]["wrong_project_result"] = "refused"
    evidence["observations"]["tamper_refusal"]["verification_result"] = "refused"
    evidence["observations"]["purge_replay_tombstone"]["tombstone_result"] = "tombstoned"
    staged = evidence["observations"]["staged_restore_stopped_stack"]
    staged["stack_state_before"] = "stopped"
    staged["digest_verification_result"] = "verified"
    staged["restore_result"] = "restored"
    recall = evidence["observations"]["recall_clean_checkout"]
    recall["checkout_state"] = "clean"
    recall["recall_result"] = "recalled"

    evidence["claims"] = [
        {
            "id": "synthetic-claim",
            "statement": "synthetic positive control",
            "supporting_observation_ids": list(STEP_IDS),
            "strength": "observed",
        }
    ]
    evidence["secrets_attestation"]["no_credentials_recorded"] = True
    evidence["secrets_attestation"]["handling_note"] = "synthetic"
    return evidence


def _closed(evidence: dict) -> dict:
    result = copy.deepcopy(evidence)
    result["gate"]["status_claim"] = "closed"
    result["gate"]["closure_requested"] = True
    result["gate"]["closure_basis_observation_ids"] = list(STEP_IDS)
    return result


def run_self_test(template: dict) -> int:
    cases: list[tuple[str, bool, str]] = []

    def check(label: str, evidence: dict, markers: tuple[str, ...], expect_ok: bool, note: str) -> None:
        with runtime_env(markers):
            result = evaluate(template, evidence)
        cases.append((label, result.ok == expect_ok, note))

    check("template with placeholders is rejected", copy.deepcopy(template), (), False, "expected FAIL")
    positive = _synthetic_evidence(template)
    check("filled evidence, open gate, operator", positive, (), True, "expected PASS")
    check("filled evidence, closed gate, operator", _closed(positive), (), True, "expected PASS")
    check("closed gate from CI env is refused", _closed(positive), ("CI",), False, "expected FAIL")
    check(
        "closed gate from unit-test env is refused",
        _closed(positive),
        ("PYTEST_CURRENT_TEST",),
        False,
        "expected FAIL",
    )

    overclaim = copy.deepcopy(positive)
    dropped = STEP_IDS[-1]
    overclaim["observed_steps"] = [step for step in STEP_IDS if step != dropped]
    overclaim["observations"][dropped]["observed"] = False
    check("claim citing an unobserved step is rejected", overclaim, (), False, "expected FAIL")

    readable = copy.deepcopy(positive)
    readable["observations"]["server_blob_inspection"]["plaintext_result"] = "readable"
    check("readable plaintext observation is rejected", readable, (), False, "expected FAIL")

    secret = copy.deepcopy(positive)
    secret["claims"][0]["statement"] = "prefix ghp_" + "A" * 24
    check("secret-shaped value is rejected", secret, (), False, "expected FAIL")

    missing = copy.deepcopy(positive)
    del missing["context"]["operator"]["email"]
    check("missing required field is rejected", missing, (), False, "expected FAIL")

    print("SELF TEST (synthetic fixtures only)")
    for label, passed, note in cases:
        print(f"  [{'PASS' if passed else 'FAIL'}] {label:<42} {note}")
    result = all(passed for _, passed, _ in cases)
    print("SELF TEST RESULT: " + ("PASS" if result else "FAIL"))
    return 0 if result else 1


# --------------------------------------------------------------------------- #
# Entry point
# --------------------------------------------------------------------------- #
def _load_json(path: Path, label: str) -> dict | None:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except OSError as exc:
        print(f"ERROR: cannot read {label} file {path}: {exc}", file=sys.stderr)
        return None
    except json.JSONDecodeError as exc:
        print(f"ERROR: invalid JSON in {label} file {path}: {exc}", file=sys.stderr)
        return None
    if not isinstance(data, dict):
        print(f"ERROR: {label} file {path} must contain a JSON object", file=sys.stderr)
        return None
    return data


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "evidence",
        nargs="?",
        help="filled evidence JSON (default: sibling evidence.json)",
    )
    parser.add_argument("--self-test", action="store_true", help="run inline synthetic cases")
    parser.add_argument("--json", action="store_true", help="emit a single JSON result object")
    args = parser.parse_args(argv)

    here = Path(__file__).resolve().parent
    template_path = here / TEMPLATE_NAME
    template = _load_json(template_path, "template")
    if template is None:
        return 1
    if args.self_test:
        return run_self_test(template)

    evidence_path = Path(args.evidence) if args.evidence else here / "evidence.json"
    if evidence_path.resolve() == template_path:
        print("ERROR: this is the blank template; copy it and fill it in first", file=sys.stderr)
        return 1
    evidence = _load_json(evidence_path, "evidence")
    if evidence is None:
        return 1

    result = evaluate(template, evidence)
    if args.json:
        print(
            json.dumps(
                {
                    "result": "PASS" if result.ok else "FAIL",
                    "gate": result.gate_status if result.ok else "unverified",
                    "runtime": result.runtime,
                    "problems": result.problems,
                },
                indent=2,
            )
        )
    else:
        print(f"Paired-restore evidence: {evidence_path}")
        print(f"  runtime detected: {result.runtime}")
        print(f"  gate status claim: {result.gate_status}")
        if result.ok:
            print("[PASS] all required fields present; no claim exceeds observed steps")
            print("[PASS] no secret-shaped fields or values detected")
            print("[PASS] gate closure consistent with runtime and observed steps")
        else:
            for problem in result.problems:
                print(f"[FAIL] {problem}")
        print("RESULT: " + ("PASS" if result.ok else "FAIL"))
        print(
            "GATE: "
            + ("unverified" if not result.ok else ("closed" if result.gate_status == "closed" else "open"))
        )
    return 0 if result.ok else 1


if __name__ == "__main__":
    sys.exit(main())
