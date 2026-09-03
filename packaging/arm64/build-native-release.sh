#!/bin/sh

# Build, inspect, package, and verify notm on a native Ubuntu ARM64 host.

set -eu

TARGET=aarch64-unknown-linux-gnu
RUST_TOOLCHAIN=1.98.0
readonly TARGET RUST_TOOLCHAIN

usage() {
  printf '%s\n' \
    "usage: $0 SOURCE_ROOT VERSION SOURCE_COMMIT OUTPUT_DIR EVIDENCE_DIR" >&2
}

if [ "$#" -ne 5 ]; then
  usage
  exit 64
fi

SOURCE_ROOT=$1
RELEASE_VERSION=$2
SOURCE_COMMIT=$3
OUTPUT_DIR=$4
EVIDENCE_DIR=$5

if [ ! -d "$SOURCE_ROOT" ]; then
  printf 'source root is not a directory: %s\n' "$SOURCE_ROOT" >&2
  exit 66
fi
SOURCE_ROOT=$(
  CDPATH=''
  cd -- "$SOURCE_ROOT"
  pwd -P
)
if ! SOURCE_COMMIT=$(
  git -C "$SOURCE_ROOT" rev-parse --verify "${SOURCE_COMMIT}^{commit}"
); then
  printf 'source commit does not resolve to a commit\n' >&2
  exit 65
fi
readonly SOURCE_ROOT RELEASE_VERSION SOURCE_COMMIT

case "$RELEASE_VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid release version: %s\n' "$RELEASE_VERSION" >&2
    exit 64
    ;;
esac
if [ "$(git -C "$SOURCE_ROOT" rev-parse HEAD)" != "$SOURCE_COMMIT" ]; then
  printf 'source commit is not the checked-out HEAD: %s\n' "$SOURCE_COMMIT" >&2
  exit 65
fi
if ! git -C "$SOURCE_ROOT" diff --quiet --exit-code HEAD -- ||
  [ -n "$(git -C "$SOURCE_ROOT" ls-files --others --exclude-standard)" ]; then
  printf '%s\n' \
    'native release build requires a clean tree matching the source commit' >&2
  exit 65
fi
METADATA_VERSION=$(
  "$SOURCE_ROOT/packaging/verify-release-metadata.py" \
    --print-version "$SOURCE_ROOT"
)
readonly METADATA_VERSION
if [ "$METADATA_VERSION" != "$RELEASE_VERSION" ]; then
  printf 'release metadata version mismatch: expected %s, got %s\n' \
    "$RELEASE_VERSION" "$METADATA_VERSION" >&2
  exit 65
fi
SOURCE_DATE_EPOCH=$(git -C "$SOURCE_ROOT" show -s --format=%ct "$SOURCE_COMMIT")
case "$SOURCE_DATE_EPOCH" in
  '' | *[!0-9]*)
    printf '%s\n' 'source commit has an invalid timestamp' >&2
    exit 65
    ;;
esac
LOCK_SHA256=$(sha256sum "$SOURCE_ROOT/Cargo.lock" | awk '{print $1}')
readonly SOURCE_DATE_EPOCH LOCK_SHA256

if [ ! -r /etc/os-release ]; then
  printf '%s\n' 'native release build requires /etc/os-release' >&2
  exit 69
fi
# The release baseline is Ubuntu 24.04, while the hosted image revision remains
# mutable and is recorded separately when the runner exposes it.
OS_ID=$(
  # shellcheck disable=SC1091 # The standard os-release path is checked above.
  . /etc/os-release
  printf '%s\n' "${ID:-}"
)
OS_VERSION_ID=$(
  # shellcheck disable=SC1091 # The standard os-release path is checked above.
  . /etc/os-release
  printf '%s\n' "${VERSION_ID:-}"
)
readonly OS_ID OS_VERSION_ID
if [ "$OS_ID" != ubuntu ] || [ "$OS_VERSION_ID" != 24.04 ]; then
  printf '%s\n' 'native release build requires an Ubuntu 24.04 userspace' >&2
  exit 69
fi

for command_name in \
  appstreamcli cargo desktop-file-validate dpkg dpkg-query file getconf git \
  ldd readelf rustc sha256sum strip; do
  if ! command -v "$command_name" >/dev/null 2>&1; then
    printf 'required native-build command is unavailable: %s\n' \
      "$command_name" >&2
    exit 69
  fi
done

if [ "$(uname -m)" != aarch64 ] || \
  [ "$(dpkg --print-architecture)" != arm64 ]; then
  printf '%s\n' 'native release build requires an AArch64 Ubuntu userspace' >&2
  exit 69
fi
if [ "$(rustc --version | awk '{print $2}')" != "$RUST_TOOLCHAIN" ]; then
  printf 'native release build requires rustc %s exactly\n' \
    "$RUST_TOOLCHAIN" >&2
  exit 69
fi
if [ "$(cargo --version | awk '{print $2}')" != "$RUST_TOOLCHAIN" ]; then
  printf 'native release build requires cargo %s exactly\n' \
    "$RUST_TOOLCHAIN" >&2
  exit 69
fi
if ! rustc --version --verbose | grep -Fx "host: $TARGET" >/dev/null; then
  printf 'Rust host is not %s\n' "$TARGET" >&2
  exit 69
fi

prepare_empty_directory() {
  requested_path=$1
  label=$2

  if [ -L "$requested_path" ]; then
    printf '%s must not be a symlink: %s\n' "$label" "$requested_path" >&2
    exit 73
  fi
  if [ -e "$requested_path" ]; then
    if [ ! -d "$requested_path" ]; then
      printf '%s is not a directory: %s\n' "$label" "$requested_path" >&2
      exit 73
    fi
  else
    mkdir -p -- "$requested_path"
  fi

  resolved_path=$(
    CDPATH=''
    cd -- "$requested_path"
    pwd -P
  )
  if find "$resolved_path" -mindepth 1 -maxdepth 1 -print -quit |
    grep -q .; then
    printf '%s must be empty: %s\n' "$label" "$resolved_path" >&2
    exit 73
  fi
  printf '%s\n' "$resolved_path"
}

OUTPUT_DIR=$(prepare_empty_directory "$OUTPUT_DIR" 'release output directory')
EVIDENCE_DIR=$(
  prepare_empty_directory "$EVIDENCE_DIR" 'reproducibility evidence directory'
)
if [ "$OUTPUT_DIR" = "$EVIDENCE_DIR" ]; then
  printf '%s\n' 'release output and evidence directories must be distinct' >&2
  exit 73
fi
readonly OUTPUT_DIR EVIDENCE_DIR

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-native-build.XXXXXX")
BUILD_INFO="$WORK_ROOT/BUILD-INFO.txt"
REMOVE_TARGET_DIR=0
TARGET_DIR=

cleanup() {
  if [ "$REMOVE_TARGET_DIR" -eq 1 ] && [ -n "$TARGET_DIR" ]; then
    rm -rf -- "$TARGET_DIR"
  fi
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

if [ "${NOTM_ARM64_RELEASE_TARGET_DIR+x}" = x ]; then
  if [ -z "$NOTM_ARM64_RELEASE_TARGET_DIR" ]; then
    printf '%s\n' 'NOTM_ARM64_RELEASE_TARGET_DIR must not be empty' >&2
    exit 64
  fi
  if [ -e "$NOTM_ARM64_RELEASE_TARGET_DIR" ] ||
    [ -L "$NOTM_ARM64_RELEASE_TARGET_DIR" ]; then
    printf 'clean Cargo target directory already exists: %s\n' \
      "$NOTM_ARM64_RELEASE_TARGET_DIR" >&2
    exit 73
  fi
  TARGET_PARENT=$(dirname -- "$NOTM_ARM64_RELEASE_TARGET_DIR")
  TARGET_BASENAME=$(basename -- "$NOTM_ARM64_RELEASE_TARGET_DIR")
  case "$TARGET_BASENAME" in
    '' | . | ..)
      printf 'invalid clean Cargo target directory: %s\n' \
        "$NOTM_ARM64_RELEASE_TARGET_DIR" >&2
      exit 64
      ;;
  esac
  if [ ! -d "$TARGET_PARENT" ]; then
    printf 'Cargo target parent is not a directory: %s\n' \
      "$TARGET_PARENT" >&2
    exit 73
  fi
  TARGET_PARENT=$(
    CDPATH=''
    cd -- "$TARGET_PARENT"
    pwd -P
  )
  TARGET_DIR="$TARGET_PARENT/$TARGET_BASENAME"
  REMOVE_TARGET_DIR=1
else
  TARGET_DIR="$WORK_ROOT/target"
fi
readonly WORK_ROOT BUILD_INFO TARGET_DIR

export CARGO_INCREMENTAL=0
export SOURCE_DATE_EPOCH
if [ -e "$TARGET_DIR" ] || [ -L "$TARGET_DIR" ]; then
  printf 'clean Cargo target directory already exists: %s\n' \
    "$TARGET_DIR" >&2
  exit 73
fi
(
  cd -- "$SOURCE_ROOT"
  CARGO_TARGET_DIR="$TARGET_DIR" \
    cargo build --locked \
      --release \
      --frozen \
      --target "$TARGET" \
      -p notm-app
)
BINARY="$TARGET_DIR/$TARGET/release/notm"
strip --strip-unneeded "$BINARY"
BINARY_SHA256=$(sha256sum "$BINARY" | awk '{print $1}')
readonly BINARY_SHA256 BINARY

REQUIRED_GLIBC=$(
  readelf --version-info "$BINARY" |
    grep -o 'GLIBC_[0-9][0-9.]*' |
    sed 's/^GLIBC_//' |
    sort -Vu |
    tail -n 1
)
HOST_GLIBC=$(getconf GNU_LIBC_VERSION | awk '{print $2}')
SOURCE_REF=${SOURCE_REF:-$SOURCE_COMMIT}
SOURCE_REPOSITORY=${SOURCE_REPOSITORY:-https://github.com/kris004/notm}
RUNNER_ARCHITECTURE=${RUNNER_ARCH:-ARM64}
BUILD_RUNNER_LABEL=${BUILD_RUNNER_LABEL:-local-native-arm64}
RUNNER_IMAGE_OS=${ImageOS:-unrecorded}
RUNNER_IMAGE_VERSION=${ImageVersion:-unrecorded}
RUST_COMPILER=$(rustc --version)
CARGO_VERSION=$(cargo --version)
KERNEL_ARCHITECTURE=$(uname -m)
DEBIAN_ARCHITECTURE=$(dpkg --print-architecture)
NATIVE_BUILD_PACKAGES=$(
  dpkg-query --show --showformat='${binary:Package}\t${Version}\n' \
    libgtk-4-dev \
    libgtksourceview-5-dev \
    libnotmuch-dev \
    libwebkitgtk-6.0-dev |
    LC_ALL=C sort
)
RUNTIME_PACKAGES=$(
  dpkg-query --show --showformat='${binary:Package}\t${Version}\n' \
    libgtk-4-1 \
    libgtksourceview-5-0 \
    libnotmuch5t64 \
    libwebkitgtk-6.0-4 |
    LC_ALL=C sort
)
ELF_DEPENDENCIES=$(
  readelf -d "$BINARY" |
    sed -n 's/.*Shared library: \[\(.*\)\]/\1/p' |
    LC_ALL=C sort -u
)
readonly \
  REQUIRED_GLIBC HOST_GLIBC SOURCE_REF \
  SOURCE_REPOSITORY RUNNER_ARCHITECTURE BUILD_RUNNER_LABEL \
  RUNNER_IMAGE_OS RUNNER_IMAGE_VERSION RUST_COMPILER CARGO_VERSION \
  KERNEL_ARCHITECTURE DEBIAN_ARCHITECTURE NATIVE_BUILD_PACKAGES \
  RUNTIME_PACKAGES ELF_DEPENDENCIES

{
  printf 'Version: %s\n' "$RELEASE_VERSION"
  printf 'Source tag: v%s\n' "$RELEASE_VERSION"
  printf 'Source commit: %s\n' "$SOURCE_COMMIT"
  printf 'Source ref: %s\n' "$SOURCE_REF"
  printf 'Source repository: %s\n' "$SOURCE_REPOSITORY"
  printf 'Source date epoch: %s\n' "$SOURCE_DATE_EPOCH"
  printf 'Target: %s\n' "$TARGET"
  printf 'Build architecture: aarch64\n'
  printf 'Build environment: Ubuntu 24.04 ARM64 userspace\n'
  printf 'Ubuntu ID: %s\n' "$OS_ID"
  printf 'Ubuntu version ID: %s\n' "$OS_VERSION_ID"
  printf 'Kernel architecture: %s\n' "$KERNEL_ARCHITECTURE"
  printf 'Debian architecture: %s\n' "$DEBIAN_ARCHITECTURE"
  printf 'Runner label: %s\n' "$BUILD_RUNNER_LABEL"
  printf 'Runner architecture: %s\n' "$RUNNER_ARCHITECTURE"
  printf 'Runner image OS: %s\n' "$RUNNER_IMAGE_OS"
  printf 'Runner image version: %s\n' "$RUNNER_IMAGE_VERSION"
  printf 'Cargo.lock SHA-256: %s\n' "$LOCK_SHA256"
  printf 'Rust compiler: %s\n' "$RUST_COMPILER"
  printf 'Cargo: %s\n' "$CARGO_VERSION"
  printf 'Release binary SHA-256: %s\n' "$BINARY_SHA256"
  printf '%s\n' \
    'Binary reproducibility: independent clean workflow comparison required'
  printf 'Required GLIBC symbols through: GLIBC_%s\n' "$REQUIRED_GLIBC"
  printf 'Validation runtime: GLIBC %s\n' "$HOST_GLIBC"
  printf '\nNative build packages:\n'
  printf '%s\n' "$NATIVE_BUILD_PACKAGES"
  printf '\nRuntime package baseline:\n'
  printf '%s\n' "$RUNTIME_PACKAGES"
  printf '\nDirect ELF dependencies:\n'
  printf '%s\n' "$ELF_DEPENDENCIES"
} >"$BUILD_INFO"

"$SOURCE_ROOT/packaging/arm64/verify-native-binary.sh" \
  "$BINARY" "$RELEASE_VERSION"
"$SOURCE_ROOT/packaging/arm64/build-release.sh" \
  "$SOURCE_ROOT" \
  "$RELEASE_VERSION" \
  "$SOURCE_COMMIT" \
  "$BINARY" \
  "$BUILD_INFO" \
  "$OUTPUT_DIR"

ARCHIVE="$OUTPUT_DIR/notm-v${RELEASE_VERSION}-${TARGET}.tar.gz"
CHECKSUM="$OUTPUT_DIR/SHA256SUMS-aarch64"
readonly ARCHIVE CHECKSUM
"$SOURCE_ROOT/packaging/arm64/verify-release.sh" \
  "$SOURCE_ROOT" \
  "$RELEASE_VERSION" \
  "$SOURCE_COMMIT" \
  "$ARCHIVE" \
  "$CHECKSUM"

ARCHIVE_SHA256=$(sha256sum "$ARCHIVE" | awk '{print $1}')
readonly ARCHIVE_SHA256
printf '%s  notm\n' "$BINARY_SHA256" >"$EVIDENCE_DIR/binary.sha256"
printf '%s  %s\n' \
  "$ARCHIVE_SHA256" \
  "$(basename -- "$ARCHIVE")" >"$EVIDENCE_DIR/archive.sha256"
{
  printf '%s\n' 'Evidence format: notm ARM64 reproducibility v1'
  printf 'Version: %s\n' "$RELEASE_VERSION"
  printf 'Source tag: v%s\n' "$RELEASE_VERSION"
  printf 'Source commit: %s\n' "$SOURCE_COMMIT"
  printf 'Source ref: %s\n' "$SOURCE_REF"
  printf 'Source repository: %s\n' "$SOURCE_REPOSITORY"
  printf 'Source date epoch: %s\n' "$SOURCE_DATE_EPOCH"
  printf 'Cargo.lock SHA-256: %s\n' "$LOCK_SHA256"
  printf 'Target: %s\n' "$TARGET"
  printf 'Build architecture: aarch64\n'
  printf 'Build environment: Ubuntu 24.04 ARM64 userspace\n'
  printf 'Ubuntu ID: %s\n' "$OS_ID"
  printf 'Ubuntu version ID: %s\n' "$OS_VERSION_ID"
  printf 'Kernel architecture: %s\n' "$KERNEL_ARCHITECTURE"
  printf 'Debian architecture: %s\n' "$DEBIAN_ARCHITECTURE"
  printf 'Rust host: %s\n' "$TARGET"
  printf 'Runner label: %s\n' "$BUILD_RUNNER_LABEL"
  printf 'Runner architecture: %s\n' "$RUNNER_ARCHITECTURE"
  printf 'Runner image OS: %s\n' "$RUNNER_IMAGE_OS"
  printf 'Runner image version: %s\n' "$RUNNER_IMAGE_VERSION"
  printf 'Rust compiler: %s\n' "$RUST_COMPILER"
  printf 'Cargo: %s\n' "$CARGO_VERSION"
  printf 'Required GLIBC symbols through: GLIBC_%s\n' "$REQUIRED_GLIBC"
  printf 'Validation runtime: GLIBC %s\n' "$HOST_GLIBC"
  printf '%s\n' \
    'Reproducibility comparison: independent clean workflow comparison required'
  printf '\nNative build packages:\n'
  printf '%s\n' "$NATIVE_BUILD_PACKAGES"
  printf '\nRuntime package baseline:\n'
  printf '%s\n' "$RUNTIME_PACKAGES"
  printf '\nDirect ELF dependencies:\n'
  printf '%s\n' "$ELF_DEPENDENCIES"
} >"$EVIDENCE_DIR/reproducibility-evidence.txt"

if [ "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 | wc -l)" -ne 2 ]; then
  printf '%s\n' 'native build emitted an unexpected release layout' >&2
  exit 65
fi
for release_artifact in "$ARCHIVE" "$CHECKSUM"; do
  if [ ! -f "$release_artifact" ] || [ -L "$release_artifact" ]; then
    printf 'native build emitted an invalid release artifact: %s\n' \
      "$release_artifact" >&2
    exit 65
  fi
done
if [ "$(find "$EVIDENCE_DIR" -mindepth 1 -maxdepth 1 | wc -l)" -ne 3 ]; then
  printf '%s\n' 'native build emitted an unexpected evidence layout' >&2
  exit 65
fi
for evidence_artifact in \
  "$EVIDENCE_DIR/binary.sha256" \
  "$EVIDENCE_DIR/archive.sha256" \
  "$EVIDENCE_DIR/reproducibility-evidence.txt"; do
  if [ ! -f "$evidence_artifact" ] || [ -L "$evidence_artifact" ]; then
    printf 'native build emitted an invalid evidence artifact: %s\n' \
      "$evidence_artifact" >&2
    exit 65
  fi
done

printf 'ARM64 archive SHA-256: %s\n' \
  "$ARCHIVE_SHA256"
printf 'ARM64 binary SHA-256: %s\n' "$BINARY_SHA256"
printf '%s\n' \
  'Independent clean workflow comparison is required for reproducibility'
