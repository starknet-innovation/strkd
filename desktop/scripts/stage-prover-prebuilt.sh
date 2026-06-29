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
# Platform is auto-detected from the host (darwin-arm64 / linux-x86_64 /
# linux-arm64 — the only platforms the prover backend publishes). Run it ON the
# machine whose installers you're building.
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
# Requires: curl, tar, and Python 3.12 on PATH (setup --prebuilt builds the venv
# before we drop it; there is no --skip-venv flag).
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"   # desktop/
# shellcheck source=prover-pin.env
source "$HERE/scripts/prover-pin.env"      # SNIP36_REPO, SNIP36_RELEASE

DST="$HERE/src-tauri/resources/prover"
BINDIR="$(mktemp -d)"
trap 'rm -rf "$BINDIR"' EXIT

echo "=== staging prebuilt prover ${SNIP36_RELEASE} from ${SNIP36_REPO} ==="

# 1. Install the pinned snip36 CLI (auto-detects platform, verifies SHA256SUMS).
curl -fsSL "https://github.com/${SNIP36_REPO}/releases/download/${SNIP36_RELEASE}/install.sh" \
  | SNIP36_INSTALL_DIR="$BINDIR" sh -s -- "$SNIP36_RELEASE"
[ -x "$BINDIR/snip36" ] || { echo "error: snip36 not installed to $BINDIR" >&2; exit 1; }

rm -rf "$DST"
mkdir -p "$DST"
cp "$BINDIR/snip36" "$DST/snip36"
chmod +x "$DST/snip36"

# 2. Provision the version-matched proving stack into the bundle dir.
( cd "$DST" && ./snip36 setup --prebuilt )

# 3. Prover-param templates the prove paths read (not in the release tarballs).
mkdir -p "$DST/sample-input"
for f in prover_params.json bootloader_input_template.json README.md; do
  curl -fsSL -o "$DST/sample-input/$f" \
    "https://raw.githubusercontent.com/${SNIP36_REPO}/${SNIP36_RELEASE}/sample-input/$f" || true
done

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

echo "staged $(du -sh "$DST" | cut -f1) into $DST (venv dropped; sample-input added)"
