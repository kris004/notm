#!/bin/sh

# Validate, install, execute, and uninstall an aarch64 release archive.

set -eu

TARGET=aarch64-unknown-linux-gnu
CHECKSUM_NAME=SHA256SUMS-aarch64
readonly TARGET CHECKSUM_NAME

usage() {
  printf '%s\n' \
    "usage: $0 SOURCE_ROOT VERSION SOURCE_COMMIT ARCHIVE CHECKSUM" >&2
}

if [ "$#" -ne 5 ]; then
  usage
  exit 64
fi

SOURCE_ROOT=$1
VERSION=$2
SOURCE_COMMIT=$3
ARCHIVE=$4
CHECKSUM=$5

case "$VERSION" in
  '' | *[!0-9A-Za-z.+-]*)
    printf 'invalid release version: %s\n' "$VERSION" >&2
    exit 64
    ;;
esac
if [ ! -d "$SOURCE_ROOT" ]; then
  printf 'source root is not a directory: %s\n' "$SOURCE_ROOT" >&2
  exit 66
fi
if [ ! -f "$ARCHIVE" ] || [ -L "$ARCHIVE" ]; then
  printf 'archive is not a regular file: %s\n' "$ARCHIVE" >&2
  exit 66
fi
if [ ! -f "$CHECKSUM" ] || [ -L "$CHECKSUM" ]; then
  printf 'checksum is not a regular file: %s\n' "$CHECKSUM" >&2
  exit 66
fi
if [ "$(basename -- "$CHECKSUM")" != "$CHECKSUM_NAME" ]; then
  printf 'unexpected ARM64 checksum filename: %s\n' "$CHECKSUM" >&2
  exit 65
fi

SOURCE_ROOT=$(
  CDPATH=''
  cd -- "$SOURCE_ROOT"
  pwd -P
)
ARCHIVE=$(
  CDPATH=''
  cd -- "$(dirname -- "$ARCHIVE")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename -- "$ARCHIVE")"
)
CHECKSUM=$(
  CDPATH=''
  cd -- "$(dirname -- "$CHECKSUM")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename -- "$CHECKSUM")"
)
if ! SOURCE_COMMIT=$(
  git -C "$SOURCE_ROOT" rev-parse --verify "${SOURCE_COMMIT}^{commit}"
); then
  printf 'source commit does not resolve to a commit\n' >&2
  exit 65
fi
readonly SOURCE_ROOT VERSION SOURCE_COMMIT ARCHIVE CHECKSUM

if [ "$(git -C "$SOURCE_ROOT" rev-parse HEAD)" != "$SOURCE_COMMIT" ]; then
  printf 'source commit is not the checked-out HEAD: %s\n' \
    "$SOURCE_COMMIT" >&2
  exit 65
fi

"$SOURCE_ROOT/packaging/verify-release-metadata.py" \
  --expected-version "$VERSION" \
  "$SOURCE_ROOT" >/dev/null

ARCHIVE_NAME="notm-v${VERSION}-${TARGET}.tar.gz"
BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
readonly ARCHIVE_NAME BUNDLE_NAME
if [ "$(basename -- "$ARCHIVE")" != "$ARCHIVE_NAME" ]; then
  printf 'unexpected ARM64 archive filename: %s\n' "$ARCHIVE" >&2
  exit 65
fi
if [ "$(dirname -- "$ARCHIVE")" != "$(dirname -- "$CHECKSUM")" ]; then
  printf '%s\n' 'archive and checksum must be in the same directory' >&2
  exit 65
fi

if [ "$(wc -l <"$CHECKSUM")" -ne 1 ]; then
  printf '%s must contain exactly one checksum\n' "$CHECKSUM_NAME" >&2
  exit 65
fi
if ! awk -v archive="$ARCHIVE_NAME" '
  $1 ~ /^[0-9a-f]{64}$/ && $2 == archive { valid++ }
  END { exit valid == 1 ? 0 : 1 }
' "$CHECKSUM"; then
  printf '%s does not contain exactly the expected archive entry\n' \
    "$CHECKSUM_NAME" >&2
  exit 65
fi
(
  cd -- "$(dirname -- "$ARCHIVE")"
  sha256sum --check --strict "$CHECKSUM_NAME"
)

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-verify.XXXXXX")
EXTRACT_ROOT="$WORK_ROOT/extract"
BUNDLE_ROOT="$EXTRACT_ROOT/$BUNDLE_NAME"
HOME_ROOT="$WORK_ROOT/home"
XDG_RUNTIME_ROOT="$WORK_ROOT/runtime"
EXPECTED_LIST="$WORK_ROOT/expected-list"
ACTUAL_LIST="$WORK_ROOT/actual-list"
OUTSIDE_SENTINEL="$WORK_ROOT/outside-sentinel"
readonly \
  WORK_ROOT EXTRACT_ROOT BUNDLE_ROOT HOME_ROOT XDG_RUNTIME_ROOT \
  EXPECTED_LIST ACTUAL_LIST OUTSIDE_SENTINEL

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -p -- "$EXTRACT_ROOT" "$HOME_ROOT" "$XDG_RUNTIME_ROOT"
chmod 700 "$XDG_RUNTIME_ROOT"
: >"$OUTSIDE_SENTINEL"

tar -tzf "$ARCHIVE" >"$ACTUAL_LIST"
if awk '
  /^\// || /(^|\/)\.\.($|\/)/ || /(^|\/)\.($|\/)/ { unsafe = 1 }
  END { exit unsafe ? 0 : 1 }
' "$ACTUAL_LIST"; then
  printf '%s\n' 'archive contains an unsafe path' >&2
  exit 65
fi

cat >"$EXPECTED_LIST" <<EOF
$BUNDLE_NAME/
$BUNDLE_NAME/BUILD-INFO.txt
$BUNDLE_NAME/CHANGELOG.md
$BUNDLE_NAME/INSTALL.md
$BUNDLE_NAME/LICENSE
$BUNDLE_NAME/bin/
$BUNDLE_NAME/bin/notm
$BUNDLE_NAME/share/
$BUNDLE_NAME/share/applications/
$BUNDLE_NAME/share/applications/io.github.kris004.notm.desktop
$BUNDLE_NAME/share/icons/
$BUNDLE_NAME/share/icons/hicolor/
$BUNDLE_NAME/share/icons/hicolor/scalable/
$BUNDLE_NAME/share/icons/hicolor/scalable/apps/
$BUNDLE_NAME/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg
$BUNDLE_NAME/share/man/
$BUNDLE_NAME/share/man/man1/
$BUNDLE_NAME/share/man/man1/notm.1
$BUNDLE_NAME/share/man/man5/
$BUNDLE_NAME/share/man/man5/notm-config.5
$BUNDLE_NAME/share/man/man7/
$BUNDLE_NAME/share/man/man7/notm-automation.7
$BUNDLE_NAME/share/man/man7/notm-test-harness.7
$BUNDLE_NAME/share/metainfo/
$BUNDLE_NAME/share/metainfo/io.github.kris004.notm.metainfo.xml
EOF
LC_ALL=C sort -o "$EXPECTED_LIST" "$EXPECTED_LIST"
LC_ALL=C sort -o "$ACTUAL_LIST" "$ACTUAL_LIST"
if ! cmp -s "$EXPECTED_LIST" "$ACTUAL_LIST"; then
  printf '%s\n' 'archive layout differs from the required ARM64 bundle layout:' >&2
  diff -u "$EXPECTED_LIST" "$ACTUAL_LIST" >&2 || true
  exit 65
fi

tar -xzf "$ARCHIVE" -C "$EXTRACT_ROOT"

test "$(stat -c '%a' "$BUNDLE_ROOT/bin/notm")" = 755
for regular_file in \
  "$BUNDLE_ROOT/BUILD-INFO.txt" \
  "$BUNDLE_ROOT/CHANGELOG.md" \
  "$BUNDLE_ROOT/INSTALL.md" \
  "$BUNDLE_ROOT/LICENSE" \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop" \
  "$BUNDLE_ROOT/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$BUNDLE_ROOT/share/man/man1/notm.1" \
  "$BUNDLE_ROOT/share/man/man5/notm-config.5" \
  "$BUNDLE_ROOT/share/man/man7/notm-automation.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-test-harness.7" \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"; do
  if [ ! -f "$regular_file" ] || [ -L "$regular_file" ]; then
    printf 'bundle entry is not a regular file: %s\n' "$regular_file" >&2
    exit 65
  fi
  if [ "$(stat -c '%a' "$regular_file")" != 644 ]; then
    printf 'bundle entry has unexpected permissions: %s\n' "$regular_file" >&2
    exit 65
  fi
done

cmp "$SOURCE_ROOT/LICENSE" "$BUNDLE_ROOT/LICENSE"
cmp "$SOURCE_ROOT/CHANGELOG.md" "$BUNDLE_ROOT/CHANGELOG.md"
cmp "$SOURCE_ROOT/packaging/io.github.kris004.notm.desktop" \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
cmp "$SOURCE_ROOT/packaging/io.github.kris004.notm.metainfo.xml" \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"
cmp \
  "$SOURCE_ROOT/packaging/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$BUNDLE_ROOT/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
cmp "$SOURCE_ROOT/docs/man/notm.1" "$BUNDLE_ROOT/share/man/man1/notm.1"
cmp "$SOURCE_ROOT/docs/man/notm-config.5" \
  "$BUNDLE_ROOT/share/man/man5/notm-config.5"
cmp "$SOURCE_ROOT/docs/man/notm-automation.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-automation.7"
cmp "$SOURCE_ROOT/docs/man/notm-test-harness.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-test-harness.7"

SOURCE_DATE_EPOCH=$(git -C "$SOURCE_ROOT" show -s --format=%ct "$SOURCE_COMMIT")
LOCK_SHA256=$(sha256sum "$SOURCE_ROOT/Cargo.lock" | awk '{print $1}')
readonly SOURCE_DATE_EPOCH LOCK_SHA256
for metadata_line in \
  "Version: $VERSION" \
  "Source tag: v$VERSION" \
  "Source commit: $SOURCE_COMMIT" \
  "Source date epoch: $SOURCE_DATE_EPOCH" \
  "Target: $TARGET" \
  'Build architecture: aarch64' \
  'Build environment: Ubuntu 24.04 ARM64 userspace' \
  "Cargo.lock SHA-256: $LOCK_SHA256"; do
  if [ "$(grep -Fxc -- "$metadata_line" "$BUNDLE_ROOT/BUILD-INFO.txt")" -ne 1 ]; then
    printf 'invalid or missing build metadata: %s\n' "$metadata_line" >&2
    exit 65
  fi
done
if [ "$(grep -Ec '^Rust compiler: rustc 1\.98\.0( |$)' \
  "$BUNDLE_ROOT/BUILD-INFO.txt")" -ne 1 ]; then
  printf '%s\n' 'build metadata does not pin rustc 1.98.0' >&2
  exit 65
fi
if [ "$(grep -Ec '^Cargo: cargo 1\.98\.0( |$)' \
  "$BUNDLE_ROOT/BUILD-INFO.txt")" -ne 1 ]; then
  printf '%s\n' 'build metadata does not pin cargo 1.98.0' >&2
  exit 65
fi
EXTRACTED_BINARY_SHA256=$(sha256sum "$BUNDLE_ROOT/bin/notm" | awk '{print $1}')
readonly EXTRACTED_BINARY_SHA256
if [ "$(grep -Fxc \
  "Release binary SHA-256: $EXTRACTED_BINARY_SHA256" \
  "$BUNDLE_ROOT/BUILD-INFO.txt")" -ne 1 ]; then
  printf '%s\n' 'build metadata does not match the extracted binary hash' >&2
  exit 65
fi
if [ "$(grep -Fxc \
  'Binary reproducibility: independent clean workflow comparison required' \
  "$BUNDLE_ROOT/BUILD-INFO.txt")" -ne 1 ]; then
  printf '%s\n' 'build metadata lacks the scoped reproducibility result' >&2
  exit 65
fi

desktop-file-validate \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"
grep -Fx 'Exec=notm launch %u' \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop" >/dev/null
grep -Fx 'TryExec=notm' \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop" >/dev/null
grep -F 'AArch64 GNU/Linux build' "$BUNDLE_ROOT/INSTALL.md" >/dev/null
grep -F 'Ubuntu 24.04 ARM64 userspace' "$BUNDLE_ROOT/INSTALL.md" >/dev/null
grep -F 'libnotmuch5t64' "$BUNDLE_ROOT/INSTALL.md" >/dev/null
grep -F 'To uninstall only the files installed by this bundle:' \
  "$BUNDLE_ROOT/INSTALL.md" >/dev/null

"$SOURCE_ROOT/packaging/arm64/verify-native-binary.sh" \
  "$BUNDLE_ROOT/bin/notm" "$VERSION"

INSTALL_ROOT="$HOME_ROOT/.local"
readonly INSTALL_ROOT
mkdir -p -- "$INSTALL_ROOT"
(
  cd -- "$BUNDLE_ROOT"
  cp -a bin share "$INSTALL_ROOT/"
)

env \
  HOME="$HOME_ROOT" \
  XDG_CONFIG_HOME="$HOME_ROOT/config" \
  XDG_CACHE_HOME="$HOME_ROOT/cache" \
  XDG_DATA_HOME="$HOME_ROOT/data" \
  XDG_RUNTIME_DIR="$XDG_RUNTIME_ROOT" \
  PATH="$INSTALL_ROOT/bin:/usr/bin:/bin" \
  notm --version | grep -Fx "notm $VERSION" >/dev/null
desktop-file-validate \
  "$INSTALL_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$INSTALL_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"

rm -f \
  "$INSTALL_ROOT/bin/notm" \
  "$INSTALL_ROOT/share/applications/io.github.kris004.notm.desktop" \
  "$INSTALL_ROOT/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$INSTALL_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml" \
  "$INSTALL_ROOT/share/man/man1/notm.1" \
  "$INSTALL_ROOT/share/man/man5/notm-config.5" \
  "$INSTALL_ROOT/share/man/man7/notm-test-harness.7" \
  "$INSTALL_ROOT/share/man/man7/notm-automation.7"

if find "$INSTALL_ROOT" \( -type f -o -type l \) -print -quit | grep -q .; then
  printf '%s\n' 'ARM64 bundle uninstall left installed files behind:' >&2
  find "$INSTALL_ROOT" \( -type f -o -type l \) -print >&2
  exit 65
fi
if [ ! -f "$OUTSIDE_SENTINEL" ]; then
  printf '%s\n' 'ARM64 bundle install/uninstall changed an outside sentinel' >&2
  exit 65
fi

printf 'Verified archive: %s\n' "$ARCHIVE_NAME"
printf 'Verified checksum: %s\n' "$CHECKSUM_NAME"
printf '%s\n' 'Verified user install and exact-file uninstall'
