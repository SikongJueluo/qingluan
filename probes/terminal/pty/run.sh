#!/usr/bin/env bash
# Throwaway PTY lifecycle / Stop-barrier probe (probe B, Gate B restructure).
# NOT production code. Runs: cargo build + quota unit tests, a crash phase
# that leaves a live fixture + registry inside the probe cgroup root, then the
# main phase with all lifecycle scenarios.
#
# Cleanup is cgroup-identity based on BOTH success and failure: the probe
# cgroup root (recorded in the workdir) is killed via cgroup.kill and removed
# — never pgrep, never bare pids. Success leaves nothing behind (no
# processes, no cgroups, no temp dir); failure keeps the workdir for
# debugging. See probes/terminal/README.md.
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)
OUT="$ROOT/target/terminal-probes"
MANIFEST="$ROOT/probes/terminal/pty/rust/Cargo.toml"

# Marker variable that must NEVER reach a spawned child (env snapshot check).
export QINGLUAN_PROBE_ONLY_IN_PROBE=1

CARGO_TARGET_DIR="$OUT/rust" cargo build --manifest-path "$MANIFEST"
# Quota state machine unit tests (double release, races, start failure).
CARGO_TARGET_DIR="$OUT/rust" cargo test --manifest-path "$MANIFEST" --quiet

PROBE="$OUT/rust/debug/qingluan-terminal-pty-probe"
FIXTURE="$OUT/rust/debug/pty-fixture"
[[ -x "$PROBE" && -x "$FIXTURE" ]] || { echo "probe binaries missing" >&2; exit 1; }

WORK=$(mktemp -d /tmp/qingluan-terminal-pty-probe.XXXXXX)
STATUS=failure
cleanup() {
  # Kill + remove the probe cgroup root on every exit path. The probe process
  # itself is never inside it, so cgroup.kill here is always safe.
  if [[ -f "$WORK/cgroup-root.json" ]]; then
    "$PROBE" --cleanup "$WORK" || echo "CLEANUP-FAILED (inspect $WORK/cgroup-root.json)" >&2
  else
    echo "CLEANUP nothing recorded (no cgroup root marker)" >&2
  fi
  if [[ "$STATUS" == success ]]; then
    rm -rf -- "$WORK"
    echo "PROBE_CLEAN workdir-removed"
  else
    echo "PROBE_FAILED workdir-kept=$WORK" >&2
  fi
}
trap cleanup EXIT

export QINGLUAN_PTY_FIXTURE_BIN="$FIXTURE"

# Phase 1: crash-spawn. Leaves a live SIGHUP-ignoring fixture inside its
# terminal cgroup plus a registry; the probe process exits right after.
"$PROBE" --phase crash "$WORK"
[[ -s "$WORK/registry.json" ]] || { echo "crash phase left no registry" >&2; exit 1; }

# Phase 2: all scenarios, including recovery of the crash-phase registry.
"$PROBE" --phase main "$WORK"

# Sweep the probe cgroup root now (reclaims the deliberately-live crash
# fixture via cgroup.kill), then verify nothing is left. `--cleanup` is
# idempotent: the trap below re-runs it harmlessly on every exit path.
if [[ -f "$WORK/cgroup-root.json" ]]; then
  "$PROBE" --cleanup "$WORK" || { echo "CLEANUP-FAILED during final sweep" >&2; exit 1; }
  CGROOT=$(sed -n 's/.*"path":"\([^"]*\)".*/\1/p' "$WORK/cgroup-root.json")
  if [[ -n "$CGROOT" && -d "$CGROOT" ]]; then
    echo "FAIL: probe cgroup root still exists: $CGROOT" >&2
    ls -la "$CGROOT" >&2 || true
    exit 1
  fi
fi

STATUS=success
echo "PROBE_OK no-leftover-cgroups no-leftover-processes no-leftover-tempdirs"
