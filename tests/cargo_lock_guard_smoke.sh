#!/bin/sh

# Deliberately mutate a disposable Cargo.lock and prove the CI guard rejects it.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-cargo-lock-guard.XXXXXX")
readonly WORK_ROOT

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

lockfile="$WORK_ROOT/Cargo.lock"
cp "$PROJECT_ROOT/Cargo.lock" "$lockfile"
expected_sha256=$(sha256sum "$lockfile" | awk '{ print $1 }')

"$PROJECT_ROOT/packaging/verify-cargo-lock.sh" \
  "$lockfile" "$expected_sha256" >/dev/null
printf '\n# deliberate negative-test mutation\n' >> "$lockfile"

if "$PROJECT_ROOT/packaging/verify-cargo-lock.sh" \
  "$lockfile" "$expected_sha256" >"$WORK_ROOT/stdout" 2>"$WORK_ROOT/stderr"; then
  printf '%s\n' 'Cargo.lock guard accepted a deliberate mutation' >&2
  exit 1
fi
grep -F 'Cargo.lock changed: expected SHA-256' "$WORK_ROOT/stderr" >/dev/null

printf '%s\n' 'cargo_lock_guard_smoke ok'
