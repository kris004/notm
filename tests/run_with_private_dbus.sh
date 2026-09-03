#!/bin/sh

# Run a command on a private session bus below an explicit disposable root.

set -eu

if [ "$#" -eq 0 ]; then
  printf 'usage: %s COMMAND [ARG ...]\n' "$0" >&2
  exit 64
fi
if [ -z "${NOTM_TEST_DISPOSABLE_ROOT:-}" ]; then
  printf '%s\n' 'error: NOTM_TEST_DISPOSABLE_ROOT is required' >&2
  exit 73
fi
case $NOTM_TEST_DISPOSABLE_ROOT in
  /*) ;;
  *)
    printf 'error: NOTM_TEST_DISPOSABLE_ROOT must be absolute: %s\n' \
      "$NOTM_TEST_DISPOSABLE_ROOT" >&2
    exit 73
    ;;
esac
if [ ! -d "$NOTM_TEST_DISPOSABLE_ROOT" ] || [ -L "$NOTM_TEST_DISPOSABLE_ROOT" ]; then
  printf 'error: NOTM_TEST_DISPOSABLE_ROOT is not a real directory: %s\n' \
    "$NOTM_TEST_DISPOSABLE_ROOT" >&2
  exit 73
fi
command -v dbus-daemon >/dev/null 2>&1 || {
  printf '%s\n' 'error: private session bus requires dbus-daemon' >&2
  exit 69
}

DISPOSABLE_ROOT=$(cd -- "$NOTM_TEST_DISPOSABLE_ROOT" && pwd -P)
readonly DISPOSABLE_ROOT
WORK_ROOT="$DISPOSABLE_ROOT/private-dbus"
readonly WORK_ROOT
if [ -e "$WORK_ROOT" ] || [ -L "$WORK_ROOT" ]; then
  printf 'error: private D-Bus work directory already exists: %s\n' \
    "$WORK_ROOT" >&2
  exit 73
fi
mkdir -m 700 -- "$WORK_ROOT"
BUS_SOCKET="$WORK_ROOT/session-bus"
readonly BUS_SOCKET
BUS_LOG="$WORK_ROOT/dbus.log"
readonly BUS_LOG
BUS_PID=

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  if [ -n "$BUS_PID" ]; then
    kill "$BUS_PID" 2>/dev/null || true
    wait "$BUS_PID" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$BUS_LOG" ]; then
    printf '%s\n' '--- private D-Bus log ---' >&2
    cat "$BUS_LOG" >&2
  fi
  rm -rf -- "$WORK_ROOT"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

dbus-daemon \
  --session \
  --nofork \
  --nopidfile \
  --address="unix:path=$BUS_SOCKET" \
  >"$BUS_LOG" 2>&1 &
BUS_PID=$!

attempt=0
while [ "$attempt" -lt 400 ]; do
  if [ -S "$BUS_SOCKET" ]; then
    break
  fi
  if ! kill -0 "$BUS_PID" 2>/dev/null; then
    wait "$BUS_PID" 2>/dev/null || true
    printf '%s\n' 'error: private session bus exited during startup' >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 0.025
done
if [ ! -S "$BUS_SOCKET" ]; then
  printf '%s\n' 'error: private session bus did not create its socket' >&2
  exit 1
fi

DBUS_SESSION_BUS_ADDRESS="unix:path=$BUS_SOCKET" \
  NOTM_TEST_DISPOSABLE_ROOT="$DISPOSABLE_ROOT" \
  "$@"
