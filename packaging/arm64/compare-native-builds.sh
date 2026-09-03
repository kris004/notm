#!/bin/sh

# Compare two independently built native ARM64 release and evidence fragments.

set -eu

TARGET=aarch64-unknown-linux-gnu
CHECKSUM_NAME=SHA256SUMS-aarch64
readonly TARGET CHECKSUM_NAME

usage() {
  printf '%s\n' \
    "usage: $0 SOURCE_ROOT VERSION SOURCE_COMMIT RELEASE_A EVIDENCE_A RELEASE_B EVIDENCE_B" >&2
}

if [ "$#" -ne 7 ]; then
  usage
  exit 64
fi

SOURCE_ROOT=$1
VERSION=$2
SOURCE_COMMIT=$3
RELEASE_A=$4
EVIDENCE_A=$5
RELEASE_B=$6
EVIDENCE_B=$7

case "$VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid release version: %s\n' "$VERSION" >&2
    exit 64
    ;;
esac
if [ ! -d "$SOURCE_ROOT" ] || [ -L "$SOURCE_ROOT" ]; then
  printf 'source root is not a real directory: %s\n' "$SOURCE_ROOT" >&2
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
  printf '%s\n' 'source commit does not resolve to a commit' >&2
  exit 65
fi
if [ "$(git -C "$SOURCE_ROOT" rev-parse HEAD)" != "$SOURCE_COMMIT" ]; then
  printf 'comparison checkout does not match source commit: %s\n' \
    "$SOURCE_COMMIT" >&2
  exit 65
fi
if [ "$("$SOURCE_ROOT/packaging/verify-release-metadata.py" \
  --print-version "$SOURCE_ROOT")" != "$VERSION" ]; then
  printf 'comparison checkout does not match release version: %s\n' \
    "$VERSION" >&2
  exit 65
fi
if ! git -C "$SOURCE_ROOT" diff --quiet "$SOURCE_COMMIT" -- Cargo.lock; then
  printf '%s\n' 'comparison Cargo.lock differs from the source commit' >&2
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
readonly SOURCE_ROOT VERSION SOURCE_COMMIT SOURCE_DATE_EPOCH LOCK_SHA256

resolve_directory() {
  requested_path=$1
  label=$2

  if [ ! -d "$requested_path" ] || [ -L "$requested_path" ]; then
    printf '%s is not a real directory: %s\n' "$label" "$requested_path" >&2
    exit 66
  fi
  (
    CDPATH=''
    cd -- "$requested_path"
    pwd -P
  )
}

RELEASE_A=$(resolve_directory "$RELEASE_A" 'release A directory')
EVIDENCE_A=$(resolve_directory "$EVIDENCE_A" 'evidence A directory')
RELEASE_B=$(resolve_directory "$RELEASE_B" 'release B directory')
EVIDENCE_B=$(resolve_directory "$EVIDENCE_B" 'evidence B directory')
if [ "$(printf '%s\n' \
  "$RELEASE_A" "$EVIDENCE_A" "$RELEASE_B" "$EVIDENCE_B" |
  LC_ALL=C sort -u |
  wc -l)" -ne 4 ]; then
  printf '%s\n' 'release and evidence directories must all be distinct' >&2
  exit 73
fi
readonly RELEASE_A EVIDENCE_A RELEASE_B EVIDENCE_B

ARCHIVE_NAME="notm-v${VERSION}-${TARGET}.tar.gz"
BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
readonly ARCHIVE_NAME BUNDLE_NAME

require_exact_directory_entries() {
  directory=$1
  expected_count=$2
  label=$3
  actual_count=$(find "$directory" -mindepth 1 -maxdepth 1 | wc -l)

  if [ "$actual_count" -ne "$expected_count" ]; then
    printf '%s has %s entries instead of %s: %s\n' \
      "$label" "$actual_count" "$expected_count" "$directory" >&2
    find "$directory" -mindepth 1 -maxdepth 1 -print >&2
    exit 65
  fi
}

require_regular_file() {
  path=$1
  if [ ! -f "$path" ] || [ -L "$path" ]; then
    printf 'comparison input is not a regular file: %s\n' "$path" >&2
    exit 65
  fi
}

for release_pair in "$RELEASE_A" "$RELEASE_B"; do
  require_exact_directory_entries "$release_pair" 2 'release directory'
  require_regular_file "$release_pair/$ARCHIVE_NAME"
  require_regular_file "$release_pair/$CHECKSUM_NAME"
done
for evidence_pair in "$EVIDENCE_A" "$EVIDENCE_B"; do
  require_exact_directory_entries "$evidence_pair" 3 'evidence directory'
  require_regular_file "$evidence_pair/binary.sha256"
  require_regular_file "$evidence_pair/archive.sha256"
  require_regular_file "$evidence_pair/reproducibility-evidence.txt"
done

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-compare.XXXXXX")
readonly WORK_ROOT

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

parse_sha_fragment() {
  fragment=$1
  expected_name=$2

  if [ "$(wc -l <"$fragment")" -ne 1 ]; then
    printf 'checksum fragment must contain one newline-terminated entry: %s\n' \
      "$fragment" >&2
    return 1
  fi
  if ! awk -v expected_name="$expected_name" '
    length($1) == 64 &&
      $1 ~ /^[0-9a-f]+$/ &&
      $0 == $1 "  " expected_name {
        digest = $1
        valid++
      }
    END {
      if (valid != 1) {
        exit 1
      }
      print digest
    }
  ' "$fragment"; then
    printf 'checksum fragment is not canonical for %s: %s\n' \
      "$expected_name" "$fragment" >&2
    return 1
  fi
}

if ! A_RELEASE_ARCHIVE_SHA256=$(
  parse_sha_fragment "$RELEASE_A/$CHECKSUM_NAME" "$ARCHIVE_NAME"
); then
  exit 65
fi
if ! B_RELEASE_ARCHIVE_SHA256=$(
  parse_sha_fragment "$RELEASE_B/$CHECKSUM_NAME" "$ARCHIVE_NAME"
); then
  exit 65
fi
if ! A_BINARY_SHA256=$(
  parse_sha_fragment "$EVIDENCE_A/binary.sha256" notm
); then
  exit 65
fi
if ! B_BINARY_SHA256=$(
  parse_sha_fragment "$EVIDENCE_B/binary.sha256" notm
); then
  exit 65
fi
if ! A_EVIDENCE_ARCHIVE_SHA256=$(
  parse_sha_fragment "$EVIDENCE_A/archive.sha256" "$ARCHIVE_NAME"
); then
  exit 65
fi
if ! B_EVIDENCE_ARCHIVE_SHA256=$(
  parse_sha_fragment "$EVIDENCE_B/archive.sha256" "$ARCHIVE_NAME"
); then
  exit 65
fi
readonly \
  A_RELEASE_ARCHIVE_SHA256 B_RELEASE_ARCHIVE_SHA256 \
  A_BINARY_SHA256 B_BINARY_SHA256 \
  A_EVIDENCE_ARCHIVE_SHA256 B_EVIDENCE_ARCHIVE_SHA256

validate_reproducibility_evidence() {
  evidence=$1

  if ! LC_ALL=C awk \
    -v version="$VERSION" \
    -v commit="$SOURCE_COMMIT" \
    -v epoch="$SOURCE_DATE_EPOCH" \
    -v lock_sha256="$LOCK_SHA256" \
    -v target="$TARGET" '
    function reject() {
      invalid = 1
    }
    NR == 1 {
      if ($0 != "Evidence format: notm ARM64 reproducibility v1") reject()
      next
    }
    NR == 2 {
      if ($0 != "Version: " version) reject()
      next
    }
    NR == 3 {
      if ($0 != "Source tag: v" version) reject()
      next
    }
    NR == 4 {
      if ($0 != "Source commit: " commit) reject()
      next
    }
    NR == 5 {
      if ($0 !~ /^Source ref: [^[:space:]]+$/) reject()
      next
    }
    NR == 6 {
      if ($0 !~ /^Source repository: [^[:space:]]+$/) reject()
      next
    }
    NR == 7 {
      if ($0 != "Source date epoch: " epoch) reject()
      next
    }
    NR == 8 {
      if ($0 != "Cargo.lock SHA-256: " lock_sha256) reject()
      next
    }
    NR == 9 {
      if ($0 != "Target: " target) reject()
      next
    }
    NR == 10 {
      if ($0 != "Build architecture: aarch64") reject()
      next
    }
    NR == 11 {
      if ($0 != "Build environment: Ubuntu 24.04 ARM64 userspace") reject()
      next
    }
    NR == 12 {
      if ($0 != "Ubuntu ID: ubuntu") reject()
      next
    }
    NR == 13 {
      if ($0 != "Ubuntu version ID: 24.04") reject()
      next
    }
    NR == 14 {
      if ($0 != "Kernel architecture: aarch64") reject()
      next
    }
    NR == 15 {
      if ($0 != "Debian architecture: arm64") reject()
      next
    }
    NR == 16 {
      if ($0 != "Rust host: " target) reject()
      next
    }
    NR == 17 {
      if ($0 !~ /^Runner label: [^[:space:]][^\t]*$/) reject()
      next
    }
    NR == 18 {
      if ($0 != "Runner architecture: ARM64") reject()
      next
    }
    NR == 19 {
      if ($0 !~ /^Runner image OS: [^[:space:]][^\t]*$/) reject()
      next
    }
    NR == 20 {
      if ($0 !~ /^Runner image version: [^[:space:]][^\t]*$/) reject()
      next
    }
    NR == 21 {
      if ($0 !~ /^Rust compiler: rustc 1\.98\.0( |$)/) reject()
      next
    }
    NR == 22 {
      if ($0 !~ /^Cargo: cargo 1\.98\.0( |$)/) reject()
      next
    }
    NR == 23 {
      if ($0 !~ /^Required GLIBC symbols through: GLIBC_[0-9][0-9.]*$/) reject()
      next
    }
    NR == 24 {
      if ($0 !~ /^Validation runtime: GLIBC [0-9][0-9.]*$/) reject()
      next
    }
    NR == 25 {
      if ($0 != \
        "Reproducibility comparison: independent clean workflow comparison required") {
        reject()
      }
      next
    }
    NR == 26 {
      if ($0 != "") reject()
      next
    }
    NR == 27 {
      if ($0 != "Native build packages:") reject()
      next
    }
    NR >= 28 && NR <= 31 {
      field_count = split($0, fields, "\t")
      package_name = fields[1]
      sub(/:arm64$/, "", package_name)
      expected_name = \
        NR == 28 ? "libgtk-4-dev" : \
        NR == 29 ? "libgtksourceview-5-dev" : \
        NR == 30 ? "libnotmuch-dev" : "libwebkitgtk-6.0-dev"
      if (field_count != 2 || package_name != expected_name ||
        fields[2] == "") {
        reject()
      }
      next
    }
    NR == 32 {
      if ($0 != "") reject()
      next
    }
    NR == 33 {
      if ($0 != "Runtime package baseline:") reject()
      next
    }
    NR >= 34 && NR <= 37 {
      field_count = split($0, fields, "\t")
      package_name = fields[1]
      sub(/:arm64$/, "", package_name)
      expected_name = \
        NR == 34 ? "libgtk-4-1" : \
        NR == 35 ? "libgtksourceview-5-0" : \
        NR == 36 ? "libnotmuch5t64" : "libwebkitgtk-6.0-4"
      if (field_count != 2 || package_name != expected_name ||
        fields[2] == "") {
        reject()
      }
      next
    }
    NR == 38 {
      if ($0 != "") reject()
      next
    }
    NR == 39 {
      if ($0 != "Direct ELF dependencies:") reject()
      next
    }
    NR >= 40 {
      if ($0 !~ /^[A-Za-z0-9_+.:][-A-Za-z0-9_+.:]*$/ ||
        (previous_dependency != "" && $0 <= previous_dependency)) {
        reject()
      }
      previous_dependency = $0
      dependency_count++
      next
    }
    END {
      if (NR < 40 || dependency_count == 0 || invalid) {
        exit 1
      }
    }
  ' "$evidence"; then
    printf 'reproducibility evidence is not canonical: %s\n' \
      "$evidence" >&2
    exit 65
  fi
}

validate_reproducibility_evidence \
  "$EVIDENCE_A/reproducibility-evidence.txt"
validate_reproducibility_evidence \
  "$EVIDENCE_B/reproducibility-evidence.txt"

extract_comparison_members() {
  archive=$1
  label=$2
  binary_output=$3
  build_info_output=$4
  listing="$WORK_ROOT/$label.members"
  binary_member="$BUNDLE_NAME/bin/notm"
  build_info_member="$BUNDLE_NAME/BUILD-INFO.txt"

  tar -tzf "$archive" >"$listing"
  if awk '
    /^\// || /(^|\/)\.\.($|\/)/ || /(^|\/)\.($|\/)/ {
      unsafe = 1
    }
    END {
      exit unsafe
    }
  ' "$listing"; then
    :
  else
    printf 'archive contains an unsafe path: %s\n' "$archive" >&2
    exit 65
  fi
  if [ "$(grep -Fxc -- "$binary_member" "$listing")" -ne 1 ] ||
    [ "$(grep -Fxc -- "$build_info_member" "$listing")" -ne 1 ]; then
    printf 'archive lacks unique comparison members: %s\n' "$archive" >&2
    exit 65
  fi
  tar -xOf "$archive" "$binary_member" >"$binary_output"
  tar -xOf "$archive" "$build_info_member" >"$build_info_output"
  if [ ! -s "$binary_output" ] || [ ! -s "$build_info_output" ]; then
    printf 'archive comparison member is empty: %s\n' "$archive" >&2
    exit 65
  fi
}

validate_build_info() {
  build_info=$1
  binary_sha256=$2

  for required_line in \
    "Version: $VERSION" \
    "Source tag: v$VERSION" \
    "Source commit: $SOURCE_COMMIT" \
    "Source date epoch: $SOURCE_DATE_EPOCH" \
    "Target: $TARGET" \
    "Cargo.lock SHA-256: $LOCK_SHA256" \
    "Release binary SHA-256: $binary_sha256" \
    'Binary reproducibility: independent clean workflow comparison required'; do
    if [ "$(grep -Fxc -- "$required_line" "$build_info")" -ne 1 ]; then
      printf 'archive build information is missing exactly once: %s\n' \
        "$required_line" >&2
      exit 65
    fi
  done
}

A_BINARY="$WORK_ROOT/a-notm"
B_BINARY="$WORK_ROOT/b-notm"
A_BUILD_INFO="$WORK_ROOT/a-BUILD-INFO.txt"
B_BUILD_INFO="$WORK_ROOT/b-BUILD-INFO.txt"
readonly A_BINARY B_BINARY A_BUILD_INFO B_BUILD_INFO
extract_comparison_members \
  "$RELEASE_A/$ARCHIVE_NAME" a "$A_BINARY" "$A_BUILD_INFO"
extract_comparison_members \
  "$RELEASE_B/$ARCHIVE_NAME" b "$B_BINARY" "$B_BUILD_INFO"

A_ACTUAL_BINARY_SHA256=$(sha256sum "$A_BINARY" | awk '{print $1}')
B_ACTUAL_BINARY_SHA256=$(sha256sum "$B_BINARY" | awk '{print $1}')
A_ACTUAL_ARCHIVE_SHA256=$(
  sha256sum "$RELEASE_A/$ARCHIVE_NAME" | awk '{print $1}'
)
B_ACTUAL_ARCHIVE_SHA256=$(
  sha256sum "$RELEASE_B/$ARCHIVE_NAME" | awk '{print $1}'
)
readonly \
  A_ACTUAL_BINARY_SHA256 B_ACTUAL_BINARY_SHA256 \
  A_ACTUAL_ARCHIVE_SHA256 B_ACTUAL_ARCHIVE_SHA256

if [ "$A_ACTUAL_BINARY_SHA256" != "$A_BINARY_SHA256" ] ||
  [ "$B_ACTUAL_BINARY_SHA256" != "$B_BINARY_SHA256" ]; then
  printf '%s\n' \
    'binary checksum evidence does not match an extracted release binary' >&2
  exit 65
fi
if [ "$A_ACTUAL_ARCHIVE_SHA256" != "$A_RELEASE_ARCHIVE_SHA256" ] ||
  [ "$A_ACTUAL_ARCHIVE_SHA256" != "$A_EVIDENCE_ARCHIVE_SHA256" ] ||
  [ "$B_ACTUAL_ARCHIVE_SHA256" != "$B_RELEASE_ARCHIVE_SHA256" ] ||
  [ "$B_ACTUAL_ARCHIVE_SHA256" != "$B_EVIDENCE_ARCHIVE_SHA256" ]; then
  printf '%s\n' \
    'archive checksum evidence does not match a release archive' >&2
  exit 65
fi

validate_build_info "$A_BUILD_INFO" "$A_ACTUAL_BINARY_SHA256"
validate_build_info "$B_BUILD_INFO" "$B_ACTUAL_BINARY_SHA256"

if ! cmp -s "$A_BINARY" "$B_BINARY"; then
  printf '%s\n' \
    'independent clean ARM64 builds produced different binaries' >&2
  sha256sum "$A_BINARY" "$B_BINARY" >&2
  exit 65
fi
if ! cmp -s "$RELEASE_A/$ARCHIVE_NAME" "$RELEASE_B/$ARCHIVE_NAME"; then
  printf '%s\n' \
    'independent clean ARM64 builds produced different archives' >&2
  sha256sum "$RELEASE_A/$ARCHIVE_NAME" \
    "$RELEASE_B/$ARCHIVE_NAME" >&2
  exit 65
fi
if ! cmp -s "$RELEASE_A/$CHECKSUM_NAME" "$RELEASE_B/$CHECKSUM_NAME"; then
  printf '%s\n' 'ARM64 release checksum fragments differ' >&2
  exit 65
fi
if ! cmp -s "$EVIDENCE_A/binary.sha256" "$EVIDENCE_B/binary.sha256"; then
  printf '%s\n' 'ARM64 binary checksum evidence differs' >&2
  exit 65
fi
if ! cmp -s "$EVIDENCE_A/archive.sha256" "$EVIDENCE_B/archive.sha256"; then
  printf '%s\n' 'ARM64 archive checksum evidence differs' >&2
  exit 65
fi
if ! cmp -s \
  "$EVIDENCE_A/reproducibility-evidence.txt" \
  "$EVIDENCE_B/reproducibility-evidence.txt"; then
  printf '%s\n' 'canonical ARM64 reproducibility evidence differs:' >&2
  diff -u \
    "$EVIDENCE_A/reproducibility-evidence.txt" \
    "$EVIDENCE_B/reproducibility-evidence.txt" >&2 || :
  exit 65
fi

printf 'Verified independent ARM64 binary SHA-256: %s\n' \
  "$A_ACTUAL_BINARY_SHA256"
printf 'Verified independent ARM64 archive SHA-256: %s\n' \
  "$A_ACTUAL_ARCHIVE_SHA256"
printf '%s\n' 'Independent clean ARM64 release builds match'
