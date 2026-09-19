#!/usr/bin/env bash
# Throwaway hybrid storage/recovery probe (probe C, Gate C). NOT production
# code. Runs: cargo build + the foundation unit tests, the single-process
# selfcheck, then the full Gate C matrix/scenario harness (`gate-c`), which
# spawns REAL writer children that die with libc::_exit(70) at exact
# boundaries, power-loss variants that physically truncate files to their
# fsynced checkpoint, and `recover` always in a NEW process (twice, for
# idempotence). The gate-c phase itself is bounded by `timeout 100`; the
# whole recipe is expected to fit the outer `timeout 120 just
# terminal-probe-storage` bound. Success MOVES the entire per-run mktemp
# workdir out of /tmp into target/terminal-probes/storage-evidence/<run-id>/
# workdir (git-ignored), preserving the raw per-point evidence (per-point
# sqlite DBs, segment files, quarantine artifacts, traces) as well as both
# summary.json files and the command transcript with timestamps, tool
# versions and exit statuses; storage-evidence/LATEST points at the newest
# run and no /tmp success residue is left behind. Failure keeps the whole
# workdir in /tmp (summary.json, traces, child stderr) for debugging.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
MANIFEST="$ROOT/probes/terminal/storage/rust/Cargo.toml"
MIGRATIONS="$ROOT/probes/terminal/storage/rust/migrations"
OUT="$ROOT/target/terminal-probes"
EVIDENCE="$OUT/storage-evidence"

WORK=$(mktemp -d /tmp/qingluan-terminal-storage-probe.XXXXXX)
TRANSCRIPT="$WORK/transcript.txt"
STATUS=failure
# One run id shared by the transcript and the evidence directory, so the
# evidence path recorded in the transcript always matches where the workdir
# is preserved.
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"

{
  echo "run_id=$RUN_ID"
  echo "started_utc=$(date -u +%FT%TZ)"
  echo "host=$(uname -srm)"
  echo "cargo=$(cargo --version 2>&1)"
  echo "rustc=$(rustc --version 2>&1)"
  echo "manifest=$MANIFEST"
} > "$TRANSCRIPT"

# Run one step, tee its output to the console and the transcript, and record
# the exit status + wall time. A failing step aborts the recipe (workdir kept).
run_step() {
  local name="$1"; shift
  local start end rc
  start=$(date -u +%s)
  echo "[$name] \$ $*" | tee -a "$TRANSCRIPT"
  rc=0
  "$@" 2>&1 | tee -a "$TRANSCRIPT" || rc=$?
  end=$(date -u +%s)
  echo "[$name] exit=$rc duration=$((end-start))s" | tee -a "$TRANSCRIPT"
  return "$rc"
}

cleanup() {
  # Safe and idempotent: nothing to do when the workdir is already gone
  # (mktemp failed or the trap fired twice), and success never deletes the
  # workdir — it moves it, so a repeated cleanup cannot destroy evidence.
  if [[ ! -e "$WORK" ]]; then
    return 0
  fi
  if [[ "$STATUS" != success ]]; then
    echo "PROBE_FAILED workdir-kept=$WORK" >&2
    return 0
  fi
  local ev="$EVIDENCE/$RUN_ID"
  # Never move into an occupied target (would nest the workdir inside it).
  if [[ -e "$ev/workdir" ]]; then
    echo "PROBE_FAILED evidence-target-exists=$ev/workdir workdir-kept=$WORK" >&2
    return 0
  fi
  {
    echo "finished_utc=$(date -u +%FT%TZ)"
    echo "result=PROBE_OK"
    echo "probe_binary=$OUT/storage-rust/debug/qingluan-terminal-storage-probe"
    echo "evidence_workdir=$ev/workdir"
  } >> "$TRANSCRIPT"
  mkdir -p "$ev"
  # Preserve EVERYTHING the run produced: the whole workdir (per-point
  # sqlite DBs, segments, quarantine artifacts, traces, both summary.json
  # files and the transcript) moves out of /tmp; nothing is left behind in
  # /tmp on success.
  if mv -- "$WORK" "$ev/workdir"; then
    printf '%s\n' "$ev" > "$EVIDENCE/LATEST"
    echo "PROBE_CLEAN evidence=$ev workdir=$ev/workdir"
  else
    echo "PROBE_FAILED evidence-move-failed workdir-kept=$WORK target=$ev/workdir" >&2
  fi
}
trap cleanup EXIT

run_step build env CARGO_TARGET_DIR="$OUT/storage-rust" cargo build --manifest-path "$MANIFEST"
run_step test env CARGO_TARGET_DIR="$OUT/storage-rust" cargo test --manifest-path "$MANIFEST" --quiet

PROBE="$OUT/storage-rust/debug/qingluan-terminal-storage-probe"
[[ -x "$PROBE" ]] || { echo "probe binary missing" >&2; exit 1; }

# Foundation regression (single-process selfcheck), then the Gate C harness.
run_step selfcheck timeout 30 "$PROBE" selfcheck --migrations "$MIGRATIONS" --workdir "$WORK/selfcheck"
run_step gate-c timeout 100 "$PROBE" gate-c --workdir "$WORK/gate-c" --migrations "$MIGRATIONS"

# No writer/recover child may survive the gate.
if pgrep -f "qingluan-terminal-storage-probe (writer|recover)" >/dev/null 2>&1; then
  echo "FAIL: storage writer/recover processes still running" >&2
  pgrep -af "qingluan-terminal-storage-probe (writer|recover)" >&2 || true
  exit 1
fi

STATUS=success
echo "PROBE_OK gate-c matrix+scenarios run_id=$RUN_ID"
