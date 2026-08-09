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

desktop-file-validate packaging/dev.notm.Notm.desktop
# The project remote is currently private, so the structural validation must
# not turn its unauthenticated HTTP 404 response into a metadata failure.
appstreamcli validate --strict --pedantic --no-net \
  packaging/dev.notm.Notm.metainfo.xml

printf '%s\n' '#!/bin/sh' 'exit 0' > "$INPUT_BINARY"
chmod 755 "$INPUT_BINARY"
: > "$OUTSIDE_SENTINEL"

prefix_under_test=/usr
data_dir="$STAGE_ROOT$prefix_under_test/share"
legacy_desktop="$data_dir/applications/notm.desktop"
mkdir -p -- "$(dirname -- "$legacy_desktop")"
: > "$legacy_desktop"

make --no-print-directory \
  PREFIX="$prefix_under_test" \
  DESTDIR="$STAGE_ROOT" \
  CARGO=true \
  BINARY="$INPUT_BINARY" \
  install

test ! -e "$legacy_desktop"
test -x "$STAGE_ROOT$prefix_under_test/bin/notm"
test -f "$data_dir/applications/dev.notm.Notm.desktop"
test -f "$data_dir/icons/hicolor/scalable/apps/dev.notm.Notm.svg"
test -f "$data_dir/metainfo/dev.notm.Notm.metainfo.xml"
test -f "$data_dir/man/man1/notm.1"
test -f "$data_dir/man/man5/notm-config.5"
test -f "$data_dir/man/man7/notm-test-harness.7"
test -f "$data_dir/man/man7/notm-automation.7"
desktop-file-validate "$data_dir/applications/dev.notm.Notm.desktop"
grep -Fx 'Exec=/usr/bin/notm launch' \
  "$data_dir/applications/dev.notm.Notm.desktop" > /dev/null
grep -Fx 'TryExec=/usr/bin/notm' \
  "$data_dir/applications/dev.notm.Notm.desktop" > /dev/null
appstreamcli validate-tree --strict --pedantic --no-net "$STAGE_ROOT"

# Exercise legacy cleanup in uninstall independently from install migration.
: > "$legacy_desktop"
make --no-print-directory \
  PREFIX="$prefix_under_test" \
  DESTDIR="$STAGE_ROOT" \
  uninstall

test ! -e "$STAGE_ROOT$prefix_under_test/bin/notm"
test ! -e "$data_dir/applications/dev.notm.Notm.desktop"
test ! -e "$legacy_desktop"
test ! -e "$data_dir/icons/hicolor/scalable/apps/dev.notm.Notm.svg"
test ! -e "$data_dir/metainfo/dev.notm.Notm.metainfo.xml"
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
