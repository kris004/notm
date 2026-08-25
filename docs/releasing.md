# Releasing notm

This page records the notm-specific inputs to the shared signed, immutable
release policy.

## Historical boundary

The existing `v0.1.0` and `v0.1.1` releases predate the policy. Their annotated
tags are unsigned and the releases are not immutable. They are historical
records and must not be retagged, moved, or replaced.

## Release inputs

A real release updates these version surfaces together:

- the workspace version in `Cargo.toml` and its `Cargo.lock` reflection;
- `CHANGELOG.md`; and
- the release entry in
  `packaging/io.github.kris004.notm.metainfo.xml`.

The exact release commit must be on protected `main` with these successful check
names:

- `Format, lint, test, and GTK smoke`
- `Analyze Rust`

The default-branch ruleset must strictly require both checks and CodeQL. The
`refs/tags/v*` ruleset must block deletion and non-fast-forward updates. Enable
release immutability before creating the next tag; it applies only to releases
published after the setting is enabled.

The non-publishing packaging dry run is:

```sh
gh workflow run release-linux.yml --repo kris004/notm --ref main
```

It must produce one `release-assets` workflow artifact containing exactly:

- `notm-vVERSION-x86_64-unknown-linux-gnu.tar.gz`
- `notm-vVERSION-src.tar.gz`
- `SHA256SUMS`

The binary is a dynamically linked x86-64 GNU/Linux build produced on Ubuntu
24.04. The source archive extracts to `notm-VERSION/` and must equal `git
archive` of the direct tag target.

## Signing values

The repository uses the configured hardware-backed OpenPGP key with fingerprint
`BE592562E6131A53F4BADE4A046928E9A919BAF9`. For the next legitimate version,
record the exact protected-main commit and run:

```sh
repo=kris004/notm
tag=vMAJOR.MINOR.PATCH
target=$(gh api "repos/${repo}/commits/main" --jq .sha)
signing_key='BE592562E6131A53F4BADE4A046928E9A919BAF9!'

test "$(git rev-parse origin/main)" = "$target"
test -z "$(git tag --list "$tag")"
git tag -s -u "$signing_key" "$tag" "$target" -m "notm ${tag#v}"
```

The YubiKey touch requested by `git tag -s` is the only human-presence step.
Before pushing, verify the exact local object:

```sh
git verify-tag --raw "$tag"
test "$(git cat-file -t "$tag")" = tag
test "$(git rev-parse "${tag}^{commit}")" = "$target"
git push origin "refs/tags/${tag}"
```

The tag push runs `.github/workflows/release-linux.yml`. It rejects an annotated
tag unless GitHub reports a direct commit target with a valid signature, the
target is reachable from the default branch, and both required checks succeeded.
It then tests, builds, and packages once; attests the exact uploaded workflow
artifact; and gives those same files to a release creation command with explicit
`--repo "$GITHUB_REPOSITORY"` context.

After publication, verify the repository-specific artifact set:

```sh
tmp=$(mktemp -d)
gh release download "$tag" --repo "$repo" --dir "$tmp"
(cd "$tmp" && sha256sum --check SHA256SUMS)
test "$(gh api "repos/${repo}/releases/tags/${tag}" --jq .immutable)" = true

for artifact in "$tmp"/*; do
  gh attestation verify "$artifact" \
    --repo "$repo" \
    --signer-workflow "$repo/.github/workflows/release-linux.yml" \
    --source-digest "$target" \
    --deny-self-hosted-runners
done
```

## Gentoo contract

The automatic Gentoo overlay continues to build from the commit-pinned GitHub
source archive:

```text
https://github.com/kris004/notm/archive/COMMIT.tar.gz
```

This release change adds a separately attested exact-source asset but does not
change that updater URL, the source tree layout used by the ebuild, dependencies,
USE flags, build commands, install paths, desktop metadata, or installed files.
The overlay's notm registry must add `Analyze Rust` to its required checks when
this workflow reaches `main`.

There is no project-specific deviation from the shared release policy.
