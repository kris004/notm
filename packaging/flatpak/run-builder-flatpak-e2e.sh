#!/usr/bin/env bash

# Run the official Builder Flatpak without falling back to live user state.

set -euo pipefail

readonly BUILDER_APP_ID=org.flatpak.Builder
tool=flatpak-builder
if [[ ${1:-} == --linter ]]; then
  tool=flatpak-builder-lint
  shift
fi
readonly tool

for name in \
  HOME \
  TMPDIR \
  XDG_CONFIG_HOME \
  XDG_CACHE_HOME \
  XDG_DATA_HOME \
  XDG_STATE_HOME \
  XDG_RUNTIME_DIR \
  XDG_DATA_DIRS \
  DBUS_SESSION_BUS_ADDRESS \
  FLATPAK_USER_DIR \
  NOTM_FLATPAK_DISPOSABLE_ROOT \
  NOTM_FLATPAK_SOURCE_ROOT \
  NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY; do
  if [[ -z ${!name:-} ]]; then
    printf 'error: Builder Flatpak wrapper requires %s\n' "$name" >&2
    exit 73
  fi
done

canonical_directory() {
  local path=$1
  local label=$2

  if [[ ! -d $path || -L $path ]]; then
    printf 'error: %s is not a real directory: %s\n' "$label" "$path" >&2
    exit 73
  fi
  (cd -- "$path" && pwd -P)
}

DISPOSABLE_ROOT=$(canonical_directory \
  "$NOTM_FLATPAK_DISPOSABLE_ROOT" 'disposable root')
DISPOSABLE_HOME=$(canonical_directory "$HOME" HOME)
DISPOSABLE_TMPDIR=$(canonical_directory "$TMPDIR" TMPDIR)
DISPOSABLE_RUNTIME_DIR=$(canonical_directory \
  "$XDG_RUNTIME_DIR" XDG_RUNTIME_DIR)
BUILDER_INSTALLATION=$(canonical_directory \
  "$FLATPAK_USER_DIR" 'Flatpak user installation')
SOURCE_ROOT=$(canonical_directory \
  "$NOTM_FLATPAK_SOURCE_ROOT" 'source root')
FORBIDDEN_HOME_ENTRY=$(readlink -f -- "$NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY")
if [[ -z $FORBIDDEN_HOME_ENTRY || \
  (! -e $FORBIDDEN_HOME_ENTRY && ! -L $FORBIDDEN_HOME_ENTRY) ]]; then
  printf 'error: Builder live-HOME denial entry is unavailable: %s\n' \
    "$NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY" >&2
  exit 73
fi
HOST_FLATPAK=$(command -v flatpak)
if [[ $HOST_FLATPAK != /* || ! -x $HOST_FLATPAK ]]; then
  printf 'error: host Flatpak executable is unavailable: %s\n' \
    "$HOST_FLATPAK" >&2
  exit 69
fi
readonly \
  DISPOSABLE_ROOT DISPOSABLE_HOME DISPOSABLE_TMPDIR DISPOSABLE_RUNTIME_DIR \
  BUILDER_INSTALLATION SOURCE_ROOT FORBIDDEN_HOME_ENTRY HOST_FLATPAK

if [[ $(stat -c '%u:%a' -- "$DISPOSABLE_ROOT") != "$(id -u):700" ]]; then
  printf 'error: Builder disposable root is not private: %s\n' \
    "$DISPOSABLE_ROOT" >&2
  exit 73
fi

case $DBUS_SESSION_BUS_ADDRESS in
  unix:path=*)
    PRIVATE_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS%%,*}
    PRIVATE_BUS_PATH=${PRIVATE_BUS_ADDRESS#unix:path=}
    ;;
  *)
    printf 'error: Builder requires a private path-based session bus: %s\n' \
      "$DBUS_SESSION_BUS_ADDRESS" >&2
    exit 73
    ;;
esac
PRIVATE_BUS_DIRECTORY=$(canonical_directory \
  "$(dirname -- "$PRIVATE_BUS_PATH")" 'session bus directory')
PRIVATE_BUS_PATH="$PRIVATE_BUS_DIRECTORY/${PRIVATE_BUS_PATH##*/}"
if [[ $PRIVATE_BUS_PATH != "$DISPOSABLE_ROOT"/* || \
  $PRIVATE_BUS_PATH == /run/user/* || \
  ! -S $PRIVATE_BUS_PATH || \
  -L $PRIVATE_BUS_PATH || \
  $(stat -c '%u' -- "$PRIVATE_BUS_PATH") != "$(id -u)" ]]; then
  printf 'error: Builder session bus is not private: %s\n' \
    "$PRIVATE_BUS_PATH" >&2
  exit 73
fi
readonly PRIVATE_BUS_ADDRESS PRIVATE_BUS_DIRECTORY PRIVATE_BUS_PATH

if [[ $DISPOSABLE_HOME != "$DISPOSABLE_ROOT"/* || \
  $DISPOSABLE_TMPDIR != "$DISPOSABLE_ROOT"/* ]]; then
  printf '%s\n' \
    'error: Builder HOME and TMPDIR must be beneath the disposable root' >&2
  exit 73
fi
if [[ $BUILDER_INSTALLATION != "$DISPOSABLE_ROOT"/* ]]; then
  printf 'error: Builder Flatpak installation escaped disposable root: %s\n' \
    "$BUILDER_INSTALLATION" >&2
  exit 73
fi
if [[ $SOURCE_ROOT == "$DISPOSABLE_ROOT" || \
  $SOURCE_ROOT == "$DISPOSABLE_ROOT"/* || \
  $DISPOSABLE_ROOT == "$SOURCE_ROOT"/* ]]; then
  printf '%s\n' \
    'error: Builder source and disposable roots must not overlap' >&2
  exit 73
fi
if [[ $FORBIDDEN_HOME_ENTRY == "$DISPOSABLE_ROOT" || \
  $FORBIDDEN_HOME_ENTRY == "$DISPOSABLE_ROOT"/* || \
  $DISPOSABLE_ROOT == "$FORBIDDEN_HOME_ENTRY"/* || \
  $FORBIDDEN_HOME_ENTRY == "$SOURCE_ROOT" || \
  $FORBIDDEN_HOME_ENTRY == "$SOURCE_ROOT"/* || \
  $SOURCE_ROOT == "$FORBIDDEN_HOME_ENTRY"/* ]]; then
  printf '%s\n' \
    'error: Builder denial entry overlaps an explicitly exposed root' >&2
  exit 73
fi
if [[ $DISPOSABLE_RUNTIME_DIR != "$DISPOSABLE_ROOT"/* || \
  $DISPOSABLE_RUNTIME_DIR == /run/user/* || \
  $(stat -c '%u:%a' -- "$DISPOSABLE_RUNTIME_DIR") != "$(id -u):700" ]]; then
  printf 'error: Builder XDG_RUNTIME_DIR is not a private test directory: %s\n' \
    "$DISPOSABLE_RUNTIME_DIR" >&2
  exit 73
fi
for name in XDG_CONFIG_HOME XDG_CACHE_HOME XDG_DATA_HOME XDG_STATE_HOME; do
  resolved=$(canonical_directory "${!name}" "$name")
  if [[ $resolved != "$DISPOSABLE_HOME"/* ]]; then
    printf 'error: Builder %s escaped disposable HOME: %s\n' \
      "$name" "$resolved" >&2
    exit 73
  fi
done

# flatpak-builder detects its Flatpak sandbox via this runtime marker. Flatpak
# normally creates it under /run/user, so supply the equivalent marker when the
# gate deliberately uses a private runtime elsewhere.
SANDBOX_MARKER="$DISPOSABLE_RUNTIME_DIR/flatpak-info"
if [[ -e $SANDBOX_MARKER || -L $SANDBOX_MARKER ]]; then
  if [[ ! -f $SANDBOX_MARKER || -L $SANDBOX_MARKER || \
    $(stat -c '%u' -- "$SANDBOX_MARKER") != "$(id -u)" ]]; then
    printf 'error: Builder sandbox marker is unsafe: %s\n' \
      "$SANDBOX_MARKER" >&2
    exit 73
  fi
else
  (umask 077 && printf '[Application]\nname=%s\n' "$BUILDER_APP_ID" \
    >"$SANDBOX_MARKER")
fi
readonly SANDBOX_MARKER

FLATPAK_RUN_ARGUMENTS=(
  --user \
  --clear-env \
  --cwd="$DISPOSABLE_ROOT" \
  --no-a11y-bus \
  --no-documents-portal \
  --socket=session-bus \
  --nofilesystem=host \
  --nofilesystem=home \
  --nofilesystem=/var/lib/flatpak \
  --nofilesystem=xdg-data/flatpak \
  --filesystem="$DISPOSABLE_ROOT" \
  --filesystem="$BUILDER_INSTALLATION" \
  --filesystem="$DISPOSABLE_RUNTIME_DIR" \
  --filesystem="$SOURCE_ROOT:ro" \
  --env=HOME="$DISPOSABLE_HOME" \
  --env=TMPDIR="$DISPOSABLE_TMPDIR" \
  --env=XDG_CONFIG_HOME="$XDG_CONFIG_HOME" \
  --env=XDG_CACHE_HOME="$XDG_CACHE_HOME" \
  --env=XDG_DATA_HOME="$XDG_DATA_HOME" \
  --env=XDG_STATE_HOME="$XDG_STATE_HOME" \
  --env=XDG_RUNTIME_DIR="$DISPOSABLE_RUNTIME_DIR" \
  --env=XDG_DATA_DIRS="$XDG_DATA_DIRS" \
  --env=FLATPAK_USER_DIR="$BUILDER_INSTALLATION" \
  --env=FLATPAK_BINARY="$HOST_FLATPAK" \
)
readonly FLATPAK_RUN_ARGUMENTS

# The official Builder app normally requests host filesystem access. Revoke
# those defaults above and prove from inside the effective sandbox that a
# pre-existing live-HOME entry is hidden before running either Builder tool.
# The variables in this program are expanded only by the shell in the sandbox.
# shellcheck disable=SC2016
if ! flatpak run "${FLATPAK_RUN_ARGUMENTS[@]}" \
  --command=sh \
  "$BUILDER_APP_ID" \
  -c '
    set -eu
    test "$HOME" = "$1"
    test "$TMPDIR" = "$2"
    test "$XDG_RUNTIME_DIR" = "$3"
    test "$FLATPAK_USER_DIR" = "$4"
    test -d "$4"
    test -d "$5"
    test -d "$6"
    test ! -e "$7"
  ' sh \
  "$DISPOSABLE_HOME" \
  "$DISPOSABLE_TMPDIR" \
  "$DISPOSABLE_RUNTIME_DIR" \
  "$BUILDER_INSTALLATION" \
  "$DISPOSABLE_ROOT" \
  "$SOURCE_ROOT" \
  "$FORBIDDEN_HOME_ENTRY"; then
  printf '%s\n' \
    'error: effective Builder sandbox failed its live-HOME denial check' >&2
  exit 73
fi

exec flatpak run \
  "${FLATPAK_RUN_ARGUMENTS[@]}" \
  --command="$tool" \
  "$BUILDER_APP_ID" \
  "$@"
