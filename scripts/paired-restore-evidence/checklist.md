# Paired production upload → purge → restore → recall evidence checklist (M2 / P22-005)

This directory is a **record template**, not a test harness. It does not upload,
purge, or restore anything against production, and does not close P22-005 by
itself. A human operator performs the observation on a disposable production
project pair and fills `evidence-template.json`. `verify-evidence.py` then checks
that the record is complete, consistent, and no stronger than what was observed.

Gate policy (from `plans/22-context-platform.md` P22-005,
`docs/operations/known-issues.md` §2–3, and the D33 evidence boundary in
`docs/product/context-platform-strategy-2026-11.md`): unit tests, CI jobs,
docs, and worker status reports are **not** the live restore gate. A unit test or
CI run that fills this file must leave the gate `open`; the verifier exits
non-zero if `gate.status_claim` is `closed` in either context.

## Files

| File | Purpose |
|---|---|
| `evidence-template.json` | Blank record. Every `<FILL: …>` value is a placeholder. Copy it; do not edit it in place. |
| `verify-evidence.py` | Stdlib-only checker. No network, no secrets, no writes, exit 0/1. |
| `checklist.md` | This guide. |

## Hard rules

1. **Placeholders only in the template.** Never put a real result, digest, or ID
   into `evidence-template.json`. Fill a copy.
2. **No credentials or secrets in the evidence file or linked transcripts.**
   Never record the recovery phrase, a private/age key, an API token, a session
   cookie, a SAS/connection string, a password, or an Authorization header.
   Record labels, key IDs, redacted HTTP status codes, and references to a
   separately protected transcript. The verifier rejects secret-bearing field
   names and secret-shaped values.
3. **No claim beyond the observed steps.** Every claim must name step IDs that
   are also in `observed_steps`; every step marked observed must appear in that
   list and satisfy its outcome check. An inferred or assumed claim is not
   accepted.
4. **Production observation, not a mock.** `cloud_base_url` must be the real
   HTTPS origin. Scratch/Postgres-only rehearsals and the read-only
   offline dry-run harnesses do not satisfy these steps.
5. **CI and unit tests cannot close the gate.** The verifier detects CI and test
   environments (for example `CI`, `CIRCLECI`, `GITHUB_ACTIONS`,
   `PYTEST_CURRENT_TEST`) and refuses a `closed` claim from them.

## Operator inputs required before starting

Collect these; the verifier requires all of them to be filled:

- **Operator identity:** full name, work email, role/authority for the
  observation (`context.operator.*`).
- **Observation date:** UTC timestamp in `YYYY-MM-DDTHH:MM:SSZ`
  (`context.observed_on_utc`).
- **Environment:** production HTTPS origin, the disposable production org ID
  and project ID, the `mn` version/revision used, the deployed Cloud revision
  from `/dist/deployed-cloud.json`, and the identifier of the stopped recovery
  stack (`context.environment.*`).
- **Evidence references:** for each step, a redacted transcript/ticket reference
  (`observations.<step>.evidence_ref`). Keep the transcript itself in the
  operator's protected store; this file holds only the pointer.
- **Digests at upload and server side:** two SHA-256 values (64 lowercase hex
  digits) for the disposable snapshot's ciphertext blob
  (`server_blob_inspection.upload_digest` / `.server_digest`).
- **Refusal statuses:** the observed HTTP status or error code for the wrong-key
  probe, wrong-project probe, and tamper probe (`*.wrong_key_status`,
  `*.wrong_project_status`, `*.observed_error`).
- **Journal references:** a read-only purge-journal object reference and the
  trusted recovery-manifest reference used as the replay watermark
  (`purge_replay_tombstone.journal_reference` / `.watermark_source`). Use labels
  only — no SAS, account key, or blob URL with a signed query string.
- **Staging reference:** the private same-filesystem staging location used for
  the restore (`staged_restore_stopped_stack.staging_path_reference`).
- **Checkout reference:** the clean checkout used for recall and a redacted
  recall query (`recall_clean_checkout.checkout_reference` / `.query_reference`).
- **Secrets attestation:** an affirmative `secrets_attestation.no_credentials_recorded = true`
  and a note explaining how secrets were kept out
  (`secrets_attestation.handling_note`).

## Observation steps (operator performs, then records)

| Step ID (`observations.*`) | What the operator must observe | Required recorded outcome |
|---|---|---|
| `server_blob_inspection` | After the production upload, inspect the stored blob **on the server side** with the configured storage tooling. Compare its digest with the upload digest, and confirm the plaintext is not readable server-side. | digest algorithm `sha256`; upload and server digests equal; `digest_match: true`; `plaintext_result: "unreadable"`. |
| `wrong_key_wrong_project_refusal` | Attempt a decrypt/read with a deliberately wrong key, and a request against a different project using the disposable identity. Both must be refused. | `wrong_key_result: "refused"`, `wrong_project_result: "refused"`, both statuses filled. |
| `tamper_refusal` | Alter a byte of the disposable ciphertext in a controlled copy and attempt verification/restore. Verification must refuse it. | `verification_result: "refused"`, tamper vector and observed error filled. |
| `purge_replay_tombstone` | After a purge, replay the journal against the staged restore using the watermark from the trusted manifest, and inspect the purged project's state. | `tombstone_result: "tombstoned"`; journal and watermark references filled. |
| `staged_restore_stopped_stack` | With Cloud and every external MCP writer stopped, verify both archive digests and stage the restore into private scratch before changing the stack. | `stack_state_before: "stopped"`, `digest_verification_result: "verified"`, `restore_result: "restored"`, fence and staging references filled. |
| `recall_clean_checkout` | From a clean checkout (no local graph or synced state carried over), ask a graph question and observe the restored content come back. | `checkout_state: "clean"`, `recall_result: "recalled"`, checkout and query references filled. |

Record each performed step ID in `observed_steps` and set its
`observations.<step>.observed` to `true`. Leave unperformed steps `false`; their
detail fields may stay as placeholders. Do not mark a step observed and leave a
placeholder or an unverified outcome in it — the verifier rejects both.

## Claims and gate

Add one or more entries to `claims`. Each needs a stable `id`, a concise
`statement`, `strength: "observed"`, and `supporting_observation_ids` containing
only IDs from `observed_steps`.

To leave the gate open (the normal state while steps are still being gathered):

- `gate.status_claim: "open"`, `gate.closure_requested: false`,
  `gate.closure_basis_observation_ids: []`.
- Still fill `gate.closure_note` with the reason the gate stays open.

To request closure, and only after **all six** steps were observed by a human in
an operator-manual session:

- `gate.status_claim: "closed"`, `gate.closure_requested: true`.
- `gate.closure_basis_observation_ids` must be exactly:
  `server_blob_inspection`, `wrong_key_wrong_project_refusal`, `tamper_refusal`,
  `purge_replay_tombstone`, `staged_restore_stopped_stack`,
  `recall_clean_checkout` (no missing or extra IDs).
- `gate.closure_note` explains the basis.

Closing the gate in the plan (`plans/22-context-platform.md` P22-005) and
updating `docs/operations/known-issues.md` §2–3 remain separate human actions;
a passing verification does not perform them.

## Verify locally

From the `CLI/` repository root (Python 3, standard library only):

```bash
cp scripts/paired-restore-evidence/evidence-template.json scripts/paired-restore-evidence/evidence.json
# fill evidence.json with the operator inputs above
python3 scripts/paired-restore-evidence/verify-evidence.py scripts/paired-restore-evidence/evidence.json
python3 scripts/paired-restore-evidence/verify-evidence.py scripts/paired-restore-evidence/evidence.json --json
python3 scripts/paired-restore-evidence/verify-evidence.py --self-test
```

Exit 0 means the record is complete and internally consistent; exit 1 lists every
problem. The `--self-test` cases use synthetic fixtures only and prove the
negative cases (unfilled placeholder, missing field, secret-shaped value,
overclaim, readable plaintext, and CI/unit-test gate-closure refusal).

Do not commit a filled `evidence.json` if it contains references or metadata the
operator chose to keep out of the public repository; keep the filled record in
the operator's protected store and pass its path to the verifier.
