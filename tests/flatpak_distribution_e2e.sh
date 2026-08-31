#!/usr/bin/env bash

# Build and exercise the production Flatpak without using live notm data.

set -euo pipefail

readonly APP_ID=io.github.kris004.notm
SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
readonly SCRIPT_DIR
SOURCE_ROOT=$(cd -- "$SCRIPT_DIR/.." && pwd -P)
readonly SOURCE_ROOT
readonly MANIFEST="$SOURCE_ROOT/$APP_ID.yml"
readonly BUILDER_FLATPAK_WRAPPER="$SOURCE_ROOT/packaging/flatpak/run-builder-flatpak-e2e.sh"
readonly KEEP_WORK=${NOTM_FLATPAK_KEEP_WORK:-0}
readonly USE_INSTALLED_RUNTIMES=${NOTM_FLATPAK_USE_INSTALLED_RUNTIMES:-0}
readonly DISABLE_SESSION_P11=${NOTM_FLATPAK_DISABLE_SESSION_P11_KIT:-0}
readonly HOME_MARKER='~'
readonly TEST_HARNESS_APPLICATION_ID="$APP_ID.test.flatpak"
readonly APP_SOURCE_COMMIT=a4f90fcc642ef566af256d20390917a56a455373
readonly SOURCE_SCREENSHOT_SHA256=8099c265a0aa7dd3d779c97786200056847bc0bb79f3a5d1150e8903e87c443d
readonly MIRRORED_SCREENSHOT_SHA256=5e7eb334b633ade7582f45b43fcc67a8791178e4235494833ec41b328882a4ec

entry_metadata_fingerprint() {
  local path=$1

  if [[ ! -e $path && ! -L $path ]]; then
    printf '%s\n' absent
    return
  fi

  # GNU stat inspects only the named directory entry. It does not enumerate or
  # hash any user files below it.
  stat --printf='present:device=%d:inode=%i:mode=%f:uid=%u:gid=%g:size=%s:atime=%X:mtime=%Y:ctime=%Z\n' \
    -- "$path"
}

parse_command() {
  python3 - "$1" <<'PY'
import shlex
import sys

for argument in shlex.split(sys.argv[1]):
    sys.stdout.buffer.write(argument.encode() + b"\0")
PY
}

clear_builder_sandbox_marker() {
  local marker="$PRIVATE_RUNTIME_DIR/flatpak-info"

  if [[ ! -e $marker && ! -L $marker ]]; then
    return
  fi
  if [[ ! -f $marker || -L $marker || \
    $(stat -c '%u' -- "$marker") != "$(id -u)" ]]; then
    printf 'error: refusing to remove unsafe Builder sandbox marker: %s\n' \
      "$marker" >&2
    exit 73
  fi
  rm -f -- "$marker"
  if [[ -e $marker || -L $marker ]]; then
    printf 'error: Builder sandbox marker remained after cleanup: %s\n' \
      "$marker" >&2
    exit 1
  fi
}

mapfile -d '' -t BUILDER_COMMAND < <(
  parse_command "${NOTM_FLATPAK_BUILDER_COMMAND:-flatpak-builder}"
)
if [[ ${#BUILDER_COMMAND[@]} -eq 0 ]]; then
  printf '%s\n' 'error: NOTM_FLATPAK_BUILDER_COMMAND is empty' >&2
  exit 64
fi
if [[ $USE_INSTALLED_RUNTIMES != 0 && $USE_INSTALLED_RUNTIMES != 1 ]]; then
  printf 'error: NOTM_FLATPAK_USE_INSTALLED_RUNTIMES must be 0 or 1\n' >&2
  exit 64
fi
if [[ $DISABLE_SESSION_P11 != 0 && $DISABLE_SESSION_P11 != 1 ]]; then
  printf 'error: NOTM_FLATPAK_DISABLE_SESSION_P11_KIT must be 0 or 1\n' >&2
  exit 64
fi

LINTER_COMMAND=()
if [[ -n ${NOTM_FLATPAK_LINTER_COMMAND:-} ]]; then
  mapfile -d '' -t LINTER_COMMAND < <(
    parse_command "$NOTM_FLATPAK_LINTER_COMMAND"
  )
  if [[ ${#LINTER_COMMAND[@]} -ne 2 || \
    ${LINTER_COMMAND[1]:-} != --linter ]]; then
    printf '%s\n' \
      'error: the linter override must use the committed official Builder wrapper' >&2
    exit 73
  fi
  LINTER_EXECUTABLE=${LINTER_COMMAND[0]}
  if [[ $LINTER_EXECUTABLE != */* ]]; then
    LINTER_EXECUTABLE=$(command -v -- "$LINTER_EXECUTABLE" || true)
  elif [[ $LINTER_EXECUTABLE != /* ]]; then
    LINTER_EXECUTABLE="$PWD/$LINTER_EXECUTABLE"
  fi
  LINTER_EXECUTABLE=$(readlink -f -- "$LINTER_EXECUTABLE" 2>/dev/null || true)
  if [[ $LINTER_EXECUTABLE != "$BUILDER_FLATPAK_WRAPPER" ]]; then
    printf '%s\n' \
      'error: the linter override must use the committed official Builder wrapper' >&2
    exit 73
  fi
  LINTER_COMMAND=("$BUILDER_FLATPAK_WRAPPER" --linter)
elif command -v flatpak-builder-lint >/dev/null 2>&1; then
  LINTER_COMMAND=(flatpak-builder-lint)
else
  printf '%s\n' 'error: the required official flatpak-builder-lint is unavailable' >&2
  exit 69
fi

if [[ ${NOTM_GUI_TEST_DISPLAY:-} != provided || ${NOTM_REQUIRE_GTK_DISPLAY:-} != 1 ]]; then
  cat >&2 <<EOF
error: the Flatpak distribution E2E requires a private offscreen display
run: dbus-run-session -- "$SOURCE_ROOT/tests/run_with_headless_weston.sh" "$0"
EOF
  exit 69
fi

for command in \
  appstreamcli \
  dbus-update-activation-environment \
  desktop-file-validate \
  file \
  flatpak \
  git \
  python3 \
  readelf \
  readlink \
  realpath \
  sha256sum \
  stat \
  update-desktop-database \
  xdg-mime; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'error: required Flatpak E2E command is unavailable: %s\n' "$command" >&2
    exit 69
  fi
done

if [[ -z ${HOME:-} || ! -d $HOME ]]; then
  printf 'error: caller HOME is unset or not a directory: %q\n' "${HOME:-}" >&2
  exit 73
fi
ORIGINAL_HOME=$(cd -- "$HOME" && pwd -P)
readonly ORIGINAL_HOME
readonly ORIGINAL_XDG_DATA_HOME=${XDG_DATA_HOME:-"$ORIGINAL_HOME/.local/share"}
ORIGINAL_HOME_FINGERPRINT=$(entry_metadata_fingerprint "$ORIGINAL_HOME")
readonly ORIGINAL_HOME_FINGERPRINT
readonly ORIGINAL_VAR_APP="$ORIGINAL_HOME/.var/app"
ORIGINAL_VAR_APP_FINGERPRINT=$(entry_metadata_fingerprint "$ORIGINAL_VAR_APP")
readonly ORIGINAL_VAR_APP_FINGERPRINT
readonly ORIGINAL_APP_DATA="$ORIGINAL_HOME/.var/app/$APP_ID"
if [[ -e $ORIGINAL_APP_DATA || -L $ORIGINAL_APP_DATA ]]; then
  printf 'error: refusing to inspect or touch existing Flatpak data: %s\n' \
    "$ORIGINAL_APP_DATA" >&2
  exit 73
fi
readonly ORIGINAL_APP_DATA_FINGERPRINT=absent

TEST_DISPOSABLE_ROOT=
if [[ -n ${NOTM_TEST_DISPOSABLE_ROOT:-} ]]; then
  if [[ $NOTM_TEST_DISPOSABLE_ROOT != /* || \
    ! -d $NOTM_TEST_DISPOSABLE_ROOT || \
    -L $NOTM_TEST_DISPOSABLE_ROOT ]]; then
    printf 'error: NOTM_TEST_DISPOSABLE_ROOT is not a real absolute directory: %s\n' \
      "${NOTM_TEST_DISPOSABLE_ROOT:-}" >&2
    exit 73
  fi
  TEST_DISPOSABLE_ROOT=$(cd -- "$NOTM_TEST_DISPOSABLE_ROOT" && pwd -P)
fi

if [[ -n ${NOTM_FLATPAK_WORK_ROOT:-} ]]; then
  WORK_ROOT=$NOTM_FLATPAK_WORK_ROOT
  if [[ $WORK_ROOT != /* || -L $WORK_ROOT ]]; then
    printf 'error: NOTM_FLATPAK_WORK_ROOT is not a real absolute path: %s\n' \
      "$WORK_ROOT" >&2
    exit 73
  fi
  WORK_ROOT=$(realpath -m -- "$WORK_ROOT")
  if [[ -n $TEST_DISPOSABLE_ROOT && \
    $WORK_ROOT != "$TEST_DISPOSABLE_ROOT"/* && \
    $WORK_ROOT != "$TEST_DISPOSABLE_ROOT" ]]; then
    printf 'error: Flatpak work root escaped the private test root: %s\n' \
      "$WORK_ROOT" >&2
    exit 73
  fi
  if [[ -e $WORK_ROOT || -L $WORK_ROOT ]]; then
    if [[ ! -d $WORK_ROOT ]]; then
      printf 'error: NOTM_FLATPAK_WORK_ROOT is not a directory: %s\n' "$WORK_ROOT" >&2
      exit 73
    fi
    if find "$WORK_ROOT" -mindepth 1 -maxdepth 1 -print -quit | grep -q .; then
      printf 'error: NOTM_FLATPAK_WORK_ROOT is not empty: %s\n' "$WORK_ROOT" >&2
      exit 73
    fi
  else
    (umask 077 && mkdir -p -- "$WORK_ROOT")
  fi
  WORK_ROOT=$(cd -- "$WORK_ROOT" && pwd -P)
elif [[ -n $TEST_DISPOSABLE_ROOT ]]; then
  WORK_ROOT="$TEST_DISPOSABLE_ROOT/flatpak-e2e"
  if [[ -e $WORK_ROOT || -L $WORK_ROOT ]]; then
    printf 'error: private Flatpak work directory already exists: %s\n' \
      "$WORK_ROOT" >&2
    exit 73
  fi
  mkdir -m 700 -- "$WORK_ROOT"
  WORK_ROOT=$(cd -- "$WORK_ROOT" && pwd -P)
else
  WORK_ROOT=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/notm-flatpak-e2e.XXXXXX")
fi
readonly WORK_ROOT
if [[ -z $TEST_DISPOSABLE_ROOT ]]; then
  TEST_DISPOSABLE_ROOT=$WORK_ROOT
fi
readonly TEST_DISPOSABLE_ROOT
if [[ $(stat -c '%u:%a' -- "$TEST_DISPOSABLE_ROOT") != "$(id -u):700" ]]; then
  printf 'error: disposable test root is not private: %s\n' \
    "$TEST_DISPOSABLE_ROOT" >&2
  exit 73
fi
if [[ $WORK_ROOT != "$TEST_DISPOSABLE_ROOT"/* && \
  $WORK_ROOT != "$TEST_DISPOSABLE_ROOT" ]]; then
  printf 'error: Flatpak work root escaped the private test root: %s\n' \
    "$WORK_ROOT" >&2
  exit 73
fi
if [[ -z ${XDG_RUNTIME_DIR:-} || \
  ! -d $XDG_RUNTIME_DIR || \
  -L $XDG_RUNTIME_DIR ]]; then
  printf 'error: XDG_RUNTIME_DIR is not a real private directory: %s\n' \
    "${XDG_RUNTIME_DIR:-}" >&2
  exit 73
fi
PRIVATE_RUNTIME_DIR=$(cd -- "$XDG_RUNTIME_DIR" && pwd -P)
readonly PRIVATE_RUNTIME_DIR
if [[ $PRIVATE_RUNTIME_DIR != "$TEST_DISPOSABLE_ROOT"/* || \
  $(stat -c '%u:%a' -- "$PRIVATE_RUNTIME_DIR") != "$(id -u):700" ]]; then
  printf 'error: XDG_RUNTIME_DIR is not a private test directory: %s\n' \
    "$PRIVATE_RUNTIME_DIR" >&2
  exit 73
fi
mkdir -m 700 -- "$WORK_ROOT/home"
DISPOSABLE_HOME=$(cd -- "$WORK_ROOT/home" && pwd -P)
readonly DISPOSABLE_HOME
if [[ $DISPOSABLE_HOME != "$WORK_ROOT"/* || $DISPOSABLE_HOME == "$ORIGINAL_HOME" ]]; then
  printf 'error: disposable HOME escaped its work root or reused caller HOME: %s\n' \
    "$DISPOSABLE_HOME" >&2
  exit 73
fi
mkdir -m 700 -- "$WORK_ROOT/tmp"
DISPOSABLE_TMPDIR=$(cd -- "$WORK_ROOT/tmp" && pwd -P)
readonly DISPOSABLE_TMPDIR
if [[ $DISPOSABLE_TMPDIR != "$WORK_ROOT"/* ]]; then
  printf 'error: disposable TMPDIR escaped its work root: %s\n' \
    "$DISPOSABLE_TMPDIR" >&2
  exit 73
fi
readonly BUILD_DIR="$WORK_ROOT/build"
readonly STATE_DIR="$WORK_ROOT/state"
readonly REPO_DIR="$WORK_ROOT/repo"
FLATPAK_HOME=${NOTM_FLATPAK_USER_DIR:-"$WORK_ROOT/flatpak-user"}
if [[ $FLATPAK_HOME != /* ]]; then
  printf 'error: Flatpak user installation must be absolute: %s\n' \
    "$FLATPAK_HOME" >&2
  exit 73
fi
FLATPAK_HOME=$(realpath -m -- "$FLATPAK_HOME")
if [[ $FLATPAK_HOME != "$TEST_DISPOSABLE_ROOT"/* ]]; then
  printf 'error: Flatpak user installation escaped the private test root: %s\n' \
    "$FLATPAK_HOME" >&2
  exit 73
fi
mkdir -p -- "$FLATPAK_HOME"
FLATPAK_HOME=$(cd -- "$FLATPAK_HOME" && pwd -P)
readonly FLATPAK_HOME
if [[ $FLATPAK_HOME == "$ORIGINAL_HOME/.local/share/flatpak" || \
  $FLATPAK_HOME == "$ORIGINAL_XDG_DATA_HOME/flatpak" ]]; then
  printf 'error: refusing to use the caller live Flatpak installation: %s\n' \
    "$FLATPAK_HOME" >&2
  exit 73
fi
readonly EVIDENCE_LOG="$WORK_ROOT/evidence.log"
readonly LOCAL_REMOTE=notm-flatpak-e2e
readonly HOST_APP_DATA="$DISPOSABLE_HOME/.var/app/$APP_ID"
SECRET_SENTINEL=

BUILDER_FORBIDDEN_HOME_ENTRY=
for candidate in \
  "$ORIGINAL_VAR_APP" \
  "$ORIGINAL_HOME/.ssh" \
  "$ORIGINAL_HOME/.gnupg" \
  "$ORIGINAL_HOME/.config" \
  "$ORIGINAL_HOME/Documents" \
  "$ORIGINAL_HOME/Downloads" \
  "$ORIGINAL_HOME/.bashrc" \
  "$ORIGINAL_HOME/.profile"; do
  if [[ ! -e $candidate || -L $candidate ]]; then
    continue
  fi
  candidate=$(readlink -f -- "$candidate")
  if [[ $candidate == "$SOURCE_ROOT" || \
    $candidate == "$SOURCE_ROOT"/* || \
    $SOURCE_ROOT == "$candidate"/* || \
    $candidate == "$TEST_DISPOSABLE_ROOT" || \
    $candidate == "$TEST_DISPOSABLE_ROOT"/* || \
    $TEST_DISPOSABLE_ROOT == "$candidate"/* ]]; then
    continue
  fi
  BUILDER_FORBIDDEN_HOME_ENTRY=$candidate
  break
done
if [[ -z $BUILDER_FORBIDDEN_HOME_ENTRY ]]; then
  printf '%s\n' \
    'error: no existing live-HOME entry is available for the Builder denial check' >&2
  exit 73
fi
readonly BUILDER_FORBIDDEN_HOME_ENTRY

export HOME="$DISPOSABLE_HOME"
export XDG_CONFIG_HOME="$HOME/.config"
export XDG_CACHE_HOME="$HOME/.cache"
export XDG_DATA_HOME="$HOME/.local/share"
export XDG_STATE_HOME="$HOME/.local/state"
export XDG_DATA_DIRS="$FLATPAK_HOME/exports/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
export FLATPAK_USER_DIR="$FLATPAK_HOME"
export TMPDIR="$DISPOSABLE_TMPDIR"
export NOTM_FLATPAK_DISPOSABLE_ROOT="$TEST_DISPOSABLE_ROOT"
export NOTM_FLATPAK_SOURCE_ROOT="$SOURCE_ROOT"
export NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY="$BUILDER_FORBIDDEN_HOME_ENTRY"
export NOTM_TEST_HARNESS_APPLICATION_ID="$TEST_HARNESS_APPLICATION_ID"

if [[ $DISABLE_SESSION_P11 == 1 ]]; then
  HOST_P11_KIT=$(command -v p11-kit)
  readonly HOST_P11_KIT
  if [[ $HOST_P11_KIT != /* || ! -x $HOST_P11_KIT ]]; then
    printf 'error: host p11-kit executable is unavailable: %s\n' \
      "$HOST_P11_KIT" >&2
    exit 69
  fi
  mkdir -m 700 -- "$WORK_ROOT/session-helper-bin"
  cat >"$WORK_ROOT/session-helper-bin/p11-kit" <<EOF
#!/bin/sh
set -eu
if [ "\${1:-}" = server ]; then
  # Some task supervisors kill p11-kit's detached server immediately. Returning
  # no PID makes the private Flatpak session helper disable this optional proxy.
  exit 0
fi
exec "$HOST_P11_KIT" "\$@"
EOF
  chmod 0755 "$WORK_ROOT/session-helper-bin/p11-kit"
  export PATH="$WORK_ROOT/session-helper-bin:$PATH"
fi

mkdir -p \
  "$XDG_CONFIG_HOME" \
  "$XDG_CACHE_HOME" \
  "$XDG_DATA_HOME" \
  "$XDG_STATE_HOME"

for command_argument in "${BUILDER_COMMAND[@]}" "${LINTER_COMMAND[@]}"; do
  case $command_argument in
    HOME=* | TMPDIR=* | XDG_CONFIG_HOME=* | XDG_CACHE_HOME=* | XDG_DATA_HOME=* | XDG_STATE_HOME=* | XDG_DATA_DIRS=* | NOTM_FLATPAK_DISPOSABLE_ROOT=* | NOTM_FLATPAK_SOURCE_ROOT=* | NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY=*)
      printf 'error: Flatpak command overrides disposable environment: %s\n' \
        "$command_argument" >&2
      exit 73
      ;;
  esac
done

cleanup() {
  local status=$?
  local original_home_fingerprint_after
  local original_var_app_fingerprint_after
  local original_fingerprint_after
  trap - EXIT HUP INT TERM

  if flatpak info --user "$APP_ID" >/dev/null 2>&1; then
    flatpak uninstall --user --noninteractive --assumeyes --delete-data "$APP_ID" \
      >/dev/null 2>&1 || true
  fi
  if flatpak remote-info --user "$LOCAL_REMOTE" "$APP_ID" >/dev/null 2>&1; then
    flatpak remote-delete --user --force "$LOCAL_REMOTE" >/dev/null 2>&1 || true
  fi
  if [[ -n $SECRET_SENTINEL ]]; then
    rm -f -- "$SECRET_SENTINEL"
  fi

  if [[ -e $ORIGINAL_APP_DATA || -L $ORIGINAL_APP_DATA ]]; then
    original_fingerprint_after=present
  else
    original_fingerprint_after=absent
  fi
  if [[ $original_fingerprint_after != "$ORIGINAL_APP_DATA_FINGERPRINT" ]]; then
    printf 'error: original Flatpak data changed outside disposable HOME: %s\n' \
      "$ORIGINAL_APP_DATA" >&2
    status=1
  fi
  original_home_fingerprint_after=$(entry_metadata_fingerprint "$ORIGINAL_HOME")
  if [[ $original_home_fingerprint_after != "$ORIGINAL_HOME_FINGERPRINT" ]]; then
    printf 'error: caller HOME directory metadata changed during Flatpak E2E: %s\n' \
      "$ORIGINAL_HOME" >&2
    status=1
  fi
  original_var_app_fingerprint_after=$(entry_metadata_fingerprint "$ORIGINAL_VAR_APP")
  if [[ $original_var_app_fingerprint_after != "$ORIGINAL_VAR_APP_FINGERPRINT" ]]; then
    printf 'error: caller .var/app directory metadata changed during Flatpak E2E: %s\n' \
      "$ORIGINAL_VAR_APP" >&2
    status=1
  fi

  if [[ $status -ne 0 || $KEEP_WORK == 1 ]]; then
    printf 'Flatpak E2E evidence retained at %s\n' "$WORK_ROOT" >&2
  else
    rm -rf -- "$WORK_ROOT"
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

mkdir -p "$STATE_DIR" "$REPO_DIR"
exec > >(tee "$EVIDENCE_LOG") 2>&1

printf 'Original HOME fingerprint: %s (%s)\n' \
  "$ORIGINAL_APP_DATA_FINGERPRINT" "$ORIGINAL_APP_DATA"
printf 'Caller HOME metadata fingerprint: %s\n' "$ORIGINAL_HOME_FINGERPRINT"
printf 'Caller .var/app metadata fingerprint: %s (%s)\n' \
  "$ORIGINAL_VAR_APP_FINGERPRINT" "$ORIGINAL_VAR_APP"
printf 'Disposable HOME: %s\n' "$HOME"
printf 'Disposable TMPDIR: %s\n' "$TMPDIR"
ACTIVATION_ENVIRONMENT=(
  HOME
  TMPDIR
  XDG_CONFIG_HOME
  XDG_CACHE_HOME
  XDG_DATA_HOME
  XDG_STATE_HOME
  XDG_DATA_DIRS
  XDG_RUNTIME_DIR
  FLATPAK_USER_DIR
  NOTM_FLATPAK_DISPOSABLE_ROOT
  NOTM_FLATPAK_SOURCE_ROOT
  NOTM_FLATPAK_FORBIDDEN_HOME_ENTRY
  NOTM_TEST_HARNESS_APPLICATION_ID
)
if [[ $DISABLE_SESSION_P11 == 1 ]]; then
  ACTIVATION_ENVIRONMENT+=(PATH)
fi
if [[ -n ${WAYLAND_DISPLAY:-} ]]; then
  ACTIVATION_ENVIRONMENT+=(WAYLAND_DISPLAY)
fi
if [[ -n ${DISPLAY:-} ]]; then
  ACTIVATION_ENVIRONMENT+=(DISPLAY)
fi
dbus-update-activation-environment "${ACTIVATION_ENVIRONMENT[@]}"
if ! "${BUILDER_COMMAND[@]}" --version >/dev/null; then
  printf 'error: Flatpak builder command failed: %q ' "${BUILDER_COMMAND[@]}" >&2
  printf '\n' >&2
  exit 69
fi

if [[ -e $HOST_APP_DATA || -L $HOST_APP_DATA ]]; then
  cat >&2 <<EOF
error: refusing to touch existing Flatpak data at $HOST_APP_DATA
uninstall the existing test installation with --delete-data or run this gate as a disposable user
EOF
  exit 73
fi

printf '%s\n' '== Static metadata and manifest validation =='
desktop-file-validate \
  "$SOURCE_ROOT/packaging/$APP_ID.desktop"
appstreamcli validate --no-net \
  "$SOURCE_ROOT/packaging/flatpak/$APP_ID.metainfo.xml"
"${BUILDER_COMMAND[@]}" --show-manifest "$MANIFEST" >"$WORK_ROOT/manifest.json"

python3 - "$WORK_ROOT/manifest.json" <<'PY'
import json
import sys

manifest = json.load(open(sys.argv[1], encoding="utf-8"))
expected_finish_args = {
    "--share=network",
    "--share=ipc",
    "--socket=wayland",
    "--socket=fallback-x11",
    "--device=dri",
    "--filesystem=~/Mail",
    "--filesystem=~/.mail",
    "--filesystem=~/.notmuch-config:ro",
}
actual_finish_args = set(manifest.get("finish-args", []))
if actual_finish_args != expected_finish_args:
    raise SystemExit(
        f"finish-args differ: expected {sorted(expected_finish_args)!r}, "
        f"got {sorted(actual_finish_args)!r}"
    )
if manifest.get("id") != "io.github.kris004.notm":
    raise SystemExit("manifest app ID differs")
if manifest.get("runtime") != "org.gnome.Platform" or manifest.get("runtime-version") != "50":
    raise SystemExit("manifest must use org.gnome.Platform//50")
if manifest.get("sdk") != "org.gnome.Sdk":
    raise SystemExit("manifest must use org.gnome.Sdk")
if manifest.get("sdk-extensions") != [
    "org.freedesktop.Sdk.Extension.rust-stable",
    "org.freedesktop.Sdk.Extension.llvm20",
]:
    raise SystemExit("manifest must use the Rust and LLVM 20 SDK extensions")

modules = manifest.get("modules", [])
app = next((module for module in modules if module.get("name") == "notm"), None)
if app is None:
    raise SystemExit("notm module is absent")
git_sources = [source for source in app.get("sources", []) if source.get("type") == "git"]
if git_sources != [
    {
        "type": "git",
        "url": "https://github.com/kris004/notm.git",
        "tag": "v0.1.2",
        "commit": "a4f90fcc642ef566af256d20390917a56a455373",
    }
]:
    raise SystemExit(f"application source is not the pinned v0.1.2 commit: {git_sources!r}")

for module in modules:
    for source in module.get("sources", []):
        if source.get("type") == "archive" and not source.get("sha256"):
            raise SystemExit(f"archive source lacks sha256: {module.get('name')}: {source!r}")
        if source.get("type") == "git" and not source.get("commit"):
            raise SystemExit(f"git source lacks commit: {module.get('name')}: {source!r}")
PY

python3 - "$SOURCE_ROOT/packaging/flatpak/$APP_ID.metainfo.xml" <<'PY'
import sys
import xml.etree.ElementTree as ElementTree

images = [
    image.text
    for image in ElementTree.parse(sys.argv[1]).getroot().findall("./screenshots/screenshot/image")
]
expected = [
    "https://raw.githubusercontent.com/kris004/notm/"
    "a4f90fcc642ef566af256d20390917a56a455373/docs/assets/notm.png"
]
if images != expected:
    raise SystemExit(f"AppStream screenshot is not commit-pinned: {images!r}")
PY

"${LINTER_COMMAND[@]}" manifest "$MANIFEST"
"${LINTER_COMMAND[@]}" appstream \
  "$SOURCE_ROOT/packaging/flatpak/$APP_ID.metainfo.xml"

printf '%s\n' '== Input hashes =='
sha256sum \
  "$MANIFEST" \
  "$SOURCE_ROOT/packaging/flatpak/cargo-sources.json" \
  "$SOURCE_ROOT/packaging/$APP_ID.desktop" \
  "$SOURCE_ROOT/packaging/flatpak/$APP_ID.metainfo.xml"

printf '%s\n' '== Isolated runtime and source download =='
flatpak remote-add \
  --user \
  --if-not-exists \
  --from \
  flathub \
  https://dl.flathub.org/repo/flathub.flatpakrepo
INSTALL_DEPS_ARGUMENTS=(--install-deps-from=flathub)
if [[ $USE_INSTALLED_RUNTIMES == 1 ]]; then
  INSTALL_DEPS_ARGUMENTS=()
  printf '%s\n' 'Using preinstalled runtimes from the disposable Flatpak installation:'
  for ref in \
    'org.gnome.Platform//50' \
    'org.gnome.Sdk//50' \
    'org.freedesktop.Sdk.Extension.rust-stable//25.08' \
    'org.freedesktop.Sdk.Extension.llvm20//25.08'; do
    flatpak info --user --show-commit "$ref"
  done
fi
"${BUILDER_COMMAND[@]}" \
  --user \
  --force-clean \
  --disable-rofiles-fuse \
  --disable-updates \
  --download-only \
  "${INSTALL_DEPS_ARGUMENTS[@]}" \
  --state-dir="$STATE_DIR" \
  "$BUILD_DIR" \
  "$MANIFEST"

printf '%s\n' '== Clean offline rebuild and repository export =='
rm -rf -- "$BUILD_DIR"
"${BUILDER_COMMAND[@]}" \
  --user \
  --force-clean \
  --disable-rofiles-fuse \
  --disable-cache \
  --disable-download \
  --disable-updates \
  --compose-url-policy=full \
  --mirror-screenshots-url=https://dl.flathub.org/media \
  --state-dir="$STATE_DIR" \
  --repo="$REPO_DIR" \
  "$BUILD_DIR" \
  "$MANIFEST"

flatpak repo "$REPO_DIR"
"${LINTER_COMMAND[@]}" builddir "$BUILD_DIR"
"${LINTER_COMMAND[@]}" repo "$REPO_DIR"

# The Flatpak-packaged Builder needs this marker while it runs, but leaving it
# in the outer process's private runtime makes the host Flatpak CLI incorrectly
# treat later install/run checks as nested sandbox calls.
clear_builder_sandbox_marker

mapfile -d '' -t SOURCE_GIT_REPOSITORIES < <(
  find "$STATE_DIR/git" \
    -mindepth 1 \
    -maxdepth 1 \
    -type d \
    -name 'https_github.com_kris004_notm.git*' \
    -print0
)
if [[ ${#SOURCE_GIT_REPOSITORIES[@]} -ne 1 ]]; then
  printf 'error: expected one pinned app source repository, found %d\n' \
    "${#SOURCE_GIT_REPOSITORIES[@]}" >&2
  exit 1
fi
SOURCE_GIT_REPOSITORY=${SOURCE_GIT_REPOSITORIES[0]}
readonly SOURCE_GIT_REPOSITORY
git --git-dir="$SOURCE_GIT_REPOSITORY" cat-file -e "$APP_SOURCE_COMMIT^{commit}"
git --git-dir="$SOURCE_GIT_REPOSITORY" \
  cat-file -e "$APP_SOURCE_COMMIT:docs/assets/notm.png"
source_screenshot_digest=$(
  git --git-dir="$SOURCE_GIT_REPOSITORY" \
    show "$APP_SOURCE_COMMIT:docs/assets/notm.png" |
    sha256sum
) || {
  printf '%s\n' 'error: could not hash the pinned source screenshot' >&2
  exit 1
}
source_screenshot_digest=${source_screenshot_digest%% *}
if [[ $source_screenshot_digest != "$SOURCE_SCREENSHOT_SHA256" ]]; then
  printf 'error: pinned source screenshot digest mismatch: %s\n' \
    "$source_screenshot_digest" >&2
  exit 1
fi
mapfile -d '' -t MIRRORED_SCREENSHOTS < <(
  find "$BUILD_DIR/files/share/app-info/media" \
    -type f -path '*/screenshots/image-1_orig.png' -print0
)
if [[ ${#MIRRORED_SCREENSHOTS[@]} -ne 1 ]]; then
  printf 'error: expected one original-size mirrored screenshot, found %d\n' \
    "${#MIRRORED_SCREENSHOTS[@]}" >&2
  exit 1
fi
# appstreamcli losslessly rewrites the commit-pinned PNG while composing media.
printf '%s  %s\n' "$MIRRORED_SCREENSHOT_SHA256" "${MIRRORED_SCREENSHOTS[0]}" | sha256sum -c -

printf '%s\n' '== Temporary user installation =='
flatpak remote-add \
  --user \
  --if-not-exists \
  --no-enumerate \
  --no-gpg-verify \
  "$LOCAL_REMOTE" \
  "file://$REPO_DIR"
flatpak install \
  --user \
  --noninteractive \
  --assumeyes \
  "$LOCAL_REMOTE" \
  "$APP_ID"

APP_LOCATION=$(flatpak info --user --show-location "$APP_ID")
readonly APP_LOCATION
APP_COMMIT=$(flatpak info --user --show-commit "$APP_ID")
readonly APP_COMMIT
readonly APP_BINARY="$APP_LOCATION/files/bin/notm"
printf 'Flatpak commit: %s\n' "$APP_COMMIT"
printf 'Flatpak location: %s\n' "$APP_LOCATION"
sha256sum "$APP_BINARY"

printf '%s\n' '== Installed integration, permissions, and ELF =='
test -x "$APP_BINARY"
test -x "$APP_LOCATION/files/bin/notmuch"
test -x "$APP_LOCATION/files/bin/msmtp"
test -f "$APP_LOCATION/files/share/applications/$APP_ID.desktop"
test -f "$APP_LOCATION/files/share/metainfo/$APP_ID.metainfo.xml"
test -f "$APP_LOCATION/files/share/icons/hicolor/scalable/apps/$APP_ID.svg"
test -f "$APP_LOCATION/files/share/licenses/$APP_ID/notm/LICENSE"

desktop-file-validate \
  "$APP_LOCATION/files/share/applications/$APP_ID.desktop"
appstreamcli validate --no-net \
  "$APP_LOCATION/files/share/metainfo/$APP_ID.metainfo.xml"
grep -Fx 'Exec=notm launch %u' \
  "$APP_LOCATION/files/share/applications/$APP_ID.desktop" >/dev/null

PERMISSIONS=$(flatpak info --user --show-permissions "$APP_ID")
printf '%s\n' "$PERMISSIONS"
for permission in \
  network \
  ipc \
  wayland \
  fallback-x11 \
  dri \
  "${HOME_MARKER}/Mail" \
  "${HOME_MARKER}/.mail" \
  "${HOME_MARKER}/.notmuch-config:ro"; do
  grep -F -- "$permission" <<<"$PERMISSIONS" >/dev/null || {
    printf 'error: installed permissions omit %s\n' "$permission" >&2
    exit 1
  }
done
for forbidden in \
  'filesystems=home' \
  'filesystems=host' \
  'host-root' \
  'session-bus' \
  'org.freedesktop.Flatpak' \
  "${HOME_MARKER}/.ssh" \
  "${HOME_MARKER}/.gnupg"; do
  if grep -F -- "$forbidden" <<<"$PERMISSIONS" >/dev/null; then
    printf 'error: forbidden broad or secret-bearing permission present: %s\n' "$forbidden" >&2
    exit 1
  fi
done

FILE_OUTPUT=$(file "$APP_BINARY")
printf '%s\n' "$FILE_OUTPUT"
READ_ELF_HEADER=$(readelf --file-header "$APP_BINARY")
printf '%s\n' "$READ_ELF_HEADER"
case $(uname -m) in
  aarch64)
    grep -F 'Machine:' <<<"$READ_ELF_HEADER" | grep -F 'AArch64' >/dev/null
    ;;
  x86_64)
    grep -F 'Machine:' <<<"$READ_ELF_HEADER" | grep -F 'Advanced Micro Devices X86-64' >/dev/null
    ;;
  *)
    printf 'error: unsupported Flatpak E2E host architecture: %s\n' "$(uname -m)" >&2
    exit 1
    ;;
esac
if readelf --dynamic "$APP_BINARY" | grep -E '\((RPATH|RUNPATH)\)' >/dev/null; then
  printf '%s\n' 'error: Flatpak notm binary contains RPATH/RUNPATH' >&2
  exit 1
fi

# The variables in this program are expanded by the shell inside the sandbox.
# shellcheck disable=SC2016
RUNTIME_OUTPUT=$(flatpak run --user --command=sh "$APP_ID" -c '
  set -eu
  test "$(/app/bin/notm --version)" = "notm 0.1.2"
  /app/bin/notmuch --version | grep -F "notmuch 0.40" >/dev/null
  /app/bin/msmtp --version | grep -F "msmtp version 1.8.33" >/dev/null
  ldd /app/bin/notm
')
printf '%s\n' "$RUNTIME_OUTPUT"
if grep -F 'not found' <<<"$RUNTIME_OUTPUT" >/dev/null; then
  printf '%s\n' 'error: Flatpak runtime has unresolved notm dependencies' >&2
  exit 1
fi

printf '%s\n' '== Runtime build evidence =='
flatpak info --user "$APP_ID"
for ref in \
  'org.gnome.Platform//50' \
  'org.gnome.Sdk//50' \
  'org.freedesktop.Sdk.Extension.rust-stable//25.08' \
  'org.freedesktop.Sdk.Extension.llvm20//25.08'; do
  flatpak info --user "$ref"
done

printf '%s\n' '== Disposable desktop and mailto routing =='
update-desktop-database "$FLATPAK_HOME/exports/share/applications"

# The production desktop entry starts a normal single-instance application,
# while an automation-enabled process intentionally uses an isolated test ID.
# After validating the installed production entry unchanged above, shadow only
# the disposable session's generated export so a mailto activation can use that
# same test ID and route back into the observable fixture process.
readonly EXPORTED_DESKTOP="$FLATPAK_HOME/exports/share/applications/$APP_ID.desktop"
readonly TEST_APPLICATIONS_DIR="$XDG_DATA_HOME/applications"
readonly TEST_DESKTOP="$TEST_APPLICATIONS_DIR/$APP_ID.desktop"
mkdir -p -- "$TEST_APPLICATIONS_DIR"
cp -L -- "$EXPORTED_DESKTOP" "$TEST_DESKTOP"
python3 - "$TEST_DESKTOP" <<'PY'
import sys
from pathlib import Path

path = Path(sys.argv[1])
contents = path.read_text(encoding="utf-8")
production = " launch @@u %u @@"
instrumented = " launch --test-harness --fixture @@u %u @@"
if contents.count(production) != 1:
    raise SystemExit("generated Flatpak desktop Exec has an unexpected launch route")
path.write_text(contents.replace(production, instrumented), encoding="utf-8")
PY
desktop-file-validate "$TEST_DESKTOP"
update-desktop-database "$TEST_APPLICATIONS_DIR"
xdg-mime default "$APP_ID.desktop" x-scheme-handler/mailto
if [[ $(xdg-mime query default x-scheme-handler/mailto) != "$APP_ID.desktop" ]]; then
  printf '%s\n' 'error: disposable session mailto handler is not the installed notm desktop entry' >&2
  exit 1
fi
dbus-update-activation-environment "${ACTIVATION_ENVIRONMENT[@]}"

printf '%s\n' '== Host-secret denial =='
SECRET_SENTINEL=$(mktemp "$HOME/.notm-flatpak-secret.XXXXXX")
printf '%s\n' 'notm Flatpak must not read this host-home sentinel' >"$SECRET_SENTINEL"
chmod 600 "$SECRET_SENTINEL"
# The positional parameter is expanded by the shell inside the sandbox.
# shellcheck disable=SC2016
SECRET_RESULT=$(flatpak run --user --command=sh "$APP_ID" -c '
  if test -e "$1"; then
    printf "%s\n" VISIBLE
    exit 90
  fi
  printf "%s\n" DENIED
' sh "$SECRET_SENTINEL")
if [[ $SECRET_RESULT != DENIED ]]; then
  printf 'error: host secret sentinel check returned %q\n' "$SECRET_RESULT" >&2
  exit 1
fi
printf '%s\n' "$SECRET_RESULT"
rm -f -- "$SECRET_SENTINEL"
SECRET_SENTINEL=

printf '%s\n' '== Sandboxed distribution E2E =='
mkdir -p "$HOST_APP_DATA/cache"
# Keep the harness root compact because Linux AF_UNIX pathname sockets are
# limited to 107 bytes and Flatpak app-data paths already contain the app ID.
DISTRIBUTION_ROOT="$HOST_APP_DATA/cache/e"
if [[ -e $DISTRIBUTION_ROOT || -L $DISTRIBUTION_ROOT ]]; then
  printf 'error: compact distribution work root already exists: %s\n' \
    "$DISTRIBUTION_ROOT" >&2
  exit 73
fi
mkdir -m 700 -- "$DISTRIBUTION_ROOT"
readonly DISTRIBUTION_ROOT
PATH_MAPPING_ROOT=$(mktemp -d "$HOST_APP_DATA/cache/path-mapping.XXXXXX")
readonly PATH_MAPPING_ROOT
readonly HOST_TO_SANDBOX_SENTINEL="$PATH_MAPPING_ROOT/host-to-sandbox"
readonly SANDBOX_TO_HOST_SENTINEL="$PATH_MAPPING_ROOT/sandbox-to-host"
printf '%s\n' 'host-to-sandbox-path-mapping' >"$HOST_TO_SANDBOX_SENTINEL"

# Flatpak keeps HOME but maps the app's XDG roots into its private app-data
# directory. The same absolute app-data path must be visible on both sides.
# shellcheck disable=SC2016
PATH_MAPPING_OUTPUT=$(flatpak run --user --command=sh "$APP_ID" -c '
  set -eu
  test "$HOME" = "$1"
  test "$XDG_CONFIG_HOME" = "$2"
  test "$XDG_CACHE_HOME" = "$3"
  test "$XDG_DATA_HOME" = "$4"
  test "$XDG_STATE_HOME" = "$5"
  test "$(cat "$6")" = "host-to-sandbox-path-mapping"
  printf "%s\n" "sandbox-to-host-path-mapping" >"$7"
  printf "HOME=%s\nXDG_CONFIG_HOME=%s\nXDG_CACHE_HOME=%s\nXDG_DATA_HOME=%s\nXDG_STATE_HOME=%s\n" \
    "$HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
' sh \
  "$HOME" \
  "$HOST_APP_DATA/config" \
  "$HOST_APP_DATA/cache" \
  "$HOST_APP_DATA/data" \
  "$HOST_APP_DATA/.local/state" \
  "$HOST_TO_SANDBOX_SENTINEL" \
  "$SANDBOX_TO_HOST_SENTINEL")
printf '%s\n' "$PATH_MAPPING_OUTPUT"
if [[ $(cat "$SANDBOX_TO_HOST_SENTINEL") != sandbox-to-host-path-mapping ]]; then
  printf '%s\n' 'error: Flatpak app-data path was not bidirectionally mapped' >&2
  exit 1
fi

readonly WRAPPER_DIR="$WORK_ROOT/wrappers"
readonly NOTM_WRAPPER="$WRAPPER_DIR/notm"
readonly NOTMUCH_WRAPPER="$WRAPPER_DIR/notmuch"
mkdir -p "$WRAPPER_DIR"

cat >"$NOTM_WRAPPER" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export FLATPAK_USER_DIR=$(printf '%q' "$FLATPAK_HOME")
exec flatpak run --user --command=notm $(printf '%q' "$APP_ID") "\$@"
EOF
cat >"$NOTMUCH_WRAPPER" <<EOF
#!/usr/bin/env bash
set -euo pipefail
export FLATPAK_USER_DIR=$(printf '%q' "$FLATPAK_HOME")
exec flatpak run --user --command=notmuch $(printf '%q' "$APP_ID") "\$@"
EOF
chmod 755 "$NOTM_WRAPPER" "$NOTMUCH_WRAPPER"

python3 -B "$SOURCE_ROOT/tests/distribution_e2e.py" \
  --notm "$NOTM_WRAPPER" \
  --notmuch "$NOTMUCH_WRAPPER" \
  --smtp-command /app/bin/msmtp \
  --work-root "$DISTRIBUTION_ROOT" \
  --require-display \
  --exercise-portal-link \
  --preserve-home

for attempt in {1..150}; do
  if ! FLATPAK_PROCESSES=$(flatpak ps --columns=application); then
    printf '%s\n' 'error: could not enumerate Flatpak processes after the E2E' >&2
    exit 1
  fi
  if ! grep -Fx "$APP_ID" <<<"$FLATPAK_PROCESSES" >/dev/null; then
    break
  fi
  if [[ $attempt -eq 150 ]]; then
    printf '%s\n' \
      'error: Flatpak notm process remained after the restart-backed E2E' >&2
    exit 1
  fi
  sleep 0.1
done

printf '%s\n' '== Uninstall and data rollback =='
flatpak uninstall \
  --user \
  --noninteractive \
  --assumeyes \
  --delete-data \
  "$APP_ID"
if flatpak info --user "$APP_ID" >/dev/null 2>&1; then
  printf '%s\n' 'error: Flatpak app remains installed after uninstall' >&2
  exit 1
fi
if [[ -e $HOST_APP_DATA || -L $HOST_APP_DATA ]]; then
  printf 'error: --delete-data left application data at %s\n' "$HOST_APP_DATA" >&2
  exit 1
fi
flatpak remote-delete --user --force "$LOCAL_REMOTE"

printf '%s\n' 'Flatpak offline build, temporary install, sandbox E2E, and uninstall passed'
