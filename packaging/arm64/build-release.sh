#!/bin/sh

# Assemble the deterministic aarch64 GNU/Linux binary bundle.

set -eu

TARGET=aarch64-unknown-linux-gnu
CHECKSUM_NAME=SHA256SUMS-aarch64
readonly TARGET CHECKSUM_NAME

usage() {
  printf '%s\n' \
    "usage: $0 SOURCE_ROOT VERSION SOURCE_COMMIT BINARY BUILD_INFO OUTPUT_DIR" >&2
}

if [ "$#" -ne 6 ]; then
  usage
  exit 64
fi

SOURCE_ROOT=$1
VERSION=$2
SOURCE_COMMIT=$3
BINARY=$4
BUILD_INFO=$5
OUTPUT_DIR=$6

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
if [ ! -x "$BINARY" ]; then
  printf 'release binary is not executable: %s\n' "$BINARY" >&2
  exit 66
fi
if [ ! -f "$BUILD_INFO" ]; then
  printf 'build information is not a file: %s\n' "$BUILD_INFO" >&2
  exit 66
fi

SOURCE_ROOT=$(
  CDPATH=''
  cd -- "$SOURCE_ROOT"
  pwd -P
)
BINARY=$(
  CDPATH=''
  cd -- "$(dirname -- "$BINARY")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename -- "$BINARY")"
)
BUILD_INFO=$(
  CDPATH=''
  cd -- "$(dirname -- "$BUILD_INFO")"
  printf '%s/%s\n' "$(pwd -P)" "$(basename -- "$BUILD_INFO")"
)

if ! SOURCE_COMMIT=$(
  git -C "$SOURCE_ROOT" rev-parse --verify "${SOURCE_COMMIT}^{commit}"
); then
  printf 'source commit does not resolve to a commit\n' >&2
  exit 65
fi
readonly SOURCE_ROOT VERSION SOURCE_COMMIT BINARY BUILD_INFO

if [ "$(git -C "$SOURCE_ROOT" rev-parse HEAD)" != "$SOURCE_COMMIT" ]; then
  printf 'source commit is not the checked-out HEAD: %s\n' "$SOURCE_COMMIT" >&2
  exit 65
fi
METADATA_VERSION=$(
  "$SOURCE_ROOT/packaging/verify-release-metadata.py" \
    --print-version "$SOURCE_ROOT"
)
readonly METADATA_VERSION
if [ "$METADATA_VERSION" != "$VERSION" ]; then
  printf 'release metadata version mismatch: expected %s, got %s\n' \
    "$VERSION" "$METADATA_VERSION" >&2
  exit 65
fi

PACKAGE_INPUTS='packaging/io.github.kris004.notm.desktop
packaging/io.github.kris004.notm.metainfo.xml
packaging/icons/hicolor/scalable/apps/io.github.kris004.notm.svg
docs/man/notm.1
docs/man/notm-config.5
docs/man/notm-test-harness.7
docs/man/notm-automation.7
LICENSE
CHANGELOG.md
Cargo.lock'
readonly PACKAGE_INPUTS

# Do not label working-tree metadata as belonging to a different source commit.
# shellcheck disable=SC2086 # The constant is an intentional newline-delimited list.
if ! git -C "$SOURCE_ROOT" diff --quiet "$SOURCE_COMMIT" -- $PACKAGE_INPUTS; then
  printf 'release package inputs differ from source commit %s\n' \
    "$SOURCE_COMMIT" >&2
  exit 65
fi

SOURCE_DATE_EPOCH=$(git -C "$SOURCE_ROOT" show -s --format=%ct "$SOURCE_COMMIT")
case "$SOURCE_DATE_EPOCH" in
  '' | *[!0-9]*)
    printf 'source commit has an invalid timestamp\n' >&2
    exit 65
    ;;
esac
readonly SOURCE_DATE_EPOCH

LOCK_SHA256=$(sha256sum "$SOURCE_ROOT/Cargo.lock" | awk '{print $1}')
readonly LOCK_SHA256

require_build_info_line() {
  line=$1
  if [ "$(grep -Fxc -- "$line" "$BUILD_INFO")" -ne 1 ]; then
    printf 'build information must contain exactly once: %s\n' "$line" >&2
    exit 65
  fi
}

require_build_info_line "Version: $VERSION"
require_build_info_line "Source tag: v$VERSION"
require_build_info_line "Source commit: $SOURCE_COMMIT"
require_build_info_line "Source date epoch: $SOURCE_DATE_EPOCH"
require_build_info_line "Target: $TARGET"
require_build_info_line 'Build architecture: aarch64'
require_build_info_line 'Build environment: Ubuntu 24.04 ARM64 userspace'
require_build_info_line "Cargo.lock SHA-256: $LOCK_SHA256"

if [ "$(grep -Ec '^Rust compiler: rustc 1\.98\.0( |$)' "$BUILD_INFO")" -ne 1 ]; then
  printf '%s\n' 'build information must identify rustc 1.98.0 exactly' >&2
  exit 65
fi
if [ "$(grep -Ec '^Cargo: cargo 1\.98\.0( |$)' "$BUILD_INFO")" -ne 1 ]; then
  printf '%s\n' 'build information must identify cargo 1.98.0 exactly' >&2
  exit 65
fi
if [ "$(grep -Ec '^Release binary SHA-256: [0-9a-f]{64}$' \
  "$BUILD_INFO")" -ne 1 ]; then
  printf '%s\n' 'build information must contain the release binary SHA-256' >&2
  exit 65
fi
require_build_info_line \
  'Binary reproducibility: independent clean workflow comparison required'
EXPECTED_BINARY_SHA256=$(sha256sum "$BINARY" | awk '{print $1}')
readonly EXPECTED_BINARY_SHA256
require_build_info_line "Release binary SHA-256: $EXPECTED_BINARY_SHA256"

if [ -L "$OUTPUT_DIR" ]; then
  printf 'release output directory must not be a symlink: %s\n' \
    "$OUTPUT_DIR" >&2
  exit 73
fi
mkdir -p -- "$OUTPUT_DIR"
OUTPUT_DIR=$(
  CDPATH=''
  cd -- "$OUTPUT_DIR"
  pwd -P
)
readonly OUTPUT_DIR

BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
ARCHIVE="$OUTPUT_DIR/$BUNDLE_NAME.tar.gz"
CHECKSUM="$OUTPUT_DIR/$CHECKSUM_NAME"
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-release.XXXXXX")
BUNDLE_ROOT="$WORK_ROOT/$BUNDLE_NAME"
BUNDLE_TAR="$WORK_ROOT/$BUNDLE_NAME.tar"
readonly \
  BUNDLE_NAME ARCHIVE CHECKSUM WORK_ROOT BUNDLE_ROOT BUNDLE_TAR

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

for artifact in "$ARCHIVE" "$CHECKSUM"; do
  if [ -e "$artifact" ] || [ -L "$artifact" ]; then
    printf 'refusing to overwrite release artifact: %s\n' "$artifact" >&2
    exit 73
  fi
done

umask 022
install -Dm755 "$BINARY" "$BUNDLE_ROOT/bin/notm"

mkdir -p -- "$BUNDLE_ROOT/share/applications"
sed \
  -e 's|^Exec=.*|Exec=notm launch %u|' \
  -e 's|^TryExec=.*|TryExec=notm|' \
  "$SOURCE_ROOT/packaging/io.github.kris004.notm.desktop" \
  >"$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"

install -Dm644 \
  "$SOURCE_ROOT/packaging/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$BUNDLE_ROOT/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
install -Dm644 \
  "$SOURCE_ROOT/packaging/io.github.kris004.notm.metainfo.xml" \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"
install -Dm644 "$SOURCE_ROOT/docs/man/notm.1" \
  "$BUNDLE_ROOT/share/man/man1/notm.1"
install -Dm644 "$SOURCE_ROOT/docs/man/notm-config.5" \
  "$BUNDLE_ROOT/share/man/man5/notm-config.5"
install -Dm644 "$SOURCE_ROOT/docs/man/notm-test-harness.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-test-harness.7"
install -Dm644 "$SOURCE_ROOT/docs/man/notm-automation.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-automation.7"

install -Dm644 "$SOURCE_ROOT/LICENSE" "$BUNDLE_ROOT/LICENSE"
install -Dm644 "$SOURCE_ROOT/CHANGELOG.md" "$BUNDLE_ROOT/CHANGELOG.md"
install -Dm644 "$BUILD_INFO" "$BUNDLE_ROOT/BUILD-INFO.txt"

cat >"$BUNDLE_ROOT/INSTALL.md" <<'EOF'
# Installing this ARM64 binary bundle

This archive contains a dynamically linked AArch64 GNU/Linux build produced
natively in an Ubuntu 24.04 ARM64 userspace. It is not a fully static or
distribution-independent package. `BUILD-INFO.txt` records the exact source,
toolchain, package baseline, runner label, and mutable runner-image revision
reported for this build.

Install it for the current user from the extracted bundle root:

```sh
mkdir -p "$HOME/.local"
cp -a bin share "$HOME/.local/"
```

Ensure `$HOME/.local/bin` is in `PATH`, then run `notm --version` and
`notm launch`.

The host must provide compatible ARM64 GTK 4, GtkSourceView 5, WebKitGTK 6.0,
and Notmuch runtime libraries. On Ubuntu 24.04 ARM64 they can be installed with:

```sh
sudo apt install \
  libgtk-4-1 libgtksourceview-5-0 libnotmuch5t64 libwebkitgtk-6.0-4
```

For other distributions, install equivalent AArch64 runtime packages or build
notm from source. See <https://github.com/kris004/notm> for configuration and
source-build details.

To uninstall only the files installed by this bundle:

```sh
rm -f \
  "$HOME/.local/bin/notm" \
  "$HOME/.local/share/applications/io.github.kris004.notm.desktop" \
  "$HOME/.local/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$HOME/.local/share/metainfo/io.github.kris004.notm.metainfo.xml" \
  "$HOME/.local/share/man/man1/notm.1" \
  "$HOME/.local/share/man/man5/notm-config.5" \
  "$HOME/.local/share/man/man7/notm-test-harness.7" \
  "$HOME/.local/share/man/man7/notm-automation.7"
```
EOF

desktop-file-validate \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"

find "$BUNDLE_ROOT" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
(
  cd -- "$WORK_ROOT"
  LC_ALL=C TZ=UTC tar \
    --sort=name \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --mtime="@$SOURCE_DATE_EPOCH" \
    -cf "$BUNDLE_TAR" \
    "$BUNDLE_NAME"
)
gzip -n -9 -c "$BUNDLE_TAR" >"$ARCHIVE"

(
  cd -- "$OUTPUT_DIR"
  sha256sum "$(basename -- "$ARCHIVE")" >"$CHECKSUM_NAME"
)

printf '%s\n' "$ARCHIVE" "$CHECKSUM"
