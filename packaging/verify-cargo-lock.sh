#!/bin/sh

# Verify Cargo.lock against a digest recorded before any build command ran.

set -eu

if [ "$#" -ne 2 ]; then
  printf 'usage: %s LOCKFILE EXPECTED_SHA256\n' "$0" >&2
  exit 2
fi

lockfile=$1
expected_sha256=$2

if [ ! -f "$lockfile" ] || [ -L "$lockfile" ]; then
  printf 'Cargo lockfile is missing or not a regular file: %s\n' \
    "$lockfile" >&2
  exit 1
fi
if ! printf '%s\n' "$expected_sha256" | grep -Eq '^[0-9a-f]{64}$'; then
  printf 'invalid expected Cargo.lock SHA-256: %s\n' \
    "$expected_sha256" >&2
  exit 2
fi

actual_sha256=$(sha256sum "$lockfile" | awk '{ print $1 }')
if [ "$actual_sha256" != "$expected_sha256" ]; then
  printf 'Cargo.lock changed: expected SHA-256 %s, got %s\n' \
    "$expected_sha256" "$actual_sha256" >&2
  exit 1
fi

printf 'Cargo.lock unchanged: %s\n' "$actual_sha256"
