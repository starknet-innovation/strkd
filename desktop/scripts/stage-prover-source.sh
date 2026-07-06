#!/usr/bin/env bash
# Build the verifier-compatible RC.6 prover (v1.1.3, "PROOF0") FROM SOURCE and
# stage a RELOCATABLE bundle into desktop/src-tauri/resources/prover.
#
# WHY from source (not stage-prover-prebuilt.sh):
#   The deployed Sepolia gateway verifier runs the RC.6 stack and accepts only
#   "PROOF0" proofs. The v1.2.x prebuilt path bumped the sequencer ahead of the
#   verifier -> "PROOF1" -> rejected on-chain (node code 69). RC.6 (v1.1.3) emits
#   PROOF0 and verifies, but its PREBUILT binaries bake the build machine's path
#   for sierra-compile and can't be relocated. Building from source lets us apply
#   scripts/patch-sequencer-relocatable.py so the runner resolves sierra-compile
#   relative to itself -> a self-contained PROOF0 bundle that verifies on-chain.
#   (Confirmed 2026-06-16: a v1.1.3/PROOF0 proof verified on Sepolia; v1.2.1/PROOF1
#   was rejected. See docs/project/status.md and memory prover-proof-format-pin.)
#
# HEAVY: full sequencer build, ~30-40 min. Requires: git, rustup (Rust), Python 3.12.
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"   # desktop/
# shellcheck source=prover-pin.env
source "$HERE/scripts/prover-pin.env"      # SNIP36_REPO, SNIP36_RELEASE
DST="$HERE/src-tauri/resources/prover"
SRC="${SNIP36_SRC:-$HOME/.cache/strkd/snip36-src}"
RUNNER_NIGHTLY="nightly-2025-07-14"        # STWO_NIGHTLY for the RC.6 stack

echo "=== build RC.6 prover from source ($SNIP36_RELEASE) -> $SRC ==="
if [ ! -d "$SRC/.git" ]; then
  rm -rf "$SRC"; mkdir -p "$(dirname "$SRC")"
  git clone --depth 1 --branch "$SNIP36_RELEASE" "https://github.com/${SNIP36_REPO}.git" "$SRC"
fi
cd "$SRC"

echo "=== [1/4] build snip36 CLI ==="
cargo build --release -p snip36-cli

echo "=== [2/4] snip36 setup (clones proving-utils + sequencer @ RC.6, venv, builds stwo + runner) ==="
./target/release/snip36 setup

echo "=== [3/4] apply relocatability patch + rebuild the runner ==="
python3 "$HERE/scripts/patch-sequencer-relocatable.py" "$SRC/deps/sequencer"
PATH="$SRC/sequencer_venv/bin:$PATH" cargo "+$RUNNER_NIGHTLY" build --release \
  --manifest-path "$SRC/deps/sequencer/Cargo.toml" \
  -p starknet_transaction_prover --features stwo_proving

echo "=== [4/4] stage lean relocatable bundle -> $DST ==="
REL="$SRC/deps/sequencer/target/release"
rm -rf "$DST"
mkdir -p "$DST/deps/bin" "$DST/deps/sequencer/target/release" "$DST/sample-input"
cp "$SRC/target/release/snip36"            "$DST/snip36"
cp "$SRC/deps/bin/stwo-run-and-prove"      "$DST/deps/bin/"
cp "$SRC/deps/bin/bootloader_program.json" "$DST/deps/bin/"
# Stage the PATCHED runner under BOTH names — config::runner_bin() prefers
# starknet_os_runner; the setup build's own os_runner is unpatched, so overwrite it.
cp "$REL/starknet_transaction_prover" "$DST/deps/sequencer/target/release/starknet_transaction_prover"
cp "$REL/starknet_transaction_prover" "$DST/deps/sequencer/target/release/starknet_os_runner"
cp -R "$REL/shared_executables"       "$DST/deps/sequencer/target/release/shared_executables"
cp -R "$SRC/sample-input/." "$DST/sample-input/" 2>/dev/null || true
chmod +x "$DST/snip36" "$DST/deps/bin/stwo-run-and-prove" \
         "$DST"/deps/sequencer/target/release/starknet_* \
         "$DST"/deps/sequencer/target/release/shared_executables/* 2>/dev/null || true

# Sanity: patched runner + sierra-compile present, patch actually applied.
[ -x "$DST/deps/sequencer/target/release/starknet_os_runner" ] || { echo "error: runner missing" >&2; exit 1; }
[ -x "$DST/deps/sequencer/target/release/shared_executables/starknet-sierra-compile" ] || { echo "error: sierra-compile missing" >&2; exit 1; }
grep -q "current_exe" "$SRC/deps/sequencer/crates/apollo_compile_to_casm/src/compiler.rs" || { echo "error: patch not applied" >&2; exit 1; }
echo "staged $(du -sh "$DST"|cut -f1) — from-source RC.6/PROOF0, relocatable patch applied"
