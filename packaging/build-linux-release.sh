#!/bin/sh

# Assemble a relocatable GNU/Linux release bundle from an already-built binary.

set -eu

usage() {
  printf '%s\n' \
    "usage: $0 SOURCE_ROOT VERSION TARGET BINARY BUILD_INFO OUTPUT_DIR" >&2
}

if [ "$#" -ne 6 ]; then
  usage
  exit 2
fi

SOURCE_ROOT=$1
VERSION=$2
TARGET=$3
BINARY=$4
BUILD_INFO=$5
OUTPUT_DIR=$6

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

if [ ! -d "$SOURCE_ROOT" ]; then
  printf 'source root is not a directory: %s\n' "$SOURCE_ROOT" >&2
  exit 2
fi
if [ ! -x "$BINARY" ]; then
  printf 'release binary is not executable: %s\n' "$BINARY" >&2
  exit 2
fi
if [ ! -f "$BUILD_INFO" ]; then
  printf 'build information is not a file: %s\n' "$BUILD_INFO" >&2
  exit 2
fi

SOURCE_ROOT=$(
  CDPATH=''
  cd -- "$SOURCE_ROOT"
  pwd
)
BINARY=$(
  CDPATH=''
  cd -- "$(dirname -- "$BINARY")"
  printf '%s/%s\n' "$(pwd)" "$(basename -- "$BINARY")"
)
BUILD_INFO=$(
  CDPATH=''
  cd -- "$(dirname -- "$BUILD_INFO")"
  printf '%s/%s\n' "$(pwd)" "$(basename -- "$BUILD_INFO")"
)
mkdir -p -- "$OUTPUT_DIR"
OUTPUT_DIR=$(
  CDPATH=''
  cd -- "$OUTPUT_DIR"
  pwd
)

BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
ARCHIVE="$OUTPUT_DIR/$BUNDLE_NAME.tar.gz"
CHECKSUM="$OUTPUT_DIR/SHA256SUMS"
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release.XXXXXX")
readonly SOURCE_ROOT BINARY BUILD_INFO OUTPUT_DIR BUNDLE_NAME ARCHIVE CHECKSUM WORK_ROOT
BUNDLE_ROOT="$WORK_ROOT/$BUNDLE_NAME"
readonly BUNDLE_ROOT

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

install -Dm755 "$BINARY" "$BUNDLE_ROOT/bin/notm"

mkdir -p -- "$BUNDLE_ROOT/share/applications"
sed \
  -e 's|^Exec=.*|Exec=notm launch|' \
  -e 's|^TryExec=.*|TryExec=notm|' \
  "$SOURCE_ROOT/packaging/io.github.kris004.notm.desktop" \
  > "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"

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

cat > "$BUNDLE_ROOT/INSTALL.md" <<'EOF'
# Installing this binary bundle

This archive contains a dynamically linked x86_64 GNU/Linux build produced on
Ubuntu 24.04. It is not a fully static or distribution-independent package.

Install it for the current user with:

```sh
mkdir -p "$HOME/.local"
cp -a bin share "$HOME/.local/"
```

Ensure `$HOME/.local/bin` is in `PATH`, then run `notm --version` and
`notm launch`.

The host must provide compatible GTK 4, GtkSourceView 5, WebKitGTK 6.0, and
Notmuch runtime libraries. On Ubuntu 24.04 they can be installed with:

```sh
sudo apt install \
  libgtk-4-1 libgtksourceview-5-0 libnotmuch5 libwebkitgtk-6.0-4
```

For other distributions, install the equivalent runtime packages or build
notm from source. See <https://github.com/kris004/notm> for configuration and
source-build details.
EOF

desktop-file-validate \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"

if [ -n "${SOURCE_DATE_EPOCH:-}" ]; then
  case "$SOURCE_DATE_EPOCH" in
    *[!0-9]*)
      printf 'SOURCE_DATE_EPOCH must be a non-negative integer\n' >&2
      exit 2
      ;;
  esac
  find "$BUNDLE_ROOT" -exec touch -h -d "@$SOURCE_DATE_EPOCH" {} +
  archive_mtime="@$SOURCE_DATE_EPOCH"
else
  archive_mtime='@0'
fi

rm -f -- "$ARCHIVE" "$CHECKSUM"
(
  cd -- "$WORK_ROOT"
  LC_ALL=C tar \
    --sort=name \
    --owner=0 \
    --group=0 \
    --numeric-owner \
    --mtime="$archive_mtime" \
    -cf - \
    "$BUNDLE_NAME"
) | gzip -n -9 > "$ARCHIVE"

(
  cd -- "$OUTPUT_DIR"
  sha256sum "$(basename -- "$ARCHIVE")" > "$(basename -- "$CHECKSUM")"
)

printf '%s\n' "$ARCHIVE" "$CHECKSUM"
