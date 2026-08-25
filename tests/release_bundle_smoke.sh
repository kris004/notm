#!/bin/sh

# Exercise the release-bundle layout without using a workstation build.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-smoke.XXXXXX")
readonly WORK_ROOT
FAKE_BINARY="$WORK_ROOT/notm"
BUILD_INFO="$WORK_ROOT/build-info.txt"
DIST_DIR="$WORK_ROOT/dist"
SECOND_DIST_DIR="$WORK_ROOT/dist-second"
EXTRACT_DIR="$WORK_ROOT/extract"
readonly FAKE_BINARY BUILD_INFO DIST_DIR SECOND_DIST_DIR EXTRACT_DIR

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

cat > "$FAKE_BINARY" <<'EOF'
#!/bin/sh
printf '%s\n' 'notm 0.1.0-test'
EOF
chmod 755 "$FAKE_BINARY"
printf '%s\n' 'fixture build information' > "$BUILD_INFO"

SOURCE_DATE_EPOCH=0 \
  "$PROJECT_ROOT/packaging/build-linux-release.sh" \
    "$PROJECT_ROOT" \
    0.1.0-test \
    x86_64-unknown-linux-gnu \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$DIST_DIR" > /dev/null
SOURCE_DATE_EPOCH=0 \
  "$PROJECT_ROOT/packaging/build-linux-release.sh" \
    "$PROJECT_ROOT" \
    0.1.0-test \
    x86_64-unknown-linux-gnu \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$SECOND_DIST_DIR" > /dev/null

archive_name=notm-v0.1.0-test-x86_64-unknown-linux-gnu.tar.gz
archive="$DIST_DIR/$archive_name"
second_archive="$SECOND_DIST_DIR/$archive_name"
source_archive_name=notm-v0.1.0-test-src.tar.gz
source_archive="$DIST_DIR/$source_archive_name"
second_source_archive="$SECOND_DIST_DIR/$source_archive_name"
bundle="$EXTRACT_DIR/notm-v0.1.0-test-x86_64-unknown-linux-gnu"
source_bundle="$EXTRACT_DIR/notm-0.1.0-test"
cmp "$archive" "$second_archive"
cmp "$source_archive" "$second_source_archive"
cmp "$DIST_DIR/SHA256SUMS" "$SECOND_DIST_DIR/SHA256SUMS"
test "$(wc -l < "$DIST_DIR/SHA256SUMS")" -eq 2
mkdir -p -- "$EXTRACT_DIR"
(
  cd -- "$DIST_DIR"
  sha256sum --check SHA256SUMS
)
tar -xzf "$archive" -C "$EXTRACT_DIR"
tar -xzf "$source_archive" -C "$EXTRACT_DIR"

test -x "$bundle/bin/notm"
test -f "$bundle/INSTALL.md"
test -f "$bundle/LICENSE"
test -f "$bundle/BUILD-INFO.txt"
test -f "$bundle/share/applications/io.github.kris004.notm.desktop"
test -f "$bundle/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
test -f "$bundle/share/metainfo/io.github.kris004.notm.metainfo.xml"
test -f "$bundle/share/man/man1/notm.1"
test -f "$bundle/share/man/man5/notm-config.5"
test -f "$bundle/share/man/man7/notm-test-harness.7"
test -f "$bundle/share/man/man7/notm-automation.7"
test -f "$source_bundle/Cargo.toml"
test -f "$source_bundle/Cargo.lock"
test -f "$source_bundle/docs/release-signing-key.asc"
test -f "$source_bundle/packaging/build-linux-release.sh"
test -x "$source_bundle/packaging/verify-release-tag.sh"
test -x "$source_bundle/tests/release_tag_smoke.sh"

test "$(stat -c '%a' "$bundle/bin/notm")" = 755
test "$(stat -c '%a' "$bundle/LICENSE")" = 644
test "$("$bundle/bin/notm")" = 'notm 0.1.0-test'
grep -Fx 'Exec=notm launch %u' \
  "$bundle/share/applications/io.github.kris004.notm.desktop" > /dev/null
grep -Fx 'TryExec=notm' \
  "$bundle/share/applications/io.github.kris004.notm.desktop" > /dev/null
grep -Fx 'MimeType=x-scheme-handler/mailto;' \
  "$bundle/share/applications/io.github.kris004.notm.desktop" > /dev/null
grep -Fx '    <mediatype>x-scheme-handler/mailto</mediatype>' \
  "$bundle/share/metainfo/io.github.kris004.notm.metainfo.xml" > /dev/null
desktop-file-validate \
  "$bundle/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$bundle/share/metainfo/io.github.kris004.notm.metainfo.xml"

test "$(tar -tzf "$archive" | sed -n '1p')" = \
  'notm-v0.1.0-test-x86_64-unknown-linux-gnu/'
test "$(tar -tvzf "$archive" | sed -n '2p' | awk '{print $2}')" = '0/0'
test "$(tar -tzf "$source_archive" | sed -n '1p')" = \
  'notm-0.1.0-test/'

if SOURCE_DATE_EPOCH=0 \
  "$PROJECT_ROOT/packaging/build-linux-release.sh" \
    "$PROJECT_ROOT" \
    0.1.0-test \
    x86_64-unknown-linux-gnu \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$DIST_DIR" >"$WORK_ROOT/overwrite.out" 2>&1; then
  printf '%s\n' 'release packaging unexpectedly overwrote an artifact' >&2
  exit 1
fi
grep -F 'refusing to overwrite release artifact:' \
  "$WORK_ROOT/overwrite.out" > /dev/null
