#!/bin/sh

# Exercise pinned OpenPGP verification with disposable keys and tags.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd
)
readonly PROJECT_ROOT
WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-release-tag.XXXXXX")
readonly WORK_ROOT
GNUPG_HOME="$WORK_ROOT/gnupg"
REPOSITORY="$WORK_ROOT/repository"
PUBLIC_KEY="$WORK_ROOT/release-signing-key.asc"
readonly GNUPG_HOME REPOSITORY PUBLIC_KEY

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

mkdir -m 700 -- "$GNUPG_HOME"
GNUPGHOME="$GNUPG_HOME" gpg \
  --batch \
  --passphrase '' \
  --quick-generate-key \
  'notm release test <release@example.invalid>' \
  rsa2048 \
  sign \
  1d >/dev/null 2>&1

fingerprint=$(
  GNUPGHOME="$GNUPG_HOME" gpg \
    --batch \
    --with-colons \
    --list-keys |
    awk -F: '$1 == "fpr" { print $10; exit }'
)
test -n "$fingerprint"
GNUPGHOME="$GNUPG_HOME" gpg \
  --batch \
  --armor \
  --output "$PUBLIC_KEY" \
  --export "$fingerprint"

git init --quiet --initial-branch=main "$REPOSITORY"
git -C "$REPOSITORY" config user.name 'notm release test'
git -C "$REPOSITORY" config user.email 'release@example.invalid'
printf '%s\n' fixture > "$REPOSITORY/fixture.txt"
git -C "$REPOSITORY" add fixture.txt
git -C "$REPOSITORY" commit --quiet -m fixture

GNUPGHOME="$GNUPG_HOME" git -C "$REPOSITORY" \
  -c user.signingkey="$fingerprint" \
  -c gpg.program=gpg \
  tag -s v1.0.0 -m 'notm 1.0.0'

"$PROJECT_ROOT/packaging/verify-release-tag.sh" \
  "$REPOSITORY" \
  v1.0.0 \
  "$PUBLIC_KEY" \
  "$fingerprint" >/dev/null

wrong_fingerprint=0000000000000000000000000000000000000000
if "$PROJECT_ROOT/packaging/verify-release-tag.sh" \
  "$REPOSITORY" \
  v1.0.0 \
  "$PUBLIC_KEY" \
  "$wrong_fingerprint" >/dev/null 2>&1; then
  printf '%s\n' 'verification accepted the wrong fingerprint' >&2
  exit 1
fi

git -C "$REPOSITORY" tag v1.0.1
if "$PROJECT_ROOT/packaging/verify-release-tag.sh" \
  "$REPOSITORY" \
  v1.0.1 \
  "$PUBLIC_KEY" \
  "$fingerprint" >/dev/null 2>&1; then
  printf '%s\n' 'verification accepted a lightweight tag' >&2
  exit 1
fi

git -C "$REPOSITORY" tag -a v1.0.2 -m 'unsigned tag'
if "$PROJECT_ROOT/packaging/verify-release-tag.sh" \
  "$REPOSITORY" \
  v1.0.2 \
  "$PUBLIC_KEY" \
  "$fingerprint" >/dev/null 2>&1; then
  printf '%s\n' 'verification accepted an unsigned annotated tag' >&2
  exit 1
fi

printf '%s\n' 'release_tag_smoke ok'
