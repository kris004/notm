#!/bin/sh

# Exercise the isolated ARM64 release builder without requiring ARM hardware.

set -eu

PROJECT_ROOT=$(
  CDPATH=''
  cd -- "$(dirname -- "$0")/.."
  pwd -P
)
readonly PROJECT_ROOT
ARM_WORKFLOW="$PROJECT_ROOT/.github/workflows/release-linux-arm64.yml"
readonly ARM_WORKFLOW
grep -F "            apparmor-profiles \\" "$ARM_WORKFLOW" >/dev/null
grep -F \
  'profile=/usr/share/apparmor/extra-profiles/bwrap-userns-restrict' \
  "$ARM_WORKFLOW" >/dev/null
grep -F "sudo apparmor_parser --replace \"\$profile\"" \
  "$ARM_WORKFLOW" >/dev/null
grep -F "          bwrap \\" "$ARM_WORKFLOW" >/dev/null
if grep -F 'WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1' \
  "$ARM_WORKFLOW" >/dev/null; then
  printf '%s\n' 'ARM64 workflow must not disable the WebKit sandbox' >&2
  exit 1
fi

WORK_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/notm-arm64-smoke.XXXXXX")
FAKE_BINARY="$WORK_ROOT/notm"
BUILD_INFO="$WORK_ROOT/BUILD-INFO.txt"
BAD_BUILD_INFO="$WORK_ROOT/BAD-BUILD-INFO.txt"
DIST_ONE="$WORK_ROOT/dist-one"
DIST_TWO="$WORK_ROOT/dist-two"
EVIDENCE_ONE="$WORK_ROOT/evidence-one"
EVIDENCE_TWO="$WORK_ROOT/evidence-two"
MISMATCH_EVIDENCE="$WORK_ROOT/evidence-mismatch"
BAD_DIGEST_EVIDENCE="$WORK_ROOT/evidence-bad-digest"
BAD_SOURCE_EVIDENCE="$WORK_ROOT/evidence-bad-source"
EXTRA_EVIDENCE="$WORK_ROOT/evidence-extra"
EXTRA_RELEASE="$WORK_ROOT/release-extra"
X86_64_DIST="$WORK_ROOT/dist-x86_64"
AGGREGATE_DIST="$WORK_ROOT/dist-aggregate"
BAD_ARM64_DIST="$WORK_ROOT/dist-bad-arm64"
SYMLINK_OUTPUT="$WORK_ROOT/dist-symlink"
SYMLINK_OUTPUT_TARGET="$WORK_ROOT/dist-symlink-target"
NON_HEAD_REPO="$WORK_ROOT/non-head-repository"
EXTRACT_ROOT="$WORK_ROOT/extract"
readonly \
  WORK_ROOT FAKE_BINARY BUILD_INFO BAD_BUILD_INFO DIST_ONE DIST_TWO \
  EVIDENCE_ONE EVIDENCE_TWO MISMATCH_EVIDENCE BAD_DIGEST_EVIDENCE \
  BAD_SOURCE_EVIDENCE EXTRA_EVIDENCE EXTRA_RELEASE X86_64_DIST \
  AGGREGATE_DIST BAD_ARM64_DIST SYMLINK_OUTPUT SYMLINK_OUTPUT_TARGET \
  NON_HEAD_REPO EXTRACT_ROOT

cleanup() {
  rm -rf -- "$WORK_ROOT"
}
trap cleanup EXIT HUP INT TERM

VERSION=$(
  "$PROJECT_ROOT/packaging/verify-release-metadata.py" \
    --print-version "$PROJECT_ROOT"
)
TARGET=aarch64-unknown-linux-gnu
SOURCE_COMMIT=$(git -C "$PROJECT_ROOT" rev-parse HEAD)
SOURCE_DATE_EPOCH=$(git -C "$PROJECT_ROOT" show -s --format=%ct "$SOURCE_COMMIT")
LOCK_SHA256=$(sha256sum "$PROJECT_ROOT/Cargo.lock" | awk '{print $1}')
BUNDLE_NAME="notm-v${VERSION}-${TARGET}"
ARCHIVE_NAME="$BUNDLE_NAME.tar.gz"
readonly \
  VERSION TARGET SOURCE_COMMIT SOURCE_DATE_EPOCH LOCK_SHA256 BUNDLE_NAME \
  ARCHIVE_NAME

cat >"$FAKE_BINARY" <<EOF
#!/bin/sh
printf '%s\\n' 'notm $VERSION'
EOF
chmod 755 "$FAKE_BINARY"
FAKE_BINARY_SHA256=$(sha256sum "$FAKE_BINARY" | awk '{print $1}')
readonly FAKE_BINARY_SHA256

cat >"$BUILD_INFO" <<EOF
Version: $VERSION
Source tag: v$VERSION
Source commit: $SOURCE_COMMIT
Source ref: refs/heads/arm64-smoke
Source date epoch: $SOURCE_DATE_EPOCH
Target: $TARGET
Build architecture: aarch64
Build environment: Ubuntu 24.04 ARM64 userspace
Runner label: fixture-arm64
Runner image version: fixture
Cargo.lock SHA-256: $LOCK_SHA256
Rust compiler: rustc 1.98.0 (fixture)
Cargo: cargo 1.98.0 (fixture)
Release binary SHA-256: $FAKE_BINARY_SHA256
Binary reproducibility: independent clean workflow comparison required
Required GLIBC symbols through: GLIBC_2.17
Validation runtime: GLIBC 2.39

Native build packages:
fixture-package\t1

Direct ELF dependencies:
libc.so.6
EOF

mkdir -p -- "$SYMLINK_OUTPUT_TARGET"
ln -s -- "$SYMLINK_OUTPUT_TARGET" "$SYMLINK_OUTPUT"
if "$PROJECT_ROOT/packaging/arm64/build-release.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$FAKE_BINARY" \
  "$BUILD_INFO" \
  "$SYMLINK_OUTPUT" >"$WORK_ROOT/symlink-output.out" 2>&1; then
  printf '%s\n' 'ARM64 builder accepted a symlink output directory' >&2
  exit 1
fi
grep -F 'release output directory must not be a symlink:' \
  "$WORK_ROOT/symlink-output.out" >/dev/null
test -z "$(find "$SYMLINK_OUTPUT_TARGET" -mindepth 1 -maxdepth 1 -print)"

"$PROJECT_ROOT/packaging/arm64/build-release.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$FAKE_BINARY" \
  "$BUILD_INFO" \
  "$DIST_ONE" >/dev/null
"$PROJECT_ROOT/packaging/arm64/build-release.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$FAKE_BINARY" \
  "$BUILD_INFO" \
  "$DIST_TWO" >/dev/null

git init --quiet --initial-branch=main "$NON_HEAD_REPO"
git -C "$NON_HEAD_REPO" config user.name 'notm ARM64 smoke'
git -C "$NON_HEAD_REPO" config user.email 'release@example.invalid'
git -C "$NON_HEAD_REPO" config commit.gpgsign false
git -C "$NON_HEAD_REPO" commit --quiet --allow-empty -m 'first'
OTHER_SOURCE_COMMIT=$(git -C "$NON_HEAD_REPO" rev-parse HEAD)
readonly OTHER_SOURCE_COMMIT
git -C "$NON_HEAD_REPO" commit --quiet --allow-empty -m 'second'
if "$PROJECT_ROOT/packaging/arm64/verify-release.sh" \
  "$NON_HEAD_REPO" \
  "$VERSION" \
  "$OTHER_SOURCE_COMMIT" \
  "$DIST_ONE/$ARCHIVE_NAME" \
  "$DIST_ONE/SHA256SUMS-aarch64" \
  >"$WORK_ROOT/wrong-head.out" 2>&1; then
  printf '%s\n' 'ARM64 verifier accepted a non-HEAD source commit' >&2
  exit 1
fi
grep -F 'source commit is not the checked-out HEAD:' \
  "$WORK_ROOT/wrong-head.out" >/dev/null

ARCHIVE_ONE="$DIST_ONE/$ARCHIVE_NAME"
ARCHIVE_TWO="$DIST_TWO/$ARCHIVE_NAME"
CHECKSUM_ONE="$DIST_ONE/SHA256SUMS-aarch64"
CHECKSUM_TWO="$DIST_TWO/SHA256SUMS-aarch64"
BUNDLE_ROOT="$EXTRACT_ROOT/$BUNDLE_NAME"
readonly \
  ARCHIVE_ONE ARCHIVE_TWO CHECKSUM_ONE CHECKSUM_TWO BUNDLE_ROOT

cmp "$ARCHIVE_ONE" "$ARCHIVE_TWO"
cmp "$CHECKSUM_ONE" "$CHECKSUM_TWO"
ARCHIVE_SHA256=$(sha256sum "$ARCHIVE_ONE" | awk '{print $1}')
readonly ARCHIVE_SHA256

write_reproducibility_evidence() {
  evidence_dir=$1

  mkdir -p -- "$evidence_dir"
  printf '%s  notm\n' "$FAKE_BINARY_SHA256" \
    >"$evidence_dir/binary.sha256"
  printf '%s  %s\n' "$ARCHIVE_SHA256" "$ARCHIVE_NAME" \
    >"$evidence_dir/archive.sha256"
  cat >"$evidence_dir/reproducibility-evidence.txt" <<EOF
Evidence format: notm ARM64 reproducibility v1
Version: $VERSION
Source tag: v$VERSION
Source commit: $SOURCE_COMMIT
Source ref: refs/heads/arm64-smoke
Source repository: https://example.invalid/notm
Source date epoch: $SOURCE_DATE_EPOCH
Cargo.lock SHA-256: $LOCK_SHA256
Target: $TARGET
Build architecture: aarch64
Build environment: Ubuntu 24.04 ARM64 userspace
Ubuntu ID: ubuntu
Ubuntu version ID: 24.04
Kernel architecture: aarch64
Debian architecture: arm64
Rust host: $TARGET
Runner label: fixture-arm64
Runner architecture: ARM64
Runner image OS: ubuntu24
Runner image version: fixture
Rust compiler: rustc 1.98.0 (fixture)
Cargo: cargo 1.98.0 (fixture)
Required GLIBC symbols through: GLIBC_2.17
Validation runtime: GLIBC 2.39
Reproducibility comparison: independent clean workflow comparison required

Native build packages:
EOF
  {
    printf 'libgtk-4-dev\tfixture\n'
    printf 'libgtksourceview-5-dev\tfixture\n'
    printf 'libnotmuch-dev\tfixture\n'
    printf 'libwebkitgtk-6.0-dev\tfixture\n'
    printf '\nRuntime package baseline:\n'
    printf 'libgtk-4-1\tfixture\n'
    printf 'libgtksourceview-5-0\tfixture\n'
    printf 'libnotmuch5t64\tfixture\n'
    printf 'libwebkitgtk-6.0-4\tfixture\n'
    printf '\nDirect ELF dependencies:\n'
    printf 'libc.so.6\n'
  } >>"$evidence_dir/reproducibility-evidence.txt"
}

write_reproducibility_evidence "$EVIDENCE_ONE"
write_reproducibility_evidence "$EVIDENCE_TWO"
"$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$DIST_TWO" \
  "$EVIDENCE_TWO" >/dev/null

cp -a -- "$EVIDENCE_TWO" "$MISMATCH_EVIDENCE"
sed -i \
  's/^Runner image version: fixture$/Runner image version: fixture-mismatch/' \
  "$MISMATCH_EVIDENCE/reproducibility-evidence.txt"
if "$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$DIST_TWO" \
  "$MISMATCH_EVIDENCE" >"$WORK_ROOT/compare-mismatch.out" 2>&1; then
  printf '%s\n' \
    'ARM64 comparator accepted mismatched reproducibility evidence' >&2
  exit 1
fi
grep -F 'canonical ARM64 reproducibility evidence differs:' \
  "$WORK_ROOT/compare-mismatch.out" >/dev/null

cp -a -- "$EVIDENCE_TWO" "$BAD_DIGEST_EVIDENCE"
printf '%s  notm\n' \
  0000000000000000000000000000000000000000000000000000000000000000 \
  >"$BAD_DIGEST_EVIDENCE/binary.sha256"
if "$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$DIST_TWO" \
  "$BAD_DIGEST_EVIDENCE" >"$WORK_ROOT/compare-bad-digest.out" 2>&1; then
  printf '%s\n' 'ARM64 comparator accepted an incorrect binary digest' >&2
  exit 1
fi
grep -F \
  'binary checksum evidence does not match an extracted release binary' \
  "$WORK_ROOT/compare-bad-digest.out" >/dev/null

cp -a -- "$EVIDENCE_TWO" "$BAD_SOURCE_EVIDENCE"
sed -i \
  's/^Cargo.lock SHA-256:.*/Cargo.lock SHA-256: 0000000000000000000000000000000000000000000000000000000000000000/' \
  "$BAD_SOURCE_EVIDENCE/reproducibility-evidence.txt"
if "$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$DIST_TWO" \
  "$BAD_SOURCE_EVIDENCE" >"$WORK_ROOT/compare-bad-source.out" 2>&1; then
  printf '%s\n' 'ARM64 comparator accepted mismatched source evidence' >&2
  exit 1
fi
grep -F 'reproducibility evidence is not canonical:' \
  "$WORK_ROOT/compare-bad-source.out" >/dev/null

cp -a -- "$EVIDENCE_TWO" "$EXTRA_EVIDENCE"
: >"$EXTRA_EVIDENCE/unexpected"
if "$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$DIST_TWO" \
  "$EXTRA_EVIDENCE" >"$WORK_ROOT/compare-extra-evidence.out" 2>&1; then
  printf '%s\n' 'ARM64 comparator accepted extra evidence' >&2
  exit 1
fi
grep -F 'evidence directory has 4 entries instead of 3:' \
  "$WORK_ROOT/compare-extra-evidence.out" >/dev/null

cp -a -- "$DIST_TWO" "$EXTRA_RELEASE"
: >"$EXTRA_RELEASE/unexpected"
if "$PROJECT_ROOT/packaging/arm64/compare-native-builds.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$DIST_ONE" \
  "$EVIDENCE_ONE" \
  "$EXTRA_RELEASE" \
  "$EVIDENCE_TWO" >"$WORK_ROOT/compare-extra-release.out" 2>&1; then
  printf '%s\n' 'ARM64 comparator accepted an extra release artifact' >&2
  exit 1
fi
grep -F 'release directory has 3 entries instead of 2:' \
  "$WORK_ROOT/compare-extra-release.out" >/dev/null

test "$(find "$DIST_ONE" -maxdepth 1 -type f | wc -l)" -eq 2
test "$(wc -l <"$CHECKSUM_ONE")" -eq 1
awk -v archive="$ARCHIVE_NAME" '
  $1 ~ /^[0-9a-f]{64}$/ && $2 == archive { valid++ }
  END { exit valid == 1 ? 0 : 1 }
' "$CHECKSUM_ONE"
(
  cd -- "$DIST_ONE"
  sha256sum --check --strict SHA256SUMS-aarch64
)

mkdir -p -- "$EXTRACT_ROOT"
tar -xzf "$ARCHIVE_ONE" -C "$EXTRACT_ROOT"

test "$(tar -tzf "$ARCHIVE_ONE" | sed -n '1p')" = "$BUNDLE_NAME/"
test "$(tar -tzf "$ARCHIVE_ONE" | wc -l)" -eq 25
test -x "$BUNDLE_ROOT/bin/notm"
test "$(stat -c '%a' "$BUNDLE_ROOT/BUILD-INFO.txt")" = 644
test "$(stat -c '%Y' "$BUNDLE_ROOT/bin/notm")" = "$SOURCE_DATE_EPOCH"
test "$("$BUNDLE_ROOT/bin/notm")" = "notm $VERSION"

for relative_path in \
  BUILD-INFO.txt \
  CHANGELOG.md \
  INSTALL.md \
  LICENSE \
  bin/notm \
  share/applications/io.github.kris004.notm.desktop \
  share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg \
  share/man/man1/notm.1 \
  share/man/man5/notm-config.5 \
  share/man/man7/notm-automation.7 \
  share/man/man7/notm-test-harness.7 \
  share/metainfo/io.github.kris004.notm.metainfo.xml; do
  test -f "$BUNDLE_ROOT/$relative_path"
done

cmp "$FAKE_BINARY" "$BUNDLE_ROOT/bin/notm"
cmp "$BUILD_INFO" "$BUNDLE_ROOT/BUILD-INFO.txt"
cmp "$PROJECT_ROOT/LICENSE" "$BUNDLE_ROOT/LICENSE"
cmp "$PROJECT_ROOT/CHANGELOG.md" "$BUNDLE_ROOT/CHANGELOG.md"
cmp "$PROJECT_ROOT/packaging/io.github.kris004.notm.desktop" \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
cmp "$PROJECT_ROOT/packaging/io.github.kris004.notm.metainfo.xml" \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"
cmp \
  "$PROJECT_ROOT/packaging/icons/hicolor/scalable/apps/io.github.kris004.notm.svg" \
  "$BUNDLE_ROOT/share/icons/hicolor/scalable/apps/io.github.kris004.notm.svg"
cmp "$PROJECT_ROOT/docs/man/notm.1" "$BUNDLE_ROOT/share/man/man1/notm.1"
cmp "$PROJECT_ROOT/docs/man/notm-config.5" \
  "$BUNDLE_ROOT/share/man/man5/notm-config.5"
cmp "$PROJECT_ROOT/docs/man/notm-automation.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-automation.7"
cmp "$PROJECT_ROOT/docs/man/notm-test-harness.7" \
  "$BUNDLE_ROOT/share/man/man7/notm-test-harness.7"

grep -Fx "Target: $TARGET" "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx 'Build architecture: aarch64' "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx 'Build environment: Ubuntu 24.04 ARM64 userspace' \
  "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -Fx "Release binary SHA-256: $FAKE_BINARY_SHA256" \
  "$BUNDLE_ROOT/BUILD-INFO.txt" >/dev/null
grep -F 'AArch64 GNU/Linux build' "$BUNDLE_ROOT/INSTALL.md" >/dev/null
grep -F 'To uninstall only the files installed by this bundle:' \
  "$BUNDLE_ROOT/INSTALL.md" >/dev/null
desktop-file-validate \
  "$BUNDLE_ROOT/share/applications/io.github.kris004.notm.desktop"
appstreamcli validate --strict --pedantic --no-net \
  "$BUNDLE_ROOT/share/metainfo/io.github.kris004.notm.metainfo.xml"

X86_64_BUILD_INFO="$WORK_ROOT/BUILD-INFO-x86_64.txt"
readonly X86_64_BUILD_INFO
cat >"$X86_64_BUILD_INFO" <<EOF
Version: $VERSION
Source tag: v$VERSION
Source commit: $SOURCE_COMMIT
Target: x86_64-unknown-linux-gnu
Build image: release-aggregation-smoke
EOF
env \
  SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  SOURCE_REF="$SOURCE_COMMIT" \
  "$PROJECT_ROOT/packaging/build-linux-release.sh" \
    "$PROJECT_ROOT" \
    "$VERSION" \
    x86_64-unknown-linux-gnu \
    "$FAKE_BINARY" \
    "$X86_64_BUILD_INFO" \
    "$X86_64_DIST" >/dev/null

ARM64_SHA256=$(sha256sum "$ARCHIVE_ONE" | awk '{print $1}')
readonly ARM64_SHA256
"$PROJECT_ROOT/packaging/arm64/aggregate-release.sh" \
  "$X86_64_DIST" \
  "$DIST_ONE" \
  "$AGGREGATE_DIST" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$ARM64_SHA256" >/dev/null
test "$(find "$AGGREGATE_DIST" -mindepth 1 -maxdepth 1 | wc -l)" -eq 4
test "$(wc -l <"$AGGREGATE_DIST/SHA256SUMS")" -eq 3
for archive_name in \
  "notm-v${VERSION}-x86_64-unknown-linux-gnu.tar.gz" \
  "$ARCHIVE_NAME" \
  "notm-v${VERSION}-src.tar.gz"; do
  test -f "$AGGREGATE_DIST/$archive_name"
done
(
  cd -- "$AGGREGATE_DIST"
  sha256sum --check --strict SHA256SUMS >/dev/null
)

if "$PROJECT_ROOT/packaging/arm64/aggregate-release.sh" \
  "$X86_64_DIST" \
  "$DIST_ONE" \
  "$WORK_ROOT/dist-wrong-digest" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  0000000000000000000000000000000000000000000000000000000000000000 \
  >"$WORK_ROOT/wrong-digest.out" 2>&1; then
  printf '%s\n' 'release aggregator accepted an incorrect ARM64 digest' >&2
  exit 1
fi
grep -F 'ARM64 workflow digest mismatch:' \
  "$WORK_ROOT/wrong-digest.out" >/dev/null

cp -a -- "$DIST_ONE" "$BAD_ARM64_DIST"
: >"$BAD_ARM64_DIST/unexpected"
if "$PROJECT_ROOT/packaging/arm64/aggregate-release.sh" \
  "$X86_64_DIST" \
  "$BAD_ARM64_DIST" \
  "$WORK_ROOT/dist-extra-input" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$ARM64_SHA256" >"$WORK_ROOT/extra-input.out" 2>&1; then
  printf '%s\n' 'release aggregator accepted an extra ARM64 artifact' >&2
  exit 1
fi
grep -F 'release fragment has 3 entries instead of 2:' \
  "$WORK_ROOT/extra-input.out" >/dev/null

if "$PROJECT_ROOT/packaging/arm64/build-release.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$FAKE_BINARY" \
  "$BUILD_INFO" \
  "$DIST_ONE" >"$WORK_ROOT/overwrite.out" 2>&1; then
  printf '%s\n' 'ARM64 builder unexpectedly overwrote an artifact' >&2
  exit 1
fi
grep -F 'refusing to overwrite release artifact:' \
  "$WORK_ROOT/overwrite.out" >/dev/null

sed 's/^Target:.*/Target: x86_64-unknown-linux-gnu/' \
  "$BUILD_INFO" >"$BAD_BUILD_INFO"
if "$PROJECT_ROOT/packaging/arm64/build-release.sh" \
  "$PROJECT_ROOT" \
  "$VERSION" \
  "$SOURCE_COMMIT" \
  "$FAKE_BINARY" \
  "$BAD_BUILD_INFO" \
  "$WORK_ROOT/dist-bad" >"$WORK_ROOT/bad.out" 2>&1; then
  printf '%s\n' 'ARM64 builder accepted mismatched build metadata' >&2
  exit 1
fi
grep -F "build information must contain exactly once: Target: $TARGET" \
  "$WORK_ROOT/bad.out" >/dev/null

printf '%s\n' 'ARM64 release builder smoke passed'
