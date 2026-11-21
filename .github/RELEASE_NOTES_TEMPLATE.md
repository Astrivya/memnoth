<!--
  The CHANGELOG is the source of truth for what changed in a release. This
  template is for the GitHub release body only and must not contradict it.
-->

## Highlights

<!-- Up to 3 bullets: the user-visible reasons to upgrade. -->

- 
- 
- 

## What's new

<!-- One bullet per user-visible addition, matching the CHANGELOG. -->

- 

## What's fixed

<!-- One bullet per user-visible fix, matching the CHANGELOG. -->

- 

## Breaking changes

<!-- Write "None" or list every breaking change; never leave this section blank. -->

None

## Known issues

<!-- Link to docs/operations/known-issues.md; never claim a clean bill of health. -->

See [known issues](../../docs/operations/known-issues.md).

## Upgrade notes

<!-- Install/update commands, signature status, and a rollback path. -->

- Install / update: `curl -fsSL https://memnoth.astrivya.ai/install | bash` (Linux, macOS) or `irm https://memnoth.astrivya.ai/install.ps1 | iex` (Windows PowerShell).
- Signature status: state signed / unsigned and whether `mn update` will self-update.
- Rollback: pin the previous tag and reinstall with the same one-liner.

## What's next

<!-- Link to docs/operations/whats-next.md. -->

See [what's next](../../docs/operations/whats-next.md).

## How to verify

<!-- Copyable commands a user can paste to confirm the release is healthy. -->

```sh
memnoth --version
memnoth doctor
curl -fsSL https://memnoth.astrivya.ai/install | bash   # Linux, macOS
irm https://memnoth.astrivya.ai/install.ps1 | iex       # Windows (PowerShell)
```
