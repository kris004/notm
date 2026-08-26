#!/bin/sh

# Exercise the pinned release-key expiry policy without secret key material.

set -eu

project_root=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly project_root
checker="$project_root/packaging/check-release-key-expiry.sh"
public_key="$project_root/docs/release-signing-key.asc"
expected_fingerprint=BE592562E6131A53F4BADE4A046928E9A919BAF9
warn_seconds=$((90 * 86400))
fail_seconds=$((30 * 86400))
readonly \
  checker public_key expected_fingerprint warn_seconds fail_seconds

work_root=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-key-expiry-smoke.XXXXXX")
readonly work_root
gnupg_home="$work_root/gnupg"
stdout="$work_root/stdout"
stderr="$work_root/stderr"
readonly gnupg_home stdout stderr

cleanup() {
  rm -rf -- "$work_root"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 -- "$gnupg_home"
key_listing=$(
  GNUPGHOME="$gnupg_home" gpg \
    --no-options \
    --batch \
    --with-colons \
    --fixed-list-mode \
    --import-options show-only \
    --import "$public_key" 2>/dev/null
)
actual_fingerprint=$(
  printf '%s\n' "$key_listing" |
    awk -F: '$1 == "fpr" { print $10; exit }'
)
expiry_epoch=$(
  printf '%s\n' "$key_listing" |
    awk -F: '$1 == "pub" { print $7; exit }'
)
test "$actual_fingerprint" = "$expected_fingerprint"
case "$expiry_epoch" in
  '' | 0 | *[!0-9]*)
    printf 'pinned public key has no usable expiry: %s\n' \
      "${expiry_epoch:-none}" >&2
    exit 1
    ;;
esac

safe_epoch=$((expiry_epoch - warn_seconds - 1))
warning_epoch=$((expiry_epoch - warn_seconds))
failure_epoch=$((expiry_epoch - fail_seconds))
expired_epoch=$((expiry_epoch + 1))

"$checker" \
  "$public_key" \
  "$expected_fingerprint" \
  "$safe_epoch" >"$stdout" 2>"$stderr"
grep -F 'release signing key expiry ok:' "$stdout" >/dev/null
test ! -s "$stderr"

GITHUB_ACTIONS=true "$checker" \
  "$public_key" \
  "$expected_fingerprint" \
  "$warning_epoch" >"$stdout" 2>"$stderr"
grep -F \
  '::warning title=Release signing key expires soon::' \
  "$stdout" >/dev/null
grep -F 'warning: release signing key expires in 90 days' \
  "$stderr" >/dev/null

if "$checker" \
  "$public_key" \
  "$expected_fingerprint" \
  "$failure_epoch" >"$stdout" 2>"$stderr"; then
  printf '%s\n' \
    'release key expiry check accepted the exact failure boundary' >&2
  exit 1
fi
grep -F 'release signing key expires in 30 days or less' \
  "$stderr" >/dev/null

if "$checker" \
  "$public_key" \
  "$expected_fingerprint" \
  "$expired_epoch" >"$stdout" 2>"$stderr"; then
  printf '%s\n' 'release key expiry check accepted an expired key' >&2
  exit 1
fi
grep -F 'release signing key expired at epoch' "$stderr" >/dev/null

wrong_fingerprint=0000000000000000000000000000000000000000
if "$checker" \
  "$public_key" \
  "$wrong_fingerprint" \
  "$safe_epoch" >"$stdout" 2>"$stderr"; then
  printf '%s\n' \
    'release key expiry check accepted the wrong fingerprint' >&2
  exit 1
fi
grep -F 'public key fingerprint mismatch:' "$stderr" >/dev/null

printf '%s\n' 'release_key_expiry_smoke ok'
