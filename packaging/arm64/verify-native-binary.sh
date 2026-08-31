#!/bin/sh

# Verify that a notm binary is a runnable native AArch64 GNU/Linux PIE.

set -eu

usage() {
  printf 'usage: %s BINARY VERSION\n' "$0" >&2
}

if [ "$#" -ne 2 ]; then
  usage
  exit 64
fi

BINARY=$1
VERSION=$2

if [ ! -x "$BINARY" ]; then
  printf 'binary is not executable: %s\n' "$BINARY" >&2
  exit 66
fi
case "$VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid version: %s\n' "$VERSION" >&2
    exit 64
    ;;
esac

for command_name in dpkg file getconf ldd readelf; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'required verification command is unavailable: %s\n' \
      "$command_name" >&2
    exit 69
  fi
done

if [ "$(uname -m)" != aarch64 ]; then
  printf 'native ARM64 verification requires uname -m = aarch64, got %s\n' \
    "$(uname -m)" >&2
  exit 69
fi
if [ "$(dpkg --print-architecture)" != arm64 ]; then
  printf 'native ARM64 verification requires dpkg architecture arm64\n' >&2
  exit 69
fi

FILE_OUTPUT=$(file -b "$BINARY")
readonly FILE_OUTPUT
if ! printf '%s\n' "$FILE_OUTPUT" |
  grep -Eq '^ELF 64-bit LSB .*ARM aarch64'; then
  printf 'binary is not an ELF64 AArch64 executable: %s\n' \
    "$FILE_OUTPUT" >&2
  exit 65
fi

if ! readelf -h "$BINARY" | grep -Eq '^[[:space:]]*Class:[[:space:]]*ELF64$'; then
  printf '%s\n' 'binary does not have ELF64 class' >&2
  exit 65
fi
if ! readelf -h "$BINARY" | grep -Eq '^[[:space:]]*Data:.*little endian'; then
  printf '%s\n' 'binary is not little-endian' >&2
  exit 65
fi
if ! readelf -h "$BINARY" | grep -Eq '^[[:space:]]*Machine:[[:space:]]*AArch64$'; then
  printf '%s\n' 'binary ELF machine is not AArch64' >&2
  exit 65
fi
if ! readelf -h "$BINARY" | grep -Eq '^[[:space:]]*Type:[[:space:]]*DYN'; then
  printf '%s\n' 'binary is not a position-independent ELF executable' >&2
  exit 65
fi

INTERPRETER=$(
  readelf -l "$BINARY" |
    sed -n 's|.*Requesting program interpreter: \([^]]*\)].*|\1|p'
)
readonly INTERPRETER
case "$INTERPRETER" in
  /lib/ld-linux-aarch64.so.1 | /lib64/ld-linux-aarch64.so.1)
    ;;
  *)
    printf 'unexpected or missing AArch64 program interpreter: %s\n' \
      "$INTERPRETER" >&2
    exit 65
    ;;
esac
if [ ! -e "$INTERPRETER" ]; then
  printf 'program interpreter is unavailable on the runtime: %s\n' \
    "$INTERPRETER" >&2
  exit 69
fi

if readelf -d "$BINARY" | grep -Eq '\((RPATH|RUNPATH)\)'; then
  printf '%s\n' 'release binary unexpectedly embeds RPATH or RUNPATH' >&2
  exit 65
fi
if ! readelf -d "$BINARY" | grep -F 'Shared library: [libc.so.6]' >/dev/null; then
  printf '%s\n' 'release binary is not dynamically linked to GNU libc' >&2
  exit 65
fi

LDD_OUTPUT=$(LC_ALL=C ldd "$BINARY")
readonly LDD_OUTPUT
if printf '%s\n' "$LDD_OUTPUT" | grep -F 'not found' >/dev/null; then
  printf '%s\n' "$LDD_OUTPUT" >&2
  printf '%s\n' 'release binary has an unresolved shared-library dependency' >&2
  exit 69
fi
if printf '%s\n' "$LDD_OUTPUT" |
  grep -Eq 'not a dynamic executable|statically linked'; then
  printf '%s\n' "$LDD_OUTPUT" >&2
  printf '%s\n' 'release binary did not produce a dynamic dependency map' >&2
  exit 65
fi

REQUIRED_GLIBC=$(
  readelf --version-info "$BINARY" |
    grep -o 'GLIBC_[0-9][0-9.]*' |
    sed 's/^GLIBC_//' |
    sort -Vu |
    tail -n 1
)
HOST_GLIBC=$(getconf GNU_LIBC_VERSION | awk '{print $2}')
readonly REQUIRED_GLIBC HOST_GLIBC
case "$REQUIRED_GLIBC" in
  '' | *[!0-9.]*)
    printf 'could not determine the binary GLIBC symbol baseline\n' >&2
    exit 65
    ;;
esac
case "$HOST_GLIBC" in
  '' | *[!0-9.]*)
    printf 'could not determine the host GLIBC version\n' >&2
    exit 69
    ;;
esac
if ! dpkg --compare-versions "$REQUIRED_GLIBC" le "$HOST_GLIBC"; then
  printf 'binary requires GLIBC_%s but host provides GLIBC %s\n' \
    "$REQUIRED_GLIBC" "$HOST_GLIBC" >&2
  exit 69
fi

ACTUAL_VERSION=$(
  HOME=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-version.XXXXXX")
  readonly HOME
  trap 'rm -rf -- "$HOME"' EXIT HUP INT TERM
  XDG_CONFIG_HOME="$HOME/config" \
    XDG_CACHE_HOME="$HOME/cache" \
    XDG_DATA_HOME="$HOME/data" \
    "$BINARY" --version
)
readonly ACTUAL_VERSION
if [ "$ACTUAL_VERSION" != "notm $VERSION" ]; then
  printf 'binary version mismatch: expected "notm %s", got "%s"\n' \
    "$VERSION" "$ACTUAL_VERSION" >&2
  exit 65
fi

printf 'Execution architecture: native %s (%s)\n' "$(uname -m)" \
  "$(dpkg --print-architecture)"
printf 'ELF identity: %s\n' "$FILE_OUTPUT"
printf 'Program interpreter: %s\n' "$INTERPRETER"
printf 'Required GLIBC symbols through: GLIBC_%s\n' "$REQUIRED_GLIBC"
printf 'Validation runtime: GLIBC %s\n' "$HOST_GLIBC"
printf '%s\n' 'Resolved dynamic dependencies:'
printf '%s\n' "$LDD_OUTPUT"
