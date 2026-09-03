#!/bin/sh

# Combine independently verified x86_64/source and ARM64 release fragments.

set -eu

TARGET_X86_64=x86_64-unknown-linux-gnu
TARGET_ARM64=aarch64-unknown-linux-gnu
readonly TARGET_X86_64 TARGET_ARM64

usage() {
  printf '%s\n' \
    "usage: $0 X86_64_DIR ARM64_DIR OUTPUT_DIR VERSION SOURCE_COMMIT ARM64_SHA256" >&2
}

if [ "$#" -ne 6 ]; then
  usage
  exit 64
fi

X86_64_DIR=$1
ARM64_DIR=$2
OUTPUT_DIR=$3
VERSION=$4
SOURCE_COMMIT=$5
EXPECTED_ARM64_SHA256=$6

case "$VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid release version: %s\n' "$VERSION" >&2
    exit 64
    ;;
esac
if ! printf '%s\n' "$SOURCE_COMMIT" | grep -Eq '^[0-9a-f]{40}$'; then
  printf 'invalid source commit: %s\n' "$SOURCE_COMMIT" >&2
  exit 64
fi
if ! printf '%s\n' "$EXPECTED_ARM64_SHA256" |
  grep -Eq '^[0-9a-f]{64}$'; then
  printf 'invalid expected ARM64 SHA-256: %s\n' \
    "$EXPECTED_ARM64_SHA256" >&2
  exit 64
fi

resolve_directory() {
  directory=$1
  label=$2
  if [ ! -d "$directory" ] || [ -L "$directory" ]; then
    printf '%s is not a real directory: %s\n' "$label" "$directory" >&2
    exit 66
  fi
  (
    CDPATH=''
    cd -- "$directory"
    pwd -P
  )
}

X86_64_DIR=$(resolve_directory "$X86_64_DIR" 'x86_64 artifact input')
ARM64_DIR=$(resolve_directory "$ARM64_DIR" 'ARM64 artifact input')
if [ -e "$OUTPUT_DIR" ] || [ -L "$OUTPUT_DIR" ]; then
  OUTPUT_DIR=$(resolve_directory "$OUTPUT_DIR" 'aggregate output')
  if find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -print -quit |
    grep -q .; then
    printf 'aggregate output must be empty: %s\n' "$OUTPUT_DIR" >&2
    exit 73
  fi
else
  mkdir -p -- "$OUTPUT_DIR"
  OUTPUT_DIR=$(resolve_directory "$OUTPUT_DIR" 'aggregate output')
fi
if [ "$X86_64_DIR" = "$ARM64_DIR" ] || \
  [ "$OUTPUT_DIR" = "$X86_64_DIR" ] || \
  [ "$OUTPUT_DIR" = "$ARM64_DIR" ]; then
  printf '%s\n' 'release input and output directories must be distinct' >&2
  exit 73
fi
readonly \
  X86_64_DIR ARM64_DIR OUTPUT_DIR VERSION SOURCE_COMMIT \
  EXPECTED_ARM64_SHA256

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/../.."
  pwd -P
)
readonly PROJECT_ROOT
if [ "$(git -C "$PROJECT_ROOT" rev-parse HEAD)" != "$SOURCE_COMMIT" ]; then
  printf 'aggregate checkout does not match source commit: %s\n' \
    "$SOURCE_COMMIT" >&2
  exit 65
fi
if [ "$("$PROJECT_ROOT/packaging/verify-release-metadata.py" \
  --print-version "$PROJECT_ROOT")" != "$VERSION" ]; then
  printf 'aggregate checkout does not match release version: %s\n' \
    "$VERSION" >&2
  exit 65
fi

X86_64_ARCHIVE_NAME="notm-v${VERSION}-${TARGET_X86_64}.tar.gz"
ARM64_ARCHIVE_NAME="notm-v${VERSION}-${TARGET_ARM64}.tar.gz"
SOURCE_ARCHIVE_NAME="notm-v${VERSION}-src.tar.gz"
X86_64_CHECKSUM_NAME=SHA256SUMS
ARM64_CHECKSUM_NAME=SHA256SUMS-aarch64
readonly \
  X86_64_ARCHIVE_NAME ARM64_ARCHIVE_NAME SOURCE_ARCHIVE_NAME \
  X86_64_CHECKSUM_NAME ARM64_CHECKSUM_NAME

require_regular_file() {
  path=$1
  if [ ! -f "$path" ] || [ -L "$path" ]; then
    printf 'release fragment is missing or not a regular file: %s\n' \
      "$path" >&2
    exit 65
  fi
}

require_exact_directory_entries() {
  directory=$1
  expected=$2
  actual=$(find "$directory" -mindepth 1 -maxdepth 1 | wc -l)
  if [ "$actual" -ne "$expected" ]; then
    printf 'release fragment has %s entries instead of %s: %s\n' \
      "$actual" "$expected" "$directory" >&2
    find "$directory" -mindepth 1 -maxdepth 1 -print >&2
    exit 65
  fi
}

X86_64_ARCHIVE="$X86_64_DIR/$X86_64_ARCHIVE_NAME"
ARM64_ARCHIVE="$ARM64_DIR/$ARM64_ARCHIVE_NAME"
SOURCE_ARCHIVE="$X86_64_DIR/$SOURCE_ARCHIVE_NAME"
X86_64_CHECKSUM="$X86_64_DIR/$X86_64_CHECKSUM_NAME"
ARM64_CHECKSUM="$ARM64_DIR/$ARM64_CHECKSUM_NAME"
readonly \
  X86_64_ARCHIVE ARM64_ARCHIVE SOURCE_ARCHIVE X86_64_CHECKSUM \
  ARM64_CHECKSUM

require_exact_directory_entries "$X86_64_DIR" 3
require_exact_directory_entries "$ARM64_DIR" 2
for artifact in \
  "$X86_64_ARCHIVE" \
  "$ARM64_ARCHIVE" \
  "$SOURCE_ARCHIVE" \
  "$X86_64_CHECKSUM" \
  "$ARM64_CHECKSUM"; do
  require_regular_file "$artifact"
done

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-aggregate.XXXXXX")
readonly WORK_ROOT
cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

validate_checksum_fragment() {
  directory=$1
  checksum_name=$2
  expected_names=$3
  expected_count=$4
  parsed_names="$WORK_ROOT/${checksum_name}.names"
  expected_names_file="$WORK_ROOT/${checksum_name}.expected"

  if [ "$(wc -l <"$directory/$checksum_name")" -ne "$expected_count" ]; then
    printf '%s must contain exactly %s checksum entries\n' \
      "$checksum_name" "$expected_count" >&2
    exit 65
  fi
  if ! awk '
    $0 !~ /^[0-9a-f]{64}  [^/]+$/ { invalid = 1 }
    END { exit invalid }
  ' "$directory/$checksum_name"; then
    printf '%s contains a non-canonical checksum entry\n' \
      "$checksum_name" >&2
    exit 65
  fi
  sed -n 's/^[0-9a-f]\{64\}  //p' "$directory/$checksum_name" |
    LC_ALL=C sort >"$parsed_names"
  printf '%s\n' "$expected_names" | tr ' ' '\n' |
    LC_ALL=C sort >"$expected_names_file"
  if ! cmp -s "$expected_names_file" "$parsed_names"; then
    printf '%s names do not match the expected release fragment\n' \
      "$checksum_name" >&2
    diff -u "$expected_names_file" "$parsed_names" >&2 || :
    exit 65
  fi
  (
    cd -- "$directory"
    sha256sum --check --strict "$checksum_name"
  )
}

validate_checksum_fragment \
  "$X86_64_DIR" \
  "$X86_64_CHECKSUM_NAME" \
  "$X86_64_ARCHIVE_NAME $SOURCE_ARCHIVE_NAME" \
  2
validate_checksum_fragment \
  "$ARM64_DIR" \
  "$ARM64_CHECKSUM_NAME" \
  "$ARM64_ARCHIVE_NAME" \
  1

ACTUAL_ARM64_SHA256=$(sha256sum "$ARM64_ARCHIVE" | awk '{print $1}')
readonly ACTUAL_ARM64_SHA256
if [ "$ACTUAL_ARM64_SHA256" != "$EXPECTED_ARM64_SHA256" ]; then
  printf 'ARM64 workflow digest mismatch: expected %s, got %s\n' \
    "$EXPECTED_ARM64_SHA256" "$ACTUAL_ARM64_SHA256" >&2
  exit 65
fi

validate_binary_archive_identity() {
  archive=$1
  bundle_name=$2
  target=$3
  listing="$WORK_ROOT/${target}.members"
  build_info="$WORK_ROOT/${target}.BUILD-INFO.txt"

  tar -tzf "$archive" >"$listing"
  if ! awk -v root="$bundle_name" '
    $0 == root "/" { next }
    index($0, root "/") == 1 && $0 !~ /(^|\/)\.\.($|\/)/ { next }
    { print "unexpected archive member: " $0 > "/dev/stderr"; invalid = 1 }
    END { exit invalid }
  ' "$listing"; then
    exit 65
  fi
  if [ "$(sed -n '1p' "$listing")" != "$bundle_name/" ]; then
    printf 'binary archive has an unexpected first member: %s\n' \
      "$archive" >&2
    exit 65
  fi
  tar -xOf "$archive" "$bundle_name/BUILD-INFO.txt" >"$build_info"
  for line in \
    "Version: $VERSION" \
    "Source commit: $SOURCE_COMMIT" \
    "Target: $target"; do
    if [ "$(grep -Fxc -- "$line" "$build_info")" -ne 1 ]; then
      printf 'binary archive identity is missing exactly once: %s\n' \
        "$line" >&2
      exit 65
    fi
  done
}

validate_binary_archive_identity \
  "$X86_64_ARCHIVE" \
  "notm-v${VERSION}-${TARGET_X86_64}" \
  "$TARGET_X86_64"
validate_binary_archive_identity \
  "$ARM64_ARCHIVE" \
  "notm-v${VERSION}-${TARGET_ARM64}" \
  "$TARGET_ARM64"

gzip -dc "$SOURCE_ARCHIVE" >"$WORK_ROOT/source.tar"
ARCHIVE_COMMIT=$(git get-tar-commit-id <"$WORK_ROOT/source.tar")
readonly ARCHIVE_COMMIT
if [ "$ARCHIVE_COMMIT" != "$SOURCE_COMMIT" ]; then
  printf 'source archive commit mismatch: expected %s, got %s\n' \
    "$SOURCE_COMMIT" "${ARCHIVE_COMMIT:-none}" >&2
  exit 65
fi

cp -- "$X86_64_ARCHIVE" "$OUTPUT_DIR/$X86_64_ARCHIVE_NAME"
cp -- "$ARM64_ARCHIVE" "$OUTPUT_DIR/$ARM64_ARCHIVE_NAME"
cp -- "$SOURCE_ARCHIVE" "$OUTPUT_DIR/$SOURCE_ARCHIVE_NAME"
cat "$X86_64_CHECKSUM" "$ARM64_CHECKSUM" |
  LC_ALL=C sort -k2,2 >"$WORK_ROOT/fragment-SHA256SUMS"
(
  cd -- "$OUTPUT_DIR"
  sha256sum \
    "$X86_64_ARCHIVE_NAME" \
    "$ARM64_ARCHIVE_NAME" \
    "$SOURCE_ARCHIVE_NAME" |
    LC_ALL=C sort -k2,2 >SHA256SUMS
)
if ! cmp -s "$WORK_ROOT/fragment-SHA256SUMS" \
  "$OUTPUT_DIR/SHA256SUMS"; then
  printf '%s\n' 'canonical checksums differ from validated input fragments' >&2
  diff -u "$WORK_ROOT/fragment-SHA256SUMS" \
    "$OUTPUT_DIR/SHA256SUMS" >&2 || :
  exit 65
fi

require_exact_directory_entries "$OUTPUT_DIR" 4
validate_checksum_fragment \
  "$OUTPUT_DIR" \
  SHA256SUMS \
  "$X86_64_ARCHIVE_NAME $ARM64_ARCHIVE_NAME $SOURCE_ARCHIVE_NAME" \
  3

printf 'canonical release assets: version=%s commit=%s ARM64_SHA256=%s\n' \
  "$VERSION" "$SOURCE_COMMIT" "$ACTUAL_ARM64_SHA256"
