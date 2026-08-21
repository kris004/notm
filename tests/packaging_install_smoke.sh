#!/bin/sh

# Validate release metadata and exercise staged install/uninstall behavior.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-packaging.XXXXXX")
readonly WORK_ROOT
STAGE_ROOT="$WORK_ROOT/stage"
readonly STAGE_ROOT
OUTSIDE_SENTINEL="$WORK_ROOT/outside-sentinel"
readonly OUTSIDE_SENTINEL
INPUT_BINARY="$WORK_ROOT/input-notm"
readonly INPUT_BINARY

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

cd "$PROJECT_ROOT"

desktop-file-validate packaging/io.github.kris004.notm.desktop
# Keep packaging validation deterministic and independent of remote assets.
appstreamcli validate --strict --pedantic --no-net \
  packaging/io.github.kris004.notm.metainfo.xml

printf '%s\n' '#!/bin/sh' 'exit 0' > "$INPUT_BINARY"
chmod 755 "$INPUT_BINARY"
: > "$OUTSIDE_SENTINEL"

prefix_under_test=/usr
data_dir="$STAGE_ROOT$prefix_under_test/share"
legacy_desktop="$data_dir/applications/notm.desktop"
legacy_app_id=dev.notm.Notm
legacy_app_desktop="$data_dir/applications/$legacy_app_id.desktop"
legacy_app_icon="$data_dir/icons/hicolor/scalable/apps/$legacy_app_id.svg"
legacy_app_metainfo="$data_dir/metainfo/$legacy_app_id.metainfo.xml"
mkdir -p -- "$(dirname -- "$legacy_desktop")"
: > "$legacy_desktop"
mkdir -p -- "$(dirname -- "$legacy_app_icon")" "$(dirname -- "$legacy_app_metainfo")"
: > "$legacy_app_desktop"
: > "$legacy_app_icon"
: > "$legacy_app_metainfo"

make --no-print-directory \
  PREFIX="$prefix_under_test" \
  DESTDIR="$STAGE_ROOT" \
  CARGO=true \
  BINARY="$INPUT_BINARY" \
  install

test ! -e "$legacy_desktop"
test ! -e "$legacy_app_desktop"
test ! -e "$legacy_app_icon"
test ! -e "$legacy_app_metainfo"
test -x "$STAGE_ROOT$prefix_under_test/bin/notm"
test -f "$data_dir/applications/io.github.kris004.notm.desktop"
test -f "$data_dir/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
test -f "$data_dir/metainfo/io.github.kris004.notm.metainfo.xml"
test -f "$data_dir/man/man1/notm.1"
test -f "$data_dir/man/man5/notm-config.5"
test -f "$data_dir/man/man7/notm-test-harness.7"
test -f "$data_dir/man/man7/notm-automation.7"
desktop-file-validate "$data_dir/applications/io.github.kris004.notm.desktop"
grep -Fx 'Exec=/usr/bin/notm launch' \
  "$data_dir/applications/io.github.kris004.notm.desktop" > /dev/null
grep -Fx 'TryExec=/usr/bin/notm' \
  "$data_dir/applications/io.github.kris004.notm.desktop" > /dev/null
appstreamcli validate-tree --strict --pedantic --no-net "$STAGE_ROOT"

# Exercise legacy cleanup in uninstall independently from install migration.
: > "$legacy_desktop"
: > "$legacy_app_desktop"
: > "$legacy_app_icon"
: > "$legacy_app_metainfo"
make --no-print-directory \
  PREFIX="$prefix_under_test" \
  DESTDIR="$STAGE_ROOT" \
  uninstall

test ! -e "$STAGE_ROOT$prefix_under_test/bin/notm"
test ! -e "$data_dir/applications/io.github.kris004.notm.desktop"
test ! -e "$legacy_desktop"
test ! -e "$legacy_app_desktop"
test ! -e "$legacy_app_icon"
test ! -e "$legacy_app_metainfo"
test ! -e "$data_dir/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
test ! -e "$data_dir/metainfo/io.github.kris004.notm.metainfo.xml"
test ! -e "$data_dir/man/man1/notm.1"
test ! -e "$data_dir/man/man5/notm-config.5"
test ! -e "$data_dir/man/man7/notm-test-harness.7"
test ! -e "$data_dir/man/man7/notm-automation.7"
test -f "$OUTSIDE_SENTINEL"

if find "$STAGE_ROOT" -type f -print -quit | grep -q .; then
  printf '%s\n' 'uninstall left staged files behind:' >&2
  find "$STAGE_ROOT" -type f -print >&2
  exit 1
fi
