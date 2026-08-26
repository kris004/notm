#!/bin/sh

# Build and test a clean release source archive without repository metadata.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-source-archive.XXXXXX")
readonly WORK_ROOT

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
  # A clean checkout proves archive provenance against the exact source commit.
  SOURCE_ROOT=$PROJECT_ROOT
  SOURCE_COMMIT=$(git -C "$PROJECT_ROOT" rev-parse HEAD)
else
  # Validate the current uncommitted implementation through an isolated commit;
  # CI and released archives take the corresponding clean-source paths.
  SOURCE_ROOT="$WORK_ROOT/source-repository"
  mkdir "$SOURCE_ROOT"
  (
    cd -- "$PROJECT_ROOT"
    git ls-files --cached --others --exclude-standard -z |
      tar --null -T - -cf -
  ) | tar -xf - -C "$SOURCE_ROOT"
  git init --quiet --initial-branch=main "$SOURCE_ROOT"
  git -C "$SOURCE_ROOT" config user.name 'notm source archive test'
  git -C "$SOURCE_ROOT" config user.email 'release@example.invalid'
  git -C "$SOURCE_ROOT" add .
  GIT_AUTHOR_DATE=2000-01-01T00:00:00Z \
    GIT_COMMITTER_DATE=2000-01-01T00:00:00Z \
    git -C "$SOURCE_ROOT" commit --quiet -m 'source archive smoke'
  SOURCE_COMMIT=$(git -C "$SOURCE_ROOT" rev-parse HEAD)
fi
readonly SOURCE_ROOT SOURCE_COMMIT

VERSION=$(
  "$SOURCE_ROOT/packaging/verify-release-metadata.py" \
    --print-version "$SOURCE_ROOT"
)
TARGET=x86_64-unknown-linux-gnu
FAKE_BINARY="$WORK_ROOT/notm"
BUILD_INFO="$WORK_ROOT/BUILD-INFO.txt"
DIST_DIR="$WORK_ROOT/dist"
EXTRACT_ROOT="$WORK_ROOT/extract"
CARGO_TARGET_DIR="$WORK_ROOT/cargo-target"
readonly \
  VERSION TARGET FAKE_BINARY BUILD_INFO DIST_DIR EXTRACT_ROOT CARGO_TARGET_DIR
export CARGO_TARGET_DIR

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
Build image: source-archive-smoke
EOF

SOURCE_DATE_EPOCH=0 \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$SOURCE_ROOT/packaging/build-linux-release.sh" \
    "$SOURCE_ROOT" \
    "$VERSION" \
    "$TARGET" \
    "$FAKE_BINARY" \
    "$BUILD_INFO" \
    "$DIST_DIR" >/dev/null
"$SOURCE_ROOT/packaging/verify-linux-release.sh" \
  "$DIST_DIR" "$VERSION" "$TARGET" "$SOURCE_COMMIT" >/dev/null

mkdir "$EXTRACT_ROOT"
tar -xzf "$DIST_DIR/notm-v${VERSION}-src.tar.gz" -C "$EXTRACT_ROOT"
ARCHIVE_ROOT="$EXTRACT_ROOT/notm-${VERSION}"
readonly ARCHIVE_ROOT
test -d "$ARCHIVE_ROOT"
test ! -e "$ARCHIVE_ROOT/.git"
if git -C "$ARCHIVE_ROOT" rev-parse --show-toplevel >/dev/null 2>&1; then
  printf '%s\n' 'source archive unexpectedly resolves a Git worktree' >&2
  exit 1
fi

"$ARCHIVE_ROOT/packaging/verify-release-metadata.py" \
  --expected-version "$VERSION" \
  --expected-source-commit "$SOURCE_COMMIT" \
  --require-archive-provenance \
  "$ARCHIVE_ROOT" >/dev/null

(
  cd -- "$ARCHIVE_ROOT"
  cargo build --release --locked -p notm-app
  cargo test --locked --workspace --all-targets --all-features
  cargo run --locked -p notm-app -- fixture-smoke
  make check-packaging
)

printf 'source_archive_smoke ok: version=%s commit=%s\n' \
  "$VERSION" "$SOURCE_COMMIT"
