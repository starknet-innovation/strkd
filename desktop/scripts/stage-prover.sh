#!/usr/bin/env bash
# Stage a lean prover bundle into src-tauri/resources/prover/ for `tauri build`.
#
# Copies only the runtime artifacts `snip36 prove virtual-os` needs (~286 MB:
# the CLI, the sequencer runner + sierra compiler, and the crates' resource
# JSONs) — NOT the ~9.6 GB of build intermediates. Verified: strkd's native
# backend proves against exactly this set.
#
# Run before `npm run tauri build`. The staged dir is gitignored.
#
#   SNIP36_REPO=/path/to/snip-36-prover-backend ./scripts/stage-prover.sh
#
# NOTE: the staged binaries are for THIS machine's architecture. For a
# distributable build, stage from a checkout built for the target platform, and
# keep the prover stack current (the sequencer side-clone must match the repo's
# pin — see docs/reference/native-prover.md).
set -euo pipefail

SRC="${SNIP36_REPO:-$HOME/Workshop/snip-36-prover-backend}"
DST="$(cd "$(dirname "$0")/.." && pwd)/src-tauri/resources/prover"

[ -x "$SRC/target/release/snip36" ] || {
  echo "error: snip36 not built at $SRC/target/release/snip36 (set SNIP36_REPO)"; exit 1; }

rm -rf "$DST"
mkdir -p "$DST/deps/sequencer/target/release"

cp "$SRC/target/release/snip36" "$DST/snip36"
cp "$SRC/deps/sequencer/target/release/starknet_os_runner" "$DST/deps/sequencer/target/release/"
cp "$SRC/deps/sequencer/target/release/starknet_transaction_prover" "$DST/deps/sequencer/target/release/"
cp -R "$SRC/deps/sequencer/target/release/shared_executables" \
      "$DST/deps/sequencer/target/release/" 2>/dev/null || true

# Resource JSONs the runner reads (relative to deps/sequencer) — not the source.
( cd "$SRC/deps/sequencer" && find crates -path '*/resources/*' -type f -print0 \
    | while IFS= read -r -d '' f; do
        mkdir -p "$DST/deps/sequencer/$(dirname "$f")"
        cp "$f" "$DST/deps/sequencer/$f"
      done )

mkdir -p "$DST/sample-input"
cp -R "$SRC/sample-input/." "$DST/sample-input/" 2>/dev/null || true

chmod +x "$DST/snip36" \
         "$DST/deps/sequencer/target/release/starknet_os_runner" \
         "$DST/deps/sequencer/target/release/starknet_transaction_prover" 2>/dev/null || true

echo "staged $(du -sh "$DST" | cut -f1) into $DST"
