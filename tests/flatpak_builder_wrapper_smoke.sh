#!/usr/bin/env bash

# Prove the Builder wrapper rejects live/out-of-root state and revokes host access.

set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
readonly SCRIPT_DIR
SOURCE_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd -P)
readonly SOURCE_ROOT
readonly WRAPPER="$SOURCE_ROOT/packaging/flatpak/run-builder-flatpak-e2e.sh"

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-flatpak-wrapper.XXXXXX")
readonly WORK_ROOT
OUTSIDE_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-flatpak-wrapper-outside.XXXXXX")
readonly OUTSIDE_ROOT
cleanup() {
  status=$?
  trap - EXIT HUP INT TERM
  rm -rf -- "$WORK_ROOT" "$OUTSIDE_ROOT"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -m 700 \
  "$WORK_ROOT/home" \
  "$WORK_ROOT/home/.cache" \
  "$WORK_ROOT/home/.config" \
  "$WORK_ROOT/home/.local" \
  "$WORK_ROOT/home/.local/share" \
  "$WORK_ROOT/home/.local/state" \
  "$WORK_ROOT/install" \
  "$WORK_ROOT/runtime" \
  "$WORK_ROOT/tmp" \
  "$OUTSIDE_ROOT/forbidden" \
  "$OUTSIDE_ROOT/install" \
  "$OUTSIDE_ROOT/runtime"
mkdir -m 700 "$WORK_ROOT/bin"
mkdir -m 700 \
  "$WORK_ROOT/e2e-install-disposable" \
  "$WORK_ROOT/e2e-install-disposable/runtime" \
  "$WORK_ROOT/e2e-runtime-disposable" \
  "$WORK_ROOT/e2e-work-disposable" \
  "$WORK_ROOT/e2e-work-disposable/runtime"
mkdir -m 755 "$WORK_ROOT/e2e-runtime-disposable/public-runtime"

cat >"$WORK_ROOT/bin/flatpak" <<'EOF'
#!/bin/sh
set -eu
{
  printf '%s\n' '=== flatpak invocation ==='
  printf '%s\n' "$@"
} >>"$CAPTURE_FILE"
EOF
chmod 0755 "$WORK_ROOT/bin/flatpak"

export HOME="$WORK_ROOT/home"
export TMPDIR="$WORK_ROOT/tmp"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_DATA_DIRS=/usr/local/share:/usr/share
export FLATPAK_USER_DIR="$WORK_ROOT/install"
export NOTM_TEST_DISPOSABLE_ROOT="$WORK_ROOT"
export NOTM_FLATPAK_DISPOSABLE_ROOT="$WORK_ROOT"
export NOTM_FLATPAK_SOURCE_ROOT="$SOURCE_ROOT"
export NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY="$OUTSIDE_ROOT/forbidden"
export CAPTURE_FILE="$WORK_ROOT/flatpak-arguments"
export PATH="$WORK_ROOT/bin:$PATH"

XDG_RUNTIME_DIR="$WORK_ROOT/runtime" \
  "$SOURCE_ROOT/tests/run_with_private_dbus.sh" "$WRAPPER" --version
grep -Fx -- "--filesystem=$WORK_ROOT" "$CAPTURE_FILE" >/dev/null
grep -Fx -- "--filesystem=$WORK_ROOT/install" "$CAPTURE_FILE" >/dev/null
grep -Fx -- "--filesystem=$WORK_ROOT/runtime" "$CAPTURE_FILE" >/dev/null
grep -Fx -- "--filesystem=$SOURCE_ROOT:ro" "$CAPTURE_FILE" >/dev/null
grep -Fx -- "--cwd=$WORK_ROOT" "$CAPTURE_FILE" >/dev/null
grep -Fx -- '--nofilesystem=host' "$CAPTURE_FILE" >/dev/null
grep -Fx -- '--nofilesystem=home' "$CAPTURE_FILE" >/dev/null
grep -Fx -- "--env=XDG_RUNTIME_DIR=$WORK_ROOT/runtime" "$CAPTURE_FILE" >/dev/null
grep -Fx -- '--command=sh' "$CAPTURE_FILE" >/dev/null
grep -Fx -- "$OUTSIDE_ROOT/forbidden" "$CAPTURE_FILE" >/dev/null
grep -Fx -- '--command=flatpak-builder' "$CAPTURE_FILE" >/dev/null

XDG_RUNTIME_DIR="$WORK_ROOT/runtime" \
  "$SOURCE_ROOT/tests/run_with_private_dbus.sh" \
  "$WRAPPER" --linter --version
grep -Fx -- '--command=flatpak-builder-lint' "$CAPTURE_FILE" >/dev/null

if NOTM_FLATPAK_LINTER_COMMAND=true \
  "$SOURCE_ROOT/tests/flatpak_distribution_e2e.sh" \
  >"$WORK_ROOT/linter-bypass.stdout" \
  2>"$WORK_ROOT/linter-bypass.stderr"; then
  printf '%s\n' 'error: Flatpak E2E accepted a non-official linter override' >&2
  exit 1
fi
grep -F 'linter override must use the committed official Builder wrapper' \
  "$WORK_ROOT/linter-bypass.stderr" >/dev/null

if FLATPAK_USER_DIR="$OUTSIDE_ROOT/install" \
  XDG_RUNTIME_DIR="$WORK_ROOT/runtime" \
  "$SOURCE_ROOT/tests/run_with_private_dbus.sh" \
  "$WRAPPER" --version \
  >"$WORK_ROOT/install-outside.stdout" \
  2>"$WORK_ROOT/install-outside.stderr"; then
  printf '%s\n' 'error: Builder wrapper accepted an out-of-root installation' >&2
  exit 1
fi
grep -F 'Builder Flatpak installation escaped disposable root' \
  "$WORK_ROOT/install-outside.stderr" >/dev/null

readonly MISSING_OUTSIDE_INSTALL="$OUTSIDE_ROOT/missing-install"
if NOTM_GUI_TEST_DISPLAY=provided \
  NOTM_REQUIRE_GTK_DISPLAY=1 \
  NOTM_TEST_DISPOSABLE_ROOT="$WORK_ROOT/e2e-install-disposable" \
  NOTM_FLATPAK_USER_DIR="$MISSING_OUTSIDE_INSTALL" \
  NOTM_FLATPAK_BUILDER_COMMAND=true \
  NOTM_FLATPAK_LINTER_COMMAND="$WRAPPER --linter" \
  XDG_RUNTIME_DIR="$WORK_ROOT/e2e-install-disposable/runtime" \
  "$SOURCE_ROOT/tests/flatpak_distribution_e2e.sh" \
  >"$WORK_ROOT/e2e-install-outside.stdout" \
  2>"$WORK_ROOT/e2e-install-outside.stderr"; then
  printf '%s\n' 'error: Flatpak E2E accepted an out-of-root installation' >&2
  exit 1
fi
if [[ -e $MISSING_OUTSIDE_INSTALL || -L $MISSING_OUTSIDE_INSTALL ]]; then
  printf '%s\n' 'error: Flatpak E2E created the rejected outside installation' >&2
  exit 1
fi
grep -F 'Flatpak user installation escaped the private test root' \
  "$WORK_ROOT/e2e-install-outside.stderr" >/dev/null

if NOTM_GUI_TEST_DISPLAY=provided \
  NOTM_REQUIRE_GTK_DISPLAY=1 \
  NOTM_TEST_DISPOSABLE_ROOT="$WORK_ROOT/e2e-runtime-disposable" \
  NOTM_FLATPAK_BUILDER_COMMAND=true \
  NOTM_FLATPAK_LINTER_COMMAND="$WRAPPER --linter" \
  XDG_RUNTIME_DIR="$WORK_ROOT/e2e-runtime-disposable/public-runtime" \
  "$SOURCE_ROOT/tests/flatpak_distribution_e2e.sh" \
  >"$WORK_ROOT/e2e-runtime-public.stdout" \
  2>"$WORK_ROOT/e2e-runtime-public.stderr"; then
  printf '%s\n' 'error: Flatpak E2E accepted a non-private runtime' >&2
  exit 1
fi
grep -F 'XDG_RUNTIME_DIR is not a private test directory' \
  "$WORK_ROOT/e2e-runtime-public.stderr" >/dev/null

readonly MISSING_OUTSIDE_WORK="$OUTSIDE_ROOT/missing-work"
if NOTM_GUI_TEST_DISPLAY=provided \
  NOTM_REQUIRE_GTK_DISPLAY=1 \
  NOTM_TEST_DISPOSABLE_ROOT="$WORK_ROOT/e2e-work-disposable" \
  NOTM_FLATPAK_WORK_ROOT="$MISSING_OUTSIDE_WORK" \
  NOTM_FLATPAK_BUILDER_COMMAND=true \
  NOTM_FLATPAK_LINTER_COMMAND="$WRAPPER --linter" \
  XDG_RUNTIME_DIR="$WORK_ROOT/e2e-work-disposable/runtime" \
  "$SOURCE_ROOT/tests/flatpak_distribution_e2e.sh" \
  >"$WORK_ROOT/e2e-work-outside.stdout" \
  2>"$WORK_ROOT/e2e-work-outside.stderr"; then
  printf '%s\n' 'error: Flatpak E2E accepted an out-of-root work directory' >&2
  exit 1
fi
if [[ -e $MISSING_OUTSIDE_WORK || -L $MISSING_OUTSIDE_WORK ]]; then
  printf '%s\n' 'error: Flatpak E2E created the rejected outside work directory' >&2
  exit 1
fi
grep -F 'Flatpak work root escaped the private test root' \
  "$WORK_ROOT/e2e-work-outside.stderr" >/dev/null

if XDG_RUNTIME_DIR="$OUTSIDE_ROOT/runtime" \
  "$SOURCE_ROOT/tests/run_with_private_dbus.sh" \
  "$WRAPPER" --version \
  >"$WORK_ROOT/outside.stdout" 2>"$WORK_ROOT/outside.stderr"; then
  printf '%s\n' 'error: Builder wrapper accepted an out-of-root runtime' >&2
  exit 1
fi
grep -F 'XDG_RUNTIME_DIR is not a private test directory' \
  "$WORK_ROOT/outside.stderr" >/dev/null

if XDG_RUNTIME_DIR="/run/user/$(id -u)" \
  "$SOURCE_ROOT/tests/run_with_private_dbus.sh" \
  "$WRAPPER" --version \
  >"$WORK_ROOT/live.stdout" 2>"$WORK_ROOT/live.stderr"; then
  printf '%s\n' 'error: Builder wrapper accepted the live user runtime' >&2
  exit 1
fi
if ! grep -E \
  'XDG_RUNTIME_DIR is not a private test directory|XDG_RUNTIME_DIR is not a real directory' \
  "$WORK_ROOT/live.stderr" >/dev/null; then
  printf '%s\n' 'error: live-runtime rejection was not fail-closed' >&2
  cat "$WORK_ROOT/live.stderr" >&2
  exit 1
fi

printf '%s\n' 'Flatpak Builder wrapper isolation smoke passed.'
