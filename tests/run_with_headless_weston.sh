#!/bin/sh

# Run a command on a private, software-rendered Wayland display.

set -eu

if [ "$#" -eq 0 ]; then
  printf 'usage: %s COMMAND [ARG ...]\n' "$0" >&2
  exit 64
fi

command -v weston >/dev/null 2>&1 || {
  printf 'error: headless Wayland tests require weston in PATH\n' >&2
  exit 69
}

WORK_ROOT=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/notm-weston.XXXXXX")
readonly WORK_ROOT
RUNTIME_DIR="$WORK_ROOT/runtime"
readonly RUNTIME_DIR
WESTON_LOG="$WORK_ROOT/weston.log"
readonly WESTON_LOG
WAYLAND_SOCKET="wayland-notm-$$"
readonly WAYLAND_SOCKET
WESTON_PID=

cleanup() {
  status=$?
  trap - EXIT HUP INT TERM

  if [ -n "$WESTON_PID" ]; then
    kill "$WESTON_PID" 2>/dev/null || true
    wait "$WESTON_PID" 2>/dev/null || true
  fi
  if [ "$status" -ne 0 ] && [ -f "$WESTON_LOG" ]; then
    printf '%s\n' '--- headless Weston log ---' >&2
    cat "$WESTON_LOG" >&2
  fi
  rm -rf -- "$WORK_ROOT"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -m 700 "$RUNTIME_DIR"
mkdir "$WORK_ROOT/home" "$WORK_ROOT/config" "$WORK_ROOT/cache" "$WORK_ROOT/data"

env -u DISPLAY -u WAYLAND_DISPLAY -u SWAYSOCK \
  HOME="$WORK_ROOT/home" \
  XDG_RUNTIME_DIR="$RUNTIME_DIR" \
  XDG_CONFIG_HOME="$WORK_ROOT/config" \
  XDG_CACHE_HOME="$WORK_ROOT/cache" \
  XDG_DATA_HOME="$WORK_ROOT/data" \
  weston \
    --backend=headless-backend.so \
    --renderer=pixman \
    --shell=desktop-shell.so \
    --socket="$WAYLAND_SOCKET" \
    --width=1920 \
    --height=1080 \
    --idle-time=0 \
    --no-config \
    --log="$WESTON_LOG" &
WESTON_PID=$!

attempt=0
while [ "$attempt" -lt 600 ]; do
  if [ -S "$RUNTIME_DIR/$WAYLAND_SOCKET" ]; then
    break
  fi
  if ! kill -0 "$WESTON_PID" 2>/dev/null; then
    wait "$WESTON_PID" 2>/dev/null || true
    printf 'error: headless Weston exited during startup\n' >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 0.025
done

if [ ! -S "$RUNTIME_DIR/$WAYLAND_SOCKET" ]; then
  printf 'error: headless Weston did not create its Wayland socket\n' >&2
  exit 1
fi

env -u DISPLAY -u SWAYSOCK \
  XDG_RUNTIME_DIR="$RUNTIME_DIR" \
  WAYLAND_DISPLAY="$WAYLAND_SOCKET" \
  GDK_BACKEND=wayland \
  NOTM_GUI_TEST_DISPLAY=provided \
  NOTM_REQUIRE_GTK_DISPLAY=1 \
  "$@"
