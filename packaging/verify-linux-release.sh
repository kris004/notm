#!/bin/sh

# Verify a complete notm GNU/Linux binary/source release bundle.

set -eu

usage() {
  printf '%s\n' \
    "usage: $0 DIST_DIR VERSION TARGET SOURCE_COMMIT" >&2
}

if [ "$#" -ne 4 ]; then
  usage
  exit 2
fi

DIST_DIR=$1
VERSION=$2
TARGET=$3
SOURCE_COMMIT=$4

case "$VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid release version: %s\n' "$VERSION" >&2
    exit 2
    ;;
esac
case "$TARGET" in
  '' | *[!0-9A-Za-z._-]*)
    printf 'invalid release target: %s\n' "$TARGET" >&2
    exit 2
    ;;
esac
if ! printf '%s\n' "$SOURCE_COMMIT" | grep -Eq '^[0-9a-f]{40}$'; then
  printf 'invalid source commit: %s\n' "$SOURCE_COMMIT" >&2
  exit 2
fi
if [ ! -d "$DIST_DIR" ]; then
  printf 'release directory is not a directory: %s\n' "$DIST_DIR" >&2
  exit 2
fi

DIST_DIR=$(
  CDPATH=''
  cd -- "$DIST_DIR"
  pwd
)
BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
SOURCE_NAME="notm-${VERSION}"
BINARY_ARCHIVE="$DIST_DIR/$BUNDLE_NAME.tar.gz"
SOURCE_ARCHIVE="$DIST_DIR/notm-v${VERSION}-src.tar.gz"
CHECKSUM="$DIST_DIR/SHA256SUMS"
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-verify.XXXXXX")
readonly \
  DIST_DIR VERSION TARGET SOURCE_COMMIT BUNDLE_NAME SOURCE_NAME \
  BINARY_ARCHIVE SOURCE_ARCHIVE CHECKSUM WORK_ROOT

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

for artifact in "$BINARY_ARCHIVE" "$SOURCE_ARCHIVE" "$CHECKSUM"; do
  if [ ! -f "$artifact" ] || [ -L "$artifact" ]; then
    printf 'release artifact is missing or not a regular file: %s\n' \
      "$artifact" >&2
    exit 1
  fi
done
if [ "$(find "$DIST_DIR" -mindepth 1 -maxdepth 1 | wc -l)" -ne 3 ]; then
  printf '%s\n' 'release directory must contain exactly three artifacts:' >&2
  find "$DIST_DIR" -mindepth 1 -maxdepth 1 -print >&2
  exit 1
fi

if [ "$(wc -l < "$CHECKSUM")" -ne 2 ] ||
  ! grep -Eq '^[0-9a-f]{64}  notm-v[^/]+\.tar\.gz$' "$CHECKSUM"; then
  printf '%s\n' 'SHA256SUMS does not contain two canonical archive entries' >&2
  exit 1
fi
sed -n 's/^[0-9a-f]\{64\}  //p' "$CHECKSUM" | LC_ALL=C sort \
  > "$WORK_ROOT/checksum-names"
printf '%s\n' \
  "$(basename -- "$BINARY_ARCHIVE")" \
  "$(basename -- "$SOURCE_ARCHIVE")" | LC_ALL=C sort \
  > "$WORK_ROOT/expected-names"
if ! cmp -s "$WORK_ROOT/expected-names" "$WORK_ROOT/checksum-names"; then
  printf '%s\n' 'SHA256SUMS names do not match the expected release archives' >&2
  diff -u "$WORK_ROOT/expected-names" "$WORK_ROOT/checksum-names" >&2 || :
  exit 1
fi
(
  cd -- "$DIST_DIR"
  sha256sum --check "$(basename -- "$CHECKSUM")"
)

validate_archive_root() {
  archive=$1
  expected_root=$2
  listing=$3
  tar -tzf "$archive" > "$listing"
  if ! awk -v root="$expected_root" '
    $0 == root "/" { next }
    index($0, root "/") == 1 && $0 !~ /(^|\/)\.\.($|\/)/ { next }
    { print "unexpected archive member: " $0 > "/dev/stderr"; invalid = 1 }
    END { exit invalid }
  ' "$listing"; then
    return 1
  fi
  test "$(sed -n '1p' "$listing")" = "$expected_root/"
}

validate_archive_root \
  "$BINARY_ARCHIVE" "$BUNDLE_NAME" "$WORK_ROOT/binary-members"
validate_archive_root \
  "$SOURCE_ARCHIVE" "$SOURCE_NAME" "$WORK_ROOT/source-members"

gzip -dc "$SOURCE_ARCHIVE" > "$WORK_ROOT/source.tar"
archive_commit=$(git get-tar-commit-id < "$WORK_ROOT/source.tar")
if [ "$archive_commit" != "$SOURCE_COMMIT" ]; then
  printf 'source archive PAX commit mismatch: expected %s, got %s\n' \
    "$SOURCE_COMMIT" "${archive_commit:-none}" >&2
  exit 1
fi

mkdir "$WORK_ROOT/binary" "$WORK_ROOT/source"
tar -xzf "$BINARY_ARCHIVE" -C "$WORK_ROOT/binary"
tar -xf "$WORK_ROOT/source.tar" -C "$WORK_ROOT/source"
BUNDLE_ROOT="$WORK_ROOT/binary/$BUNDLE_NAME"
SOURCE_ROOT="$WORK_ROOT/source/$SOURCE_NAME"
readonly BUNDLE_ROOT SOURCE_ROOT

if find "$BUNDLE_ROOT" "$SOURCE_ROOT" -type l -print -quit | grep -q .; then
  printf '%s\n' 'release archives must not contain symbolic links' >&2
  exit 1
fi
test ! -e "$SOURCE_ROOT/.git"

required_bundle_files='
bin/notm
BUILD-INFO.txt
CHANGELOG.md
INSTALL.md
LICENSE
share/applications/io.github.kris004.notm.desktop
share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg
share/man/man1/notm.1
share/man/man5/notm-config.5
share/man/man7/notm-automation.7
share/man/man7/notm-test-harness.7
share/metainfo/io.github.kris004.notm.metainfo.xml
'
printf '%s' "$required_bundle_files" |
  while IFS= read -r relative; do
    [ -z "$relative" ] || test -f "$BUNDLE_ROOT/$relative"
  done
test -x "$BUNDLE_ROOT/bin/notm"
test "$("$BUNDLE_ROOT/bin/notm" --version)" = "notm $VERSION"

grep -Fx "Version: $VERSION" "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx "Source tag: v$VERSION" "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx "Source commit: $SOURCE_COMMIT" "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx "Target: $TARGET" "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null

"$SOURCE_ROOT/packaging/verify-release-metadata.py" \
  --expected-version "$VERSION" \
  --expected-source-commit "$SOURCE_COMMIT" \
  --require-archive-provenance \
  "$SOURCE_ROOT" >/dev/null

for relative in \
  CHANGELOG.md \
  LICENSE \
  docs/man/notm.1 \
  docs/man/notm-config.5 \
  docs/man/notm-automation.7 \
  docs/man/notm-test-harness.7 \
  packaging/io.github.kris004.notm.metainfo.xml
do
  case "$relative" in
    docs/man/*)
      installed="share/man/man${relative##*.}/${relative##*/}"
      ;;
    packaging/*.metainfo.xml)
      installed="share/metainfo/${relative##*/}"
      ;;
    *)
      installed=${relative##*/}
      ;;
  esac
  cmp "$SOURCE_ROOT/$relative" "$BUNDLE_ROOT/$installed"
done

desktop-file-validate \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"
grep -Fx 'Exec=notm launch %u' \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop" >/dev/null
grep -Fx 'TryExec=notm' \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop" >/dev/null

printf 'release bundle verified: version=%s target=%s commit=%s\n' \
  "$VERSION" "$TARGET" "$SOURCE_COMMIT"
