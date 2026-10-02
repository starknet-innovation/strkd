#!/usr/bin/env bash
# Install cargo-dist from its release tarball, verified against a pinned
# SHA-256, instead of piping dist's installer script into `sh`.
#
# The pins were computed 2026-10-02 from the v0.32.0 release assets and matched
# the release's own .sha256 files. To bump: change DIST_VERSION (and
# `cargo-dist-version` in the root Cargo.toml), download the four tarballs,
# check each against its published .sha256, and replace the hashes below.
set -euo pipefail

DIST_VERSION=0.32.0

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)               TRIPLE=x86_64-unknown-linux-gnu;  SHA=eb52f9fae0d0506774e9f1801c1168f87fa2c87a45e2d64d3ae7c89401929946 ;;
  Linux-aarch64|Linux-arm64)  TRIPLE=aarch64-unknown-linux-gnu; SHA=d29bcffeb3f8b0c517b4ce0dd2470926ed5cb0bb29d78c6bdd5f88d76ee14a6a ;;
  Darwin-arm64)               TRIPLE=aarch64-apple-darwin;      SHA=aa343b2ff78ec2981f17a65140250c5ad6062c74072163f68c5c2686d94763a7 ;;
  Darwin-x86_64)              TRIPLE=x86_64-apple-darwin;       SHA=6243464a8389e006b9256ee548bc795638f1a17113c1b6669c0e05ce89fd05c5 ;;
  *) echo "error: no pinned cargo-dist for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
TARBALL="cargo-dist-${TRIPLE}.tar.xz"
curl --proto '=https' --tlsv1.2 -fsSL -o "$TMP/$TARBALL" \
  "https://github.com/axodotdev/cargo-dist/releases/download/v${DIST_VERSION}/${TARBALL}"

if command -v sha256sum >/dev/null 2>&1; then GOT="$(sha256sum "$TMP/$TARBALL" | cut -d' ' -f1)"
else GOT="$(shasum -a 256 "$TMP/$TARBALL" | cut -d' ' -f1)"; fi
if [ "$GOT" != "$SHA" ]; then
  echo "error: $TARBALL failed its integrity pin (expected $SHA, got $GOT)" >&2
  exit 1
fi

tar xJf "$TMP/$TARBALL" -C "$TMP"
mkdir -p "$HOME/.cargo/bin"
install -m 0755 "$TMP/cargo-dist-${TRIPLE}/dist" "$HOME/.cargo/bin/dist"
[ -n "${GITHUB_PATH:-}" ] && echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
"$HOME/.cargo/bin/dist" --version
