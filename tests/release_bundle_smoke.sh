#!/bin/sh

# Exercise deterministic bundle assembly and the standalone release verifier.

set -eu

# Exercise the release assembler from the private gate environment that
# exposed caller-umask-dependent payload modes.
umask 077

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-smoke.XXXXXX")
DETERMINISTIC_EPOCH=1700000000
readonly WORK_ROOT DETERMINISTIC_EPOCH
FAKE_BINARY="$WORK_ROOT/notm"
BUILD_INFO="$WORK_ROOT/build-info.txt"
DIST_DIR="$WORK_ROOT/dist"
SECOND_DIST_DIR="$WORK_ROOT/dist-second"
WRONG_DIST_DIR="$WORK_ROOT/dist-wrong-version"
SYMLINK_DIST_TARGET="$WORK_ROOT/dist-symlink-target"
SYMLINK_DIST="$WORK_ROOT/dist-symlink"
EXTRACT_DIR="$WORK_ROOT/extract"
readonly \
  FAKE_BINARY BUILD_INFO DIST_DIR SECOND_DIST_DIR WRONG_DIST_DIR \
  SYMLINK_DIST_TARGET SYMLINK_DIST EXTRACT_DIR

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

archival_commit=$(
  sed -n 's/^commit=\([0-9a-f]\{40\}\)$/\1/p' \
    "$PROJECT_ROOT/.git_archival.txt"
)
if [ -n "$archival_commit" ]; then
  SOURCE_ROOT=$PROJECT_ROOT
  SOURCE_COMMIT=$archival_commit
elif git_root=$(git -C "$PROJECT_ROOT" rev-parse --show-toplevel 2>/dev/null) &&
  [ "$git_root" = "$PROJECT_ROOT" ] &&
  [ -z "$(git -C "$PROJECT_ROOT" status --porcelain --untracked-files=all)" ]; then
  # A clean checkout exercises the exact commit and archive used in CI.
  SOURCE_ROOT=$PROJECT_ROOT
  SOURCE_COMMIT=$(git -C "$PROJECT_ROOT" rev-parse HEAD)
else
  # Include current worktree changes in pre-commit validation. A temporary
  # repository also exercises the exact git-archive path used in production.
  SOURCE_ROOT="$WORK_ROOT/source-repository"
  mkdir "$SOURCE_ROOT"
  (
    cd -- "$PROJECT_ROOT"
    git ls-files --cached --others --exclude-standard -z |
      tar --null -T - -cf -
  ) | tar -xf - -C "$SOURCE_ROOT"
  git init --quiet --initial-branch=main "$SOURCE_ROOT"
  git -C "$SOURCE_ROOT" config user.name 'notm release test'
  git -C "$SOURCE_ROOT" config user.email 'release@example.invalid'
  git -C "$SOURCE_ROOT" add .
  GIT_AUTHOR_DATE=2000-01-01T00:00:00Z \
    GIT_COMMITTER_DATE=2000-01-01T00:00:00Z \
    git -C "$SOURCE_ROOT" commit --quiet -m 'release smoke source'
  SOURCE_COMMIT=$(git -C "$SOURCE_ROOT" rev-parse HEAD)
fi
readonly SOURCE_ROOT SOURCE_COMMIT

VERSION=$(
  "$SOURCE_ROOT/packaging/verify-release-metadata.py" \
    --print-version "$SOURCE_ROOT"
)
TARGET=x86_64-unknown-linux-gnu
readonly VERSION TARGET

cat > "$FAKE_BINARY" <<EOF
#!/bin/sh
printf '%s\n' 'notm $VERSION'
EOF
chmod 755 "$FAKE_BINARY"
cat > "$BUILD_INFO" <<EOF
Version: $VERSION
Source tag: v$VERSION
Source commit: $SOURCE_COMMIT
Target: $TARGET
Build image: release-smoke-fixture
EOF

SOURCE_DATE_EPOCH=$DETERMINISTIC_EPOCH \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "$VERSION" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$DIST_DIR" > /dev/null
# Ensure the two tar invocations cannot inherit the same wall-clock second.
# The archive-mode PAX global header must still follow SOURCE_DATE_EPOCH.
sleep 2
SOURCE_DATE_EPOCH=$DETERMINISTIC_EPOCH \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "$VERSION" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$SECOND_DIST_DIR" > /dev/null

"$SOURCE_ROOT/packaging/verify-linux-release.sh" \
  "$DIST_DIR" "$VERSION" "$TARGET" "$SOURCE_COMMIT" >/dev/null
"$SOURCE_ROOT/packaging/verify-linux-release.sh" \
  "$SECOND_DIST_DIR" "$VERSION" "$TARGET" "$SOURCE_COMMIT" >/dev/null

archive_name="notm-v${VERSION}-${TARGET}.tar.gz"
archive="$DIST_DIR/$archive_name"
second_archive="$SECOND_DIST_DIR/$archive_name"
source_archive_name="notm-v${VERSION}-src.tar.gz"
source_archive="$DIST_DIR/$source_archive_name"
second_source_archive="$SECOND_DIST_DIR/$source_archive_name"
bundle="$EXTRACT_DIR/notm-v${VERSION}-${TARGET}"
source_bundle="$EXTRACT_DIR/notm-${VERSION}"
cmp "$archive" "$second_archive"
cmp "$source_archive" "$second_source_archive"
cmp "$DIST_DIR/SHA256SUMS" "$SECOND_DIST_DIR/SHA256SUMS"
test "$(wc -l < "$DIST_DIR/SHA256SUMS")" -eq 2
mkdir -p -- "$EXTRACT_DIR"
# The gate runs with a private umask. Preserve the archive's declared modes so
# the assertions below test the release payload rather than the caller's umask.
tar --same-permissions -xzf "$archive" -C "$EXTRACT_DIR"
tar --same-permissions -xzf "$source_archive" -C "$EXTRACT_DIR"

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
test -f "$source_bundle/.git_archival.txt"
test -f "$source_bundle/docs/release-signing-key.asc"
test -x "$source_bundle/packaging/build-linux-release.sh"
test -x "$source_bundle/packaging/verify-linux-release.sh"
test -x "$source_bundle/packaging/verify-release-metadata.py"
test -x "$source_bundle/packaging/verify-release-tag.sh"
test -x "$source_bundle/tests/release_tag_smoke.sh"
test ! -e "$source_bundle/.git"

test "$(stat -c '%a' "$bundle/bin/notm")" = 755
test "$(stat -c '%a' "$bundle")" = 755
test "$(stat -c '%a' "$bundle/bin")" = 755
test "$(stat -c '%a' "$bundle/share")" = 755
test "$(stat -c '%a' "$bundle/share/applications")" = 755
test "$(stat -c '%a' "$bundle/LICENSE")" = 644
test "$(stat -c '%a' "$bundle/INSTALL.md")" = 644
test "$(stat -c '%a' "$bundle/BUILD-INFO.txt")" = 644
test "$(stat -c '%a' "$bundle/share/applications/io.github.kris004.notm.desktop")" = 644
test "$(stat -c '%a' "$bundle/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg")" = 644
test "$(stat -c '%a' "$bundle/share/metainfo/io.github.kris004.notm.metainfo.xml")" = 644
test "$(stat -c '%a' "$bundle/share/man/man1/notm.1")" = 644
test "$("$bundle/bin/notm" --version)" = "notm $VERSION"
gzip -dc "$source_archive" > "$WORK_ROOT/source.tar"
gzip -dc "$second_source_archive" > "$WORK_ROOT/source-second.tar"
test "$(git get-tar-commit-id < "$WORK_ROOT/source.tar")" = "$SOURCE_COMMIT"
test "$(git get-tar-commit-id < "$WORK_ROOT/source-second.tar")" = \
  "$SOURCE_COMMIT"
"$source_bundle/packaging/verify-release-metadata.py" \
  --expected-version "$VERSION" \
  --expected-source-commit "$SOURCE_COMMIT" \
  --require-archive-provenance \
  "$source_bundle" >/dev/null

test "$(tar -tzf "$archive" | sed -n '1p')" = \
  "notm-v${VERSION}-${TARGET}/"
test "$(tar -tvzf "$archive" | sed -n '2p' | awk '{print $2}')" = '0/0'
test "$(tar -tzf "$source_archive" | sed -n '1p')" = \
  "notm-${VERSION}/"

if SOURCE_DATE_EPOCH=0 \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "$VERSION" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$DIST_DIR" >"$WORK_ROOT/overwrite.out" 2>&1; then
  printf '%s\n' 'release packaging unexpectedly overwrote an artifact' >&2
  exit 1
fi
grep -F 'refusing to overwrite release artifact:' \
  "$WORK_ROOT/overwrite.out" >/dev/null

if SOURCE_DATE_EPOCH=0 \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "${VERSION}-drift" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$WRONG_DIST_DIR" >"$WORK_ROOT/wrong-version.out" 2>&1; then
  printf '%s\n' 'release packaging accepted a metadata version mismatch' >&2
  exit 1
fi
grep -F 'release version does not match verified metadata:' \
  "$WORK_ROOT/wrong-version.out" >/dev/null

mkdir "$SYMLINK_DIST_TARGET"
ln -s "$SYMLINK_DIST_TARGET" "$SYMLINK_DIST"
if SOURCE_DATE_EPOCH=$DETERMINISTIC_EPOCH \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "$VERSION" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$SYMLINK_DIST" >"$WORK_ROOT/symlink-output.out" 2>&1; then
  printf '%s\n' 'release packaging accepted a symlink output directory' >&2
  exit 1
fi
grep -F 'release output directory must not be a symlink:' \
  "$WORK_ROOT/symlink-output.out" >/dev/null
test -z "$(find "$SYMLINK_DIST_TARGET" -mindepth 1 -print -quit)"

printf '%s\n' 'release_bundle_smoke ok'
