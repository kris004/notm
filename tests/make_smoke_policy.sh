#!/bin/sh

# Prove the default Make smoke target cannot reach live mailbox/send commands.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-make-smoke.XXXXXX")
readonly WORK_ROOT
CARGO_LOG="$WORK_ROOT/cargo.log"
readonly CARGO_LOG
FAKE_CARGO="$WORK_ROOT/cargo"
readonly FAKE_CARGO

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

cat >"$FAKE_CARGO" <<'EOF'
#!/bin/sh
set -eu
: "${NOTM_SMOKE_CARGO_LOG:?}"
printf '%s\n' "$*" >>"$NOTM_SMOKE_CARGO_LOG"
EOF
chmod 755 "$FAKE_CARGO"

NOTM_SMOKE_CARGO_LOG="$CARGO_LOG" \
  make --no-print-directory -s -C "$PROJECT_ROOT" CARGO="$FAKE_CARGO" smoke

expected='run --locked -p notm-app -- fixture-smoke'
if ! test "$(wc -l <"$CARGO_LOG")" -eq 1 ||
  ! grep -Fx "$expected" "$CARGO_LOG" >/dev/null; then
  printf '%s\n' 'make smoke invoked unexpected Cargo commands:' >&2
  cat "$CARGO_LOG" >&2
  exit 1
fi

if grep -E 'live-readonly-smoke|live-self-send|probe-send' "$CARGO_LOG" >/dev/null; then
  printf '%s\n' 'make smoke attempted to use a live or external service' >&2
  cat "$CARGO_LOG" >&2
  exit 1
fi

readonly_recipe=$(make --no-print-directory -s -n -C "$PROJECT_ROOT" \
  CARGO=cargo smoke-live-readonly)
send_recipe=$(make --no-print-directory -s -n -C "$PROJECT_ROOT" \
  CARGO=cargo smoke-live-send)
test "$readonly_recipe" = \
  'cargo run --locked -p notm-app -- live-readonly-smoke'
test "$send_recipe" = 'cargo run --locked -p notm-app -- live-self-send'

printf '%s\n' 'Make smoke policy passed: default is fixture-only; live targets are explicit'
