#!/bin/sh

# Verify an annotated Git tag against one pinned OpenPGP primary key.

set -eu

usage() {
  printf '%s\n' \
    "usage: $0 REPOSITORY TAG PUBLIC_KEY EXPECTED_PRIMARY_FINGERPRINT" >&2
}

if [ "$#" -ne 4 ]; then
  usage
  exit 2
fi

REPOSITORY=$1
TAG=$2
PUBLIC_KEY=$3
EXPECTED_FINGERPRINT=$4

if [ ! -d "$REPOSITORY" ]; then
  printf 'repository is not a directory: %s\n' "$REPOSITORY" >&2
  exit 2
fi
if [ ! -f "$PUBLIC_KEY" ]; then
  printf 'public key is not a file: %s\n' "$PUBLIC_KEY" >&2
  exit 2
fi
if ! printf '%s\n' "$EXPECTED_FINGERPRINT" |
  grep -Eq '^[0-9A-F]{40}$'; then
  printf 'invalid primary fingerprint: %s\n' "$EXPECTED_FINGERPRINT" >&2
  exit 2
fi

if [ "$(git -C "$REPOSITORY" cat-file -t "$TAG" 2>/dev/null)" != tag ]; then
  printf 'release tag is not an annotated tag object: %s\n' "$TAG" >&2
  exit 1
fi

GNUPG_HOME=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-gpg.XXXXXX")
readonly GNUPG_HOME
chmod 700 "$GNUPG_HOME"

cleanup() {
  rm -rf -- "$GNUPG_HOME"
}
trap cleanup EXIT HUP INT TERM

key_listing=$(
  GNUPGHOME="$GNUPG_HOME" gpg \
    --batch \
    --with-colons \
    --import-options show-only \
    --import "$PUBLIC_KEY" 2>/dev/null
)
key_fingerprint=$(
  printf '%s\n' "$key_listing" |
    awk -F: '$1 == "fpr" { print $10; exit }'
)
if [ "$key_fingerprint" != "$EXPECTED_FINGERPRINT" ]; then
  printf 'public key fingerprint mismatch: expected %s, got %s\n' \
    "$EXPECTED_FINGERPRINT" \
    "${key_fingerprint:-none}" >&2
  exit 1
fi

GNUPGHOME="$GNUPG_HOME" gpg \
  --batch \
  --quiet \
  --import "$PUBLIC_KEY"

if ! verify_output=$(
  GNUPGHOME="$GNUPG_HOME" git -C "$REPOSITORY" \
    -c gpg.program=gpg \
    verify-tag --raw "$TAG" 2>&1
); then
  printf '%s\n' "$verify_output" >&2
  printf 'release tag signature is not valid for the pinned key: %s\n' \
    "$TAG" >&2
  exit 1
fi

validsig=$(
  printf '%s\n' "$verify_output" |
    sed -n 's/^\[GNUPG:\] VALIDSIG //p' |
    tail -n 1
)
signing_fingerprint=$(printf '%s\n' "$validsig" | awk '{ print $1 }')
primary_fingerprint=$(printf '%s\n' "$validsig" | awk '{ print $10 }')
primary_fingerprint=${primary_fingerprint:-$signing_fingerprint}
if [ "$signing_fingerprint" != "$EXPECTED_FINGERPRINT" ] &&
  [ "$primary_fingerprint" != "$EXPECTED_FINGERPRINT" ]; then
  printf 'valid signature does not belong to pinned primary key: %s\n' \
    "$EXPECTED_FINGERPRINT" >&2
  exit 1
fi

printf '%s\n' "$verify_output"
