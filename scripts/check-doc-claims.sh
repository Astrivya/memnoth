#!/usr/bin/env bash
# check-doc-claims.sh — grep-based claim fence for shipped copy.
#
# This gate fails when copy that a customer can read makes a claim the project
# cannot stand behind: "team memory" as if it shipped, ranking superlatives,
# exclusivity ("nobody does this"), cloud-authenticity claims ("signed by
# Memnoth Cloud", "attested"), a derived diagnostic dressed up as an official
# benchmark score, or hosted semantic search over ciphertext as a positive
# capability. Those claims are either false today or need a gate transcript
# that does not exist; the research docs already record why each one is wrong.
#
# It is deliberately a grep gate, not a prose parser. Every hit is reported as
# PATH:LINE with the claim name and the source line. Two things suppress a hit,
# and only these two:
#
#   1. An accurate negation on the same line. "Not team memory", "no official
#      benchmark score", "hosted semantic search over ciphertext is impossible
#      by design", "nobody does this is false" are all allowed. So are precise
#      minisign statements ("minisign-signed release", "signed with the
#      repository's minisign key"): they never match the cloud-signing claim.
#      The claim phrase itself is removed before the negation words are looked
#      for, so "No.1" does not accidentally negate itself via the word "No".
#   2. An explicit allowlist entry keyed by PATH:LINE with a written reason.
#      This is for honest historical/research mentions that carry no negation
#      word (e.g. a gap list that names "team memory" as a gap). See the
#      ALLOWLIST section below.
#
# SCOPE (all four are scanned; anything else is ignored):
#   docs/                       workspace root docs: *.md *.mdx *.markdown *.txt
#   web marketing copy          src/components/memnoth/**, four route files,
#                               src/styles/marketing.css, content/docs/**/*.mdx,
#                               public/SKILL.md  (see WEB_GLOBS)
#   CLI/CHANGELOG.md            the changelog only
#   CLI/contract/**             every generated/manual contract file
#
# Usage:
#   bash scripts/check-doc-claims.sh [--verbose] [--audit-allowlist]
#                                    [--docs DIR] [--web DIR] [--cli-root DIR]
#                                    [--allowlist FILE] [--root DIR]
#   bash scripts/check-doc-claims.sh --self-test
#   DOC_CLAIM_ALLOWLIST=path/to/file bash scripts/check-doc-claims.sh
#
# External allowlist file format: one entry per line, KEY<TAB>REASON. Blank
# lines and lines starting with '#' are ignored. Keys are <scope>/<relpath>:<line>
# where <scope> is docs, web or CLI (contract files are CLI/contract/...).
# Example:
#   docs/research/audit-and-strategy-2026-11-21.md:86<TAB>Gap list, not a claim.
#
# Exit: 0 when no unnegated, unallowlisted hit remains; 1 when one does;
#       2 on bad usage or a missing external allowlist file.
#
# Self-test builds synthetic fixtures in a temp dir (never in the repo) and
# checks both positive and negative cases; it exits 0 only when every case
# behaves as expected. No secrets, no network, no file writes outside temp.

set -uo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
CLI_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
WORKSPACE_ROOT="$(cd -- "$CLI_ROOT/.." && pwd -P)"

DOCS_ROOT="$WORKSPACE_ROOT/docs"
WEB_ROOT="$WORKSPACE_ROOT/web"
CLI_CLAIM_ROOT="$CLI_ROOT"

QUIET=0
VERBOSE=0
AUDIT_ALLOWLIST=0
SELF_TEST=0
EXTRA_ALLOWLIST="${DOC_CLAIM_ALLOWLIST:-}"
G_LOG_FILE=""

# ---------------------------------------------------------------------------
# Claim patterns. ERE, lowercase, with portable non-alnum word boundaries
# (GNU grep -E \b is not portable to BSD grep; [^[:alnum:]_] is).
# ---------------------------------------------------------------------------
CLAIMS=(
  'team-memory|(^|[^[:alnum:]_])team[[:space:]_-]+memory([^[:alnum:]_]|$)'
  'no-1|(^|[^[:alnum:]_])no\.?[[:space:]]*1([^[:alnum:]_]|$)'
  'most-advanced|(^|[^[:alnum:]_])most[[:space:]]+advanced([^[:alnum:]_]|$)'
  'nobody-does-this|(^|[^[:alnum:]_])nobody[[:space:]]+does[[:space:]]+this([^[:alnum:]_]|$)'
  'signed-by-cloud|(^|[^[:alnum:]_])signed[[:space:]]+by[[:space:]]+(the[[:space:]]+)?memnoth[[:space:]]+cloud([^[:alnum:]_]|$)'
  'attested|(^|[^[:alnum:]_])attested([^[:alnum:]_]|$)'
  'official-benchmark|(^|[^[:alnum:]_])official[[:space:]]+benchmarks?([^[:alnum:]_]|$)'
  'hosted-search-ciphertext|(^|[^[:alnum:]_])hosted[[:space:]]+semantic[[:space:]]+search[[:space:]]+over[[:space:]]+ciphertext([^[:alnum:]_]|$)'
)

NEGATION_RE='(^|[^[:alnum:]_])(not|no|none|nor|never|without|unsigned|unverified|impossible|false|isn.t|aren.t|doesn.t|don.t|didn.t|wasn.t|weren.t|can.t|cannot|won.t|do[[:space:]]+not|does[[:space:]]+not|did[[:space:]]+not|is[[:space:]]+not|are[[:space:]]+not|claim[[:space:]_-]?fence|fenced|caveat|diagnostic|dry[[:space:]_-]?run|remains?|still|partial|gaps?|removed?|wrong|misleading|no[[:space:]]+longer|must[[:space:]]+go|do[[:space:]]+not[[:space:]]+call|never[[:space:]]+call)([^[:alnum:]_]|$)'

# ---------------------------------------------------------------------------
# ALLOWLIST — explicit PATH:LINE -> reason. Each entry is an honest mention of
# a forbidden phrase that is not a shipped claim (a gap list, a strategy doc,
# a trap to avoid). The reason must say why it is safe. Entries are exact
# paths + line numbers: when the cited line moves, re-check it and update the
# entry. `bash scripts/check-doc-claims.sh --audit-allowlist` reports entries
# that no longer match any hit, so the list cannot quietly rot.
# ---------------------------------------------------------------------------
declare -A ALLOW=()
allow() { ALLOW["$1"]="$2"; }

# Research: competitor/gap tables and strategy docs name these phrases as
# things to refute, avoid, or cost out — not as shipped product claims.
# "CLOSED" on the deep-research line means the exclusivity claim was falsified;
# the audit doc's roadmap explicitly says "do NOT call it team memory".
allow "docs/research/deep-research-2026-11-21.md:72" "Research table: 'nobody shares team memory' marked CLOSED/falsified, not claimed."
allow "docs/research/audit-and-strategy-2026-11-21.md:86" "Highest-leverage moves: 'ship MOAT-6 team memory' is a proposal, gate open."
allow "docs/research/audit-and-strategy-2026-11-21.md:93" "Research brief title quotes the challenger's 'Most Advanced' framing; not a shipped claim."
allow "docs/research/audit-and-strategy-2026-11-21.md:205" "Trap list: 'nobody does this' named as a claim to avoid."
allow "docs/research/audit-and-strategy-2026-11-21.md:303" "Roadmap table names team memory with evidence as unbuilt (M5/M8)."
allow "docs/research/audit-and-strategy-2026-11-21.md:310" "Effort estimate for an unbuilt 'team-memory plane' (W6/W8/W9/W11); not a claim."
allow "docs/research/codebase-deep-reference.md:97" "Reference summary lists '+3 team moats (attributed team memory)' as unbuilt work."

# Benchmark docs: recommendations to reach a real benchmark, not a claim that
# a derived diagnostic already has an official score.
allow "docs/product/benchmark-priorities-and-roi.md:58" "Recommendation to submit to official benchmarks after reproduction; not a result."
allow "docs/product/benchmark-priorities-and-roi.md:123" "Caveat: adaptation must be reviewed before its output is called official."

# The command atlas maps each claim to the gate that would allow it. M2/M5/M8
# are open, so the row is a fence, not a shipped claim.
allow "docs/cli/tui-command-atlas.md:995" "Claim-to-gate table: 'Team memory' fenced behind open M2 + M5 + M8."

# Handoff table is a historical status row (W11 team memory = 0 done), not a
# shipped claim.
allow "docs/operations/handoff-2026-11-12.md:82" "Historical handoff table: W11 team memory has 0/15 done."

# ---------------------------------------------------------------------------
# File discovery
# ---------------------------------------------------------------------------
list_ext_files() {
  local root="$1" exts="$2"
  [ -d "$root" ] || return 0
  (
    cd -- "$root" || exit 0
    find . -type f -print 2>/dev/null | sed 's#^\./##' | while IFS= read -r f; do
      case "/$f/" in
        */node_modules/*|*/.git/*|*/.output/*|*/target/*|*/dist/*|*/.cache/*) continue ;;
      esac
      local e ok=0
      for e in $exts; do
        case "$f" in *."$e") ok=1; break ;; esac
      done
      [ "$ok" -eq 1 ] && printf '%s\n' "$f"
    done
  )
}

# Explicit web marketing/published-copy surface. Add a glob here when a new
# marketing page or landing component is introduced; do not widen to all of
# web/src (shared UI primitives are not marketing copy).
WEB_GLOBS=(
  'src/components/memnoth/**/*.tsx'
  'src/components/memnoth/**/*.ts'
  'src/components/memnoth/**/*.css'
  'src/components/memnoth/**/*.md'
  'src/routes/index.tsx'
  'src/routes/pricing.tsx'
  'src/routes/proof.tsx'
  'src/routes/download.tsx'
  'src/styles/marketing.css'
  'content/docs/**/*.mdx'
  'public/SKILL.md'
)

list_web_files() {
  local root="$1"
  [ -d "$root" ] || return 0
  (
    cd -- "$root" || exit 0
    shopt -s nullglob globstar
    local g f
    for g in "${WEB_GLOBS[@]}"; do
      for f in $g; do
        [ -f "$f" ] && printf '%s\n' "$f"
      done
    done
  ) | sort -u
}

# ---------------------------------------------------------------------------
# Scan engine
# ---------------------------------------------------------------------------
emit() {
  [ "$QUIET" -eq 1 ] || printf '%s\n' "$1"
  [ -n "$G_LOG_FILE" ] && printf '%s\n' "$1" >> "$G_LOG_FILE"
  return 0
}

G_HITS=0
G_NEG=0
G_LIST=0
G_BAD=0
G_FILES=0
G_BAD_KEYS=""
G_BAD_LABELS=""
G_MATCHED=""

evaluate_hit() {
  local label="$1" rel="$2" ln="$3" text="$4" name="$5" regex="$6"
  local key="$label/$rel:$ln"
  G_HITS=$((G_HITS + 1))
  G_MATCHED="$G_MATCHED"$'\n'"$key"

  # Remove the claim phrase, then look for an accurate negation. Stripping
  # first stops "No.1" from negating itself via the word "No". Bash built-ins
  # keep this in-process: spawning tr/sed/grep per hit is far too slow on
  # Windows, where every process start is expensive.
  local lower stripped matched
  lower="${text,,}"
  if [[ "$lower" =~ $regex ]]; then
    matched="${BASH_REMATCH[0]}"
    stripped="${lower//"$matched"/ }"
  else
    stripped="$lower"
  fi
  if [[ "$stripped" =~ $NEGATION_RE ]]; then
    G_NEG=$((G_NEG + 1))
    [ "$VERBOSE" -eq 1 ] && emit "ALLOW negation  $key  [$name]  $text"
    return 0
  fi

  if [ -n "${ALLOW[$key]+set}" ]; then
    G_LIST=$((G_LIST + 1))
    [ "$VERBOSE" -eq 1 ] && emit "ALLOW allowlist $key  [$name]  ${ALLOW[$key]}"
    return 0
  fi

  G_BAD=$((G_BAD + 1))
  G_BAD_KEYS="$G_BAD_KEYS $key"
  G_BAD_LABELS="$G_BAD_LABELS $name"
  emit "FAIL  $key  [$name]  $text"
}

# One combined grep per root, then classify each matched line in-process.
ALL_REGEX=""
for _spec in "${CLAIMS[@]}"; do
  ALL_REGEX+="${_spec#*|}|"
done
ALL_REGEX="${ALL_REGEX%|}"

scan_root() {
  local label="$1" root="$2"; shift 2
  local files=("$@")
  [ -d "$root" ] || return 0
  [ "${#files[@]}" -gt 0 ] || return 0
  local out hit rel rest lineno text lower spec name regex
  out="$( (cd -- "$root" && grep -anHiE -- "$ALL_REGEX" "${files[@]}" 2>/dev/null) || true )"
  while IFS= read -r hit; do
    [ -n "$hit" ] || continue
    rel="${hit%%:*}"
    rest="${hit#*:}"
    lineno="${rest%%:*}"
    text="${rest#*:}"
    lower="${text,,}"
    for spec in "${CLAIMS[@]}"; do
      name="${spec%%|*}"
      regex="${spec#*|}"
      [[ "$lower" =~ $regex ]] || continue
      evaluate_hit "$label" "$rel" "$lineno" "$text" "$name" "$regex"
    done
  done <<< "$out"
}

gate() {
  G_HITS=0; G_NEG=0; G_LIST=0; G_BAD=0
  G_BAD_KEYS=""; G_BAD_LABELS=""; G_MATCHED=""

  local -a docs_files=() web_files=() cli_files=() contract_files=()
  mapfile -t docs_files < <(list_ext_files "$DOCS_ROOT" 'md mdx markdown txt')
  mapfile -t web_files < <(list_web_files "$WEB_ROOT")
  if [ -f "$CLI_CLAIM_ROOT/CHANGELOG.md" ]; then
    cli_files=("CHANGELOG.md")
  fi
  if [ -d "$CLI_CLAIM_ROOT/contract" ]; then
    mapfile -t contract_files < <(
      cd -- "$CLI_CLAIM_ROOT" && find contract -type f -print 2>/dev/null | sed 's#^\./##'
    )
  fi
  G_FILES=$(( ${#docs_files[@]} + ${#web_files[@]} + ${#cli_files[@]} + ${#contract_files[@]} ))

  [ "${#docs_files[@]}" -eq 0     ] || scan_root "docs" "$DOCS_ROOT" "${docs_files[@]}"
  [ "${#web_files[@]}" -eq 0      ] || scan_root "web"  "$WEB_ROOT" "${web_files[@]}"
  [ "${#cli_files[@]}" -eq 0      ] || scan_root "CLI"  "$CLI_CLAIM_ROOT" "${cli_files[@]}"
  [ "${#contract_files[@]}" -eq 0 ] || scan_root "CLI"  "$CLI_CLAIM_ROOT" "${contract_files[@]}"
}

load_allowlist() {
  local f="$1" line key reason
  [ -n "$f" ] || return 0
  if [ ! -f "$f" ]; then
    printf 'allowlist file not found: %s\n' "$f" >&2
    return 1
  fi
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|'#'*) continue ;; esac
    case "$line" in *$'\t'*) : ;; *) continue ;; esac
    key="${line%%$'\t'*}"
    reason="${line#*$'\t'}"
    [ -n "$key" ] && ALLOW["$key"]="$reason"
  done < "$f"
}

audit_allowlist() {
  local key stale=0
  for key in "${!ALLOW[@]}"; do
    if ! printf '%s\n' "$G_MATCHED" | grep -Fqx -- "$key"; then
      printf 'STALE allowlist entry  %s  (%s)\n' "$key" "${ALLOW[$key]}" >&2
      stale=$((stale + 1))
    fi
  done
  return "$stale"
}

# ---------------------------------------------------------------------------
# Self-test: synthetic fixtures in a temp dir, never in the repo.
# ---------------------------------------------------------------------------
ST_TOTAL=0
ST_PASS=0

st_check() {
  local label="$1" ok="$2" detail="$3"
  ST_TOTAL=$((ST_TOTAL + 1))
  if [ "$ok" -eq 1 ]; then
    ST_PASS=$((ST_PASS + 1))
    printf 'PASS  %s  %s\n' "$label" "$detail"
  else
    printf 'FAIL  %s  %s\n' "$label" "$detail" >&2
  fi
}

CASE_ROOT=""
make_case_root() {
  # Bash expands all names in one `local` statement before assigning any of
  # them: `local base="$1" r="$base/$name"` would yield r="/". Assign in
  # separate statements. Globals are set here because this function must NOT
  # run in a command substitution — a subshell would discard DOCS_ROOT etc.
  local base="$1"
  local name="$2"
  local r="$base/$name"
  mkdir -p "$r/docs/_fixture" \
           "$r/web/src/components/memnoth/landing" \
           "$r/web/src/components/ui" \
           "$r/web/src/routes" "$r/web/src/styles" \
           "$r/web/content/docs" "$r/web/public" \
           "$r/CLI/contract"
  CASE_ROOT="$r"
  DOCS_ROOT="$r/docs"
  WEB_ROOT="$r/web"
  CLI_CLAIM_ROOT="$r/CLI"
}

reset_case() {
  ALLOW=()
  QUIET=1
  VERBOSE=0
  AUDIT_ALLOWLIST=0
  G_LOG_FILE=""
  G_HITS=0; G_NEG=0; G_LIST=0; G_BAD=0; G_FILES=0
  G_BAD_KEYS=""; G_BAD_LABELS=""; G_MATCHED=""
}

ALL_EIGHT_BAD=(
  'Team memory is now live.'
  'We are the No.1 platform.'
  'The most advanced agent memory.'
  'Nobody does this.'
  'Releases are signed by Memnoth Cloud.'
  'Builds are attested.'
  'Our official benchmark score proves it.'
  'We provide hosted semantic search over ciphertext.'
)
ALL_EIGHT_GOOD=(
  'Not team memory: no team surface ships here.'
  'We are not No.1 in any ranking.'
  'This is not the most advanced engine; it is early.'
  'The claim that nobody does this is false.'
  'Not signed by Memnoth Cloud; a minisign-signed release only.'
  'This build is not attested.'
  'Derived diagnostics are not an official benchmark score.'
  'Hosted semantic search over ciphertext is impossible by design.'
)

self_test() {
  local saved_docs="$DOCS_ROOT" saved_web="$WEB_ROOT" saved_cli="$CLI_CLAIM_ROOT"
  local -A saved_allow=()
  local k
  for k in "${!ALLOW[@]}"; do saved_allow["$k"]="${ALLOW[$k]}"; done

  local base
  base="$(mktemp -d "${TMPDIR:-/tmp}/mn-doc-claims.XXXXXX")" || {
    printf 'cannot create temp dir\n' >&2
    return 1
  }
  local line
  printf 'Self-test (synthetic fixtures in %s)\n' "$base"

  # 1. Clean copy: accurate negations and precise minisign statements pass.
  make_case_root "$base" 1
  for line in "${ALL_EIGHT_GOOD[@]}"; do printf '%s\n' "$line" >> "$CASE_ROOT/docs/_fixture/clean.md"; done
  printf '%s\n' 'A minisign-signed release, signed with the repository minisign key.' \
    >> "$CASE_ROOT/web/src/components/memnoth/landing/_fixture.tsx"
  reset_case; gate
  st_check "negations + minisign pass" "$([ "$G_BAD" -eq 0 ] && echo 1 || echo 0)" \
    "hits=$G_HITS negated=$G_NEG violations=$G_BAD"

  # 2. All eight affirmative claims fail, one per label.
  make_case_root "$base" 2
  for line in "${ALL_EIGHT_BAD[@]}"; do printf '%s\n' "$line" >> "$CASE_ROOT/docs/_fixture/claims.md"; done
  reset_case; gate
  local missing=""
  for line in team-memory no-1 most-advanced nobody-does-this signed-by-cloud attested official-benchmark hosted-search-ciphertext; do
    case "$G_BAD_LABELS " in *" $line "*) : ;; *) missing="$missing $line" ;; esac
  done
  st_check "eight affirmative claims fail" \
    "$([ "$G_BAD" -eq 8 ] && [ -z "$missing" ] && echo 1 || echo 0)" \
    "violations=$G_BAD missing=${missing:-none}"

  # 3. Web marketing copy, published docs, changelog and contract in scope.
  make_case_root "$base" 3
  printf '%s\n' 'Team memory ships today.' >> "$CASE_ROOT/web/src/components/memnoth/landing/_fixture.tsx"
  printf '%s\n' 'We are No.1.' >> "$CASE_ROOT/web/src/routes/index.tsx"
  printf '%s\n' 'content: "the most advanced";' >> "$CASE_ROOT/web/src/styles/marketing.css"
  printf '%s\n' 'Releases are signed by Memnoth Cloud.' >> "$CASE_ROOT/web/content/docs/_fixture.mdx"
  printf '%s\n' 'Builds are attested by the service.' >> "$CASE_ROOT/web/public/SKILL.md"
  printf '%s\n' 'This is an official benchmark score.' >> "$CASE_ROOT/CLI/contract/memnoth.1"
  printf '%s\n' 'We offer hosted semantic search over ciphertext.' >> "$CASE_ROOT/CLI/CHANGELOG.md"
  reset_case; gate
  st_check "all four scopes are scanned" "$([ "$G_BAD" -eq 7 ] && echo 1 || echo 0)" \
    "violations=$G_BAD (expected 7)"

  # 4. Out-of-scope files are ignored.
  make_case_root "$base" 4
  printf '%s\n' 'Team memory is live.' >> "$CASE_ROOT/web/src/components/ui/_fixture.tsx"
  printf '%s\n' 'We are No.1.' >> "$CASE_ROOT/web/src/routes/help.tsx"
  printf '%s\n' 'Builds are attested.' >> "$CASE_ROOT/CLI/src_fixture.rs"
  reset_case; gate
  st_check "out-of-scope files ignored" "$([ "$G_BAD" -eq 0 ] && echo 1 || echo 0)" \
    "violations=$G_BAD (expected 0)"

  # 5. Allowlist suppresses a flagged line and records the reason.
  make_case_root "$base" 5
  printf '%s\n' 'Team memory is now live.' >> "$CASE_ROOT/docs/_fixture/claims.md"
  reset_case
  ALLOW["docs/_fixture/claims.md:1"]="fixture reason: historical quote, not a claim"
  VERBOSE=1
  G_LOG_FILE="$base/case5.log"; gate
  local reason_ok=0
  if grep -Fq 'fixture reason: historical quote, not a claim' "$base/case5.log" 2>/dev/null \
     && [ "$G_BAD" -eq 0 ] && [ "$G_LIST" -eq 1 ]; then reason_ok=1; fi
  st_check "allowlist suppresses with reason" "$reason_ok" \
    "violations=$G_BAD allowed=$G_LIST"

  # 6. A moved line is no longer allowlisted and fails again.
  make_case_root "$base" 6
  printf '%s\n' 'safe line' >> "$CASE_ROOT/docs/_fixture/claims.md"
  printf '%s\n' 'Team memory is now live.' >> "$CASE_ROOT/docs/_fixture/claims.md"
  reset_case
  ALLOW["docs/_fixture/claims.md:1"]="fixture reason: historical quote, not a claim"
  gate
  st_check "allowlist key is line-exact" "$([ "$G_BAD" -eq 1 ] && echo 1 || echo 0)" \
    "violations=$G_BAD (expected 1)"

  # 7. --audit-allowlist flags a stale entry that matched no hit.
  make_case_root "$base" 7
  printf '%s\n' 'Nothing forbidden here.' >> "$CASE_ROOT/docs/_fixture/clean.md"
  reset_case
  ALLOW["docs/_fixture/gone.md:999"]="fixture reason: file was deleted"
  gate
  local audit_out audit_rc
  audit_out="$(audit_allowlist 2>&1)"; audit_rc=$?
  st_check "allowlist audit finds stale entry" \
    "$([ "$audit_rc" -eq 1 ] && printf '%s' "$audit_out" | grep -Fq 'gone.md:999' && echo 1 || echo 0)" \
    "audit_rc=$audit_rc stale_reported=$(printf '%s' "$audit_out" | grep -c STALE)"

  rm -rf "$base"

  DOCS_ROOT="$saved_docs"; WEB_ROOT="$saved_web"; CLI_CLAIM_ROOT="$saved_cli"
  ALLOW=()
  for k in "${!saved_allow[@]}"; do ALLOW["$k"]="${saved_allow[$k]}"; done

  printf 'Self-test: %d/%d cases passed\n' "$ST_PASS" "$ST_TOTAL"
  [ "$ST_PASS" -eq "$ST_TOTAL" ]
}

usage() {
  sed -n '2,45p' "${BASH_SOURCE[0]}"
}

# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------
while [ "$#" -gt 0 ]; do
  case "$1" in
    --self-test)        SELF_TEST=1; shift ;;
    --verbose)          VERBOSE=1; shift ;;
    --quiet)            QUIET=1; shift ;;
    --audit-allowlist)  AUDIT_ALLOWLIST=1; shift ;;
    --docs)             DOCS_ROOT="${2:?--docs needs a directory}"; shift 2 ;;
    --web)              WEB_ROOT="${2:?--web needs a directory}"; shift 2 ;;
    --cli-root)         CLI_CLAIM_ROOT="${2:?--cli-root needs a directory}"; shift 2 ;;
    --allowlist)        EXTRA_ALLOWLIST="${2:?--allowlist needs a file}"; shift 2 ;;
    --root)
      WORKSPACE_ROOT="${2:?--root needs a directory}"
      DOCS_ROOT="$WORKSPACE_ROOT/docs"
      WEB_ROOT="$WORKSPACE_ROOT/web"
      shift 2
      ;;
    -h|--help)          usage; exit 0 ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ "$SELF_TEST" -eq 1 ]; then
  self_test
  exit $?
fi

if ! load_allowlist "$EXTRA_ALLOWLIST"; then
  exit 2
fi

gate

printf 'check-doc-claims: scanned %d files (docs/, web copy, CLI/CHANGELOG.md, CLI/contract/)\n' "$G_FILES"
printf '  matched=%d  allowed(negation)=%d  allowed(allowlist)=%d  violations=%d\n' \
  "$G_HITS" "$G_NEG" "$G_LIST" "$G_BAD"

RC=0
[ "$G_BAD" -eq 0 ] || RC=1

if [ "$AUDIT_ALLOWLIST" -eq 1 ]; then
  if ! audit_allowlist; then
    printf '  allowlist audit: stale entries found\n' >&2
    RC=1
  else
    printf '  allowlist audit: all %d entries matched a hit\n' "${#ALLOW[@]}"
  fi
fi

if [ "$RC" -eq 0 ]; then
  printf '  Verdict: PASS\n'
else
  printf '  Verdict: FAIL  (%d unallowlisted claim(s):%s)\n' "$G_BAD" "$G_BAD_KEYS"
fi
exit "$RC"
