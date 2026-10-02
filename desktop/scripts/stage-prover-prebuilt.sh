#!/usr/bin/env bash
# Stage the bundled prover for `tauri build` from the prover backend's PREBUILT
# release artifacts — no local snip-36-prover-backend checkout, no 30-40 min
# prover build. CI-friendly sibling of stage-prover.sh (which stages from a
# locally-built checkout); both produce the same resources/prover layout that
# desktop/src-tauri/src/lib.rs loads (WORK_DIR = resources/prover, BIN = its
# snip36; snip36 resolves deps/ relative to its cwd).
#
#   ./scripts/stage-prover-prebuilt.sh
#
# Platform is auto-detected from the host: darwin-arm64 or linux-x86_64, the
# two the desktop app ships (and the only ones with integrity pins). Run it ON
# the machine whose installers you're building.
#
# Mechanism: install the pinned `snip36` CLI, then `snip36 setup --prebuilt`,
# which downloads the proving stack MATCHED to that snip36 build (deps-v5 for
# v1.2.1, sequencer SHA ac43943…) — no hand-coordinating the sequencer pin, the
# mismatch class behind the historical INVALID_PROOF incident.
#
# Then we trim to a lean, RELOCATABLE bundle (validated 2026-06-16 with
# `snip36 doctor` on an M4 Pro, stack relocated outside any CI workspace):
#   - DROP sequencer_venv/ (~278 MB): the Python cairo-compile venv is for
#     contract authoring, NOT proving (see the prover backend Dockerfile, which
#     ships a working prover image without it), AND its shebang/pyvenv.cfg
#     hardcode the build machine's paths, so it can't be relocated anyway.
#   - ADD sample-input/ (prover-param templates the prove paths read) — fetched
#     from the repo at the pinned tag; not present in the release tarballs.
# Result: ~403 MB, `snip36 doctor` green on every proving component.
#
# Integrity: every downloaded or staged file is checked against hashes pinned
# in prover-pin.env, independent of the release's own SHA256SUMS: the CLI
# tarball before extraction, sample-input/ (fetched at a commit, not a tag), and
# the final shipped binaries. A mismatch fails the build. Nothing is piped to sh.
#
# Requires: curl, tar, sha256sum or shasum, and Python 3.12 on PATH
# (setup --prebuilt builds the venv before we drop it; there is no --skip-venv
# flag).
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"   # desktop/
# shellcheck source=prover-pin.env
source "$HERE/scripts/prover-pin.env"      # SNIP36_REPO, SNIP36_RELEASE, SNIP36_COMMIT, *_SHA256*

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
# expect_sha256 <file> <expected-hex> <label>
expect_sha256() {
  local got; got="$(sha256 "$1")"
  if [ "$got" != "$2" ]; then
    echo "error: $3 failed its integrity pin" >&2
    echo "  expected $2" >&2
    echo "  got      $got" >&2
    exit 1
  fi
}
# check_pins <base-dir> <"path=sha256" lines> <label>
check_pins() {
  local base="$1" n=0 line path want
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    path="${line%%=*}"; want="${line#*=}"
    [ -f "$base/$path" ] || { echo "error: pinned $3 file missing: $path" >&2; exit 1; }
    expect_sha256 "$base/$path" "$want" "$3 $path"
    n=$((n + 1))
  done <<< "$2"
  [ "$n" -gt 0 ] || { echo "error: no $3 pins defined" >&2; exit 1; }
  echo "  $n $3 file(s) match their pins"
}

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)                PLATFORM=darwin-arm64 ;;
  Linux-x86_64)                PLATFORM=linux-x86_64 ;;
  *) echo "error: no pinned prover for $(uname -s)-$(uname -m) (pins exist for darwin-arm64, linux-x86_64)" >&2; exit 1 ;;
esac
PKEY="${PLATFORM//-/_}"
CLI_SHA_VAR="SNIP36_CLI_TARBALL_SHA256_${PKEY}"
SHIPPED_VAR="SNIP36_SHIPPED_SHA256_${PKEY}"
CLI_SHA="${!CLI_SHA_VAR:?missing $CLI_SHA_VAR in prover-pin.env}"
SHIPPED_PINS="${!SHIPPED_VAR:?missing $SHIPPED_VAR in prover-pin.env}"

DST="$HERE/src-tauri/resources/prover"
BINDIR="$(mktemp -d)"
trap 'rm -rf "$BINDIR"' EXIT

echo "=== staging prebuilt prover ${SNIP36_RELEASE} from ${SNIP36_REPO} ==="

# 1. Download the pinned snip36 CLI tarball and verify it before extracting.
TARBALL="snip36-${PLATFORM}.tar.gz"
curl -fsSL -o "$BINDIR/$TARBALL" \
  "https://github.com/${SNIP36_REPO}/releases/download/${SNIP36_RELEASE}/${TARBALL}"
expect_sha256 "$BINDIR/$TARBALL" "$CLI_SHA" "$TARBALL"
tar xzf "$BINDIR/$TARBALL" -C "$BINDIR" snip36
[ -x "$BINDIR/snip36" ] || { echo "error: snip36 not found in $TARBALL" >&2; exit 1; }

# Fresh bundle dir, but keep the tracked README that documents it.
KEEP_README="$BINDIR/prover-README.md"
[ -f "$DST/README.md" ] && cp "$DST/README.md" "$KEEP_README"
rm -rf "$DST"
mkdir -p "$DST"
[ -f "$KEEP_README" ] && cp "$KEEP_README" "$DST/README.md"
cp "$BINDIR/snip36" "$DST/snip36"
chmod +x "$DST/snip36"

# 2. Provision the version-matched proving stack into the bundle dir.
( cd "$DST" && ./snip36 setup --prebuilt )

# 3. Prover-param templates the prove paths read (not in the release tarballs).
#    Fetched at the pinned commit and verified: a missing or altered file fails.
mkdir -p "$DST/sample-input"
while IFS= read -r line; do
  [ -z "$line" ] && continue
  f="${line%%=*}"
  curl -fsSL -o "$DST/sample-input/$f" \
    "https://raw.githubusercontent.com/${SNIP36_REPO}/${SNIP36_COMMIT}/sample-input/$f"
done <<< "$SNIP36_SAMPLE_INPUT_SHA256"
check_pins "$DST/sample-input" "$SNIP36_SAMPLE_INPUT_SHA256" "sample-input"

# 4. Gate: the full offline stack check must pass BEFORE we trim. doctor reads
#    deps/ relative to cwd, exactly as strkd's native backend invokes it. It
#    exits non-zero on failure; we also assert the explicit ready line so a
#    future change to its exit behavior can't let a broken stack through.
echo "=== snip36 doctor (offline stack check, full stack) ==="
( cd "$DST" && ./snip36 doctor 2>&1 | tee /tmp/strkd-doctor.log )
grep -q "PROVING STACK READY" /tmp/strkd-doctor.log \
  || { echo "error: prebuilt stack failed snip36 doctor (see above)" >&2; exit 1; }

# 5. Trim to the lean, relocatable shippable set: drop the venv (not used for
#    proving; non-relocatable). After this, doctor's venv check will fail by
#    design — proving does not depend on it.
rm -rf "$DST/sequencer_venv"

# 6. Final sanity: the proving-critical binaries are present + executable.
RUNNER="$DST/deps/sequencer/target/release/starknet_transaction_prover"
PROVER="$DST/deps/bin/stwo-run-and-prove"
[ -x "$RUNNER" ] || { echo "error: runner missing/!x: $RUNNER" >&2; exit 1; }
[ -x "$PROVER" ] || { echo "error: stwo prover missing/!x: $PROVER" >&2; exit 1; }
chmod +x "$DST"/deps/sequencer/target/release/* 2>/dev/null || true
find "$DST/deps/compiler-tools" -type f -name 'starknet-sierra-compile' -exec chmod +x {} + 2>/dev/null || true

# 7. Integrity gate on what actually ships. setup --prebuilt checks the deps
#    tarball against the release's own SHA256SUMS; this checks the extracted
#    files against pins that don't come from the release.
echo "=== integrity pins (${PLATFORM}) ==="
check_pins "$DST" "$SHIPPED_PINS" "shipped"

echo "staged $(du -sh "$DST" | cut -f1) into $DST (venv dropped; sample-input added; pins verified)"
