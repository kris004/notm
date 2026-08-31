# Releasing notm

This page records the notm-specific inputs to the shared signed, immutable
release policy.

## Historical boundary

The existing `v0.1.0` and `v0.1.1` releases predate the policy. Their annotated
tags are unsigned and the releases are not immutable. They are historical
records and must not be retagged, moved, or replaced.

## Release inputs

A real release updates these version surfaces together:

- the workspace version in `Cargo.toml` and every local-package reflection in
  `Cargo.lock`;
- the latest released heading, date, and link in `CHANGELOG.md`;
- the versioned `.TH` headers in all four files under `docs/man/`;
- the latest release version and date in
  `packaging/io.github.kris004.notm.metainfo.xml`; and
- the stable application ID, binary, desktop launch command, icon, and mailto
  declarations shared by Cargo, the Makefile, the desktop entry, and AppStream.

`Version=1.0` in the desktop entry is the Desktop Entry specification version,
not the notm release version. Do not change it during a release bump. The
authoritative consistency check and its deterministic negative tests are:

```sh
packaging/verify-release-metadata.py .
python3 -B tests/release_metadata_smoke.py
```

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

It must produce one canonical `release-assets` workflow artifact containing
exactly:

- `notm-vVERSION-x86_64-unknown-linux-gnu.tar.gz`
- `notm-vVERSION-aarch64-unknown-linux-gnu.tar.gz`
- `notm-vVERSION-src.tar.gz`
- `SHA256SUMS`

ARM delivery is split across two independent native build jobs and a separate
comparison job. The comparison job downloads build A and build B by exact
artifact ID and requires their extracted stripped binaries and
`binary.sha256`, `archive.sha256`, and `reproducibility-evidence.txt` evidence
to match exactly. Build B runs the packaged application under WebKitGTK's normal
sandbox on a native `ubuntu-24.04-arm` runner. A skipped or unavailable native
execution gate is a release failure. The comparison job runs only after both
native jobs succeed and re-uploads the verified ARM fragment for aggregation.

The x86_64/source and compared ARM64 fragments are also downloaded by exact
artifact ID. Their source commit, version, names, checksums, and embedded build
identity must agree before the aggregator creates the canonical four-file
`release-assets` artifact. Intermediate fragments are never attested,
published, or selected by artifact name; attestation and publication consume
only the aggregator's exact artifact ID.

Both binaries are dynamically linked GNU/Linux builds produced on native
Ubuntu 24.04 runners for their named architecture. In the production
Git-checkout path, the source archive extracts to `notm-VERSION/` and must equal
`git archive` of the direct tag target. Git records that target in the tar PAX
header, and `.git_archival.txt` carries the same commit into the extracted tree
through `export-subst`. When the builder is itself run from that extracted tree
without `.git`, it instead creates a deterministic tar from the archive
contents and preserves the embedded commit in the PAX header. The standalone
x86_64/source verifier validates its pre-aggregation three-file fragment,
checksums, binary version, build information, and release metadata without
needing a `.git` directory:

```sh
packaging/verify-linux-release.sh \
  dist VERSION x86_64-unknown-linux-gnu FULL_SOURCE_COMMIT
```

`tests/arm64_release_smoke.sh` statically exercises deterministic ARM assembly,
fragment verification, and canonical aggregation on any supported host. Its
fixture executable is not native ARM execution evidence; the reusable
`release-linux-arm64.yml` workflow supplies that evidence.

`tests/source_archive_smoke.sh` additionally compiles and tests a clean
extraction with `--locked`, runs its fixture and packaging suites, and proves
the archive-side release smoke works without repository metadata.

## Signing values

The repository uses the configured hardware-backed OpenPGP key with fingerprint
`BE592562E6131A53F4BADE4A046928E9A919BAF9`. For the next legitimate version,
the corresponding minimal public key is pinned in
`docs/release-signing-key.asc`. The release workflow verifies the tag against
that key as well as GitHub's tag-object verification.

The pinned primary currently expires at **2026-11-13 22:55:31 UTC**. CI and the
release workflow warn when 90 days or less remain and fail when 30 days or less
remain, when the key is expired, or when the pinned certificate has no expiry.
The read-only check is:

```sh
packaging/check-release-key-expiry.sh \
  docs/release-signing-key.asc \
  BE592562E6131A53F4BADE4A046928E9A919BAF9
```

### Hardware-key expiry handoff

Complete an extension before the 30-day failure boundary on the trusted system
that has access to the certification-capable primary key and the hardware
token. Choose an explicitly approved ISO expiry date more than 90 days away,
then run:

```sh
fingerprint=BE592562E6131A53F4BADE4A046928E9A919BAF9
authentication_subkey=A707625241E8F3FCA82FF7E237AE3AC9F486FBC3
encryption_subkey=9556DD524A4983D7E67F223098B779AB58AD4357
new_expiry=YYYY-MM-DD

gpg --list-secret-keys --with-subkey-fingerprint "$fingerprint"
gpg --quick-set-expire \
  "$fingerprint" \
  "$new_expiry"
gpg --quick-set-expire \
  "$fingerprint" \
  "$new_expiry" \
  "$authentication_subkey" \
  "$encryption_subkey"

updated_key=$(mktemp)
trap 'rm -f "$updated_key"' EXIT
gpg --batch --armor --export-options export-minimal \
  --export "$fingerprint" >"$updated_key"
packaging/check-release-key-expiry.sh "$updated_key" "$fingerprint"
gpg --batch --show-keys --with-subkey-fingerprints "$updated_key"
install -m 0644 "$updated_key" docs/release-signing-key.asc
```

The first expiry command updates the primary key; the second updates the named
authentication and encryption subkeys. Supplying subkey fingerprints does not
also update the primary key. The expiry update and export require no new key or
fingerprint, but GnuPG may request the hardware-key PIN and touch. If the
certification-capable primary key is unavailable, stop; do not generate a
replacement or weaken the policy as a workaround. Re-run the complete delivery
gate, publish the refreshed public certificate through the GitHub account's
**SSH and GPG keys** settings, and confirm GitHub reports the same fingerprint
and new expiry before signing a tag. If GitHub requires deleting and re-adding
the existing public key, treat that account change as a separate human-approved
handoff and verify historical signature records before and after it.

If extension is intentionally replaced by rotation, use a separately reviewed
change to update the pinned public certificate, workflow fingerprint, Git
signing configuration, GitHub account key, tests, and these instructions
together. Rotation never authorizes moving or recreating an existing tag.

After the expiry gate is healthy, record the exact protected-main commit and
run:

```sh
repo=kris004/notm
tag=vMAJOR.MINOR.PATCH
target=$(gh api "repos/${repo}/commits/main" --jq .sha)
signing_key='BE592562E6131A53F4BADE4A046928E9A919BAF9!'

test "$(git rev-parse origin/main)" = "$target"
test -z "$(git tag --list "$tag")"
git tag -s -u "$signing_key" "$tag" "$target" -m "notm ${tag#v}"
```

For an ordinary release after the expiry policy is healthy, the YubiKey touch
requested by `git tag -s` is the only human-presence step. Before pushing,
verify the exact local object:

```sh
git verify-tag --raw "$tag"
test "$(git cat-file -t "$tag")" = tag
test "$(git rev-parse "${tag}^{commit}")" = "$target"
git push origin "refs/tags/${tag}"
```

The tag push runs `.github/workflows/release-linux.yml`. It rejects an annotated
tag unless GitHub reports a direct commit target with a valid signature, the
target is reachable from the default branch, and both required checks succeeded.
It tests and builds the production binary, packages it once, independently
rebuilds and tests the extracted source archive, waits for both independent
native ARM builds and their exact comparison, and forms one canonical four-file
artifact from exact artifact IDs. It attests that exact aggregator artifact and
gives those same files to a release creation command with explicit
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
