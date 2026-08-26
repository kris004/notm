#!/bin/sh

# Enforce the expiry policy for one pinned OpenPGP primary key.

set -eu

warn_days=90
fail_days=30
seconds_per_day=86400
readonly warn_days fail_days seconds_per_day

usage() {
  printf '%s\n' \
    "usage: $0 PUBLIC_KEY EXPECTED_PRIMARY_FINGERPRINT [CURRENT_EPOCH]" >&2
}

if [ "$#" -lt 2 ] || [ "$#" -gt 3 ]; then
  usage
  exit 2
fi

public_key=$1
expected_fingerprint=$2

if [ ! -f "$public_key" ]; then
  printf 'public key is not a file: %s\n' "$public_key" >&2
  exit 2
fi
if ! printf '%s\n' "$expected_fingerprint" |
  grep -Eq '^[0-9A-F]{40}$'; then
  printf 'invalid primary fingerprint: %s\n' "$expected_fingerprint" >&2
  exit 2
fi

if [ "$#" -eq 3 ]; then
  current_epoch=$3
else
  current_epoch=$(date +%s)
fi
case "$current_epoch" in
  '' | *[!0-9]*)
    printf 'current epoch must be a non-negative integer: %s\n' \
      "$current_epoch" >&2
    exit 2
    ;;
esac
case "$current_epoch" in
  0 | [1-9]*) ;;
  *)
    printf 'current epoch must be canonical decimal: %s\n' \
      "$current_epoch" >&2
    exit 2
    ;;
esac

gnupg_home=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-key-expiry.XXXXXX")
readonly gnupg_home
chmod 700 "$gnupg_home"

cleanup() {
  rm -rf -- "$gnupg_home"
}
trap cleanup EXIT HUP INT TERM

if ! key_listing=$(
  GNUPGHOME="$gnupg_home" gpg \
    --no-options \
    --batch \
    --with-colons \
    --fixed-list-mode \
    --import-options show-only \
    --import "$public_key" 2>/dev/null
); then
  printf 'unable to parse public key: %s\n' "$public_key" >&2
  exit 1
fi

if ! primary_record=$(
  printf '%s\n' "$key_listing" |
    awk -F: '
      $1 == "pub" {
        primary_count++
        if (primary_count == 1) {
          expiry = $7
          need_fingerprint = 1
        }
        next
      }
      need_fingerprint && $1 == "fpr" {
        fingerprint = $10
        need_fingerprint = 0
      }
      END {
        if (primary_count != 1 || fingerprint == "") {
          exit 1
        }
        print fingerprint ":" expiry
      }
    '
); then
  printf '%s\n' \
    'public key must contain exactly one OpenPGP primary key' >&2
  exit 1
fi

key_fingerprint=${primary_record%%:*}
expiry_epoch=${primary_record#*:}
if [ "$key_fingerprint" != "$expected_fingerprint" ]; then
  printf 'public key fingerprint mismatch: expected %s, got %s\n' \
    "$expected_fingerprint" \
    "${key_fingerprint:-none}" >&2
  exit 1
fi

case "$expiry_epoch" in
  '' | 0)
    printf 'release signing key has no expiry: %s\n' \
      "$expected_fingerprint" >&2
    exit 1
    ;;
  *[!0-9]*)
    printf 'release signing key has an unsupported expiry value: %s\n' \
      "$expiry_epoch" >&2
    exit 1
    ;;
esac

remaining_seconds=$((expiry_epoch - current_epoch))
if [ "$remaining_seconds" -le 0 ]; then
  printf 'release signing key expired at epoch %s: %s\n' \
    "$expiry_epoch" \
    "$expected_fingerprint" >&2
  exit 1
fi

remaining_days=$(((remaining_seconds + seconds_per_day - 1) / seconds_per_day))
fail_seconds=$((fail_days * seconds_per_day))
warn_seconds=$((warn_days * seconds_per_day))

if [ "$remaining_seconds" -le "$fail_seconds" ]; then
  printf 'release signing key expires in %s days or less at epoch %s: %s\n' \
    "$fail_days" \
    "$expiry_epoch" \
    "$expected_fingerprint" >&2
  exit 1
fi

if [ "$remaining_seconds" -le "$warn_seconds" ]; then
  warning_message="release signing key expires in ${remaining_days} days at epoch ${expiry_epoch}: ${expected_fingerprint}"
  printf 'warning: %s\n' "$warning_message" >&2
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    printf '::warning title=Release signing key expires soon::%s\n' \
      "$warning_message"
  fi
  exit 0
fi

printf 'release signing key expiry ok: %s days remain (epoch %s): %s\n' \
  "$remaining_days" \
  "$expiry_epoch" \
  "$expected_fingerprint"
