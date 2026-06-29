# Bundled prover (staged here at build time)

This directory holds the on-device SNIP-36 / stwo prover bundle that the desktop
app ships with. It is **populated by the staging scripts**, not committed — the
bundle is 286–403 MB:

```sh
cd desktop
./scripts/stage-prover.sh            # from a locally-built snip-36-prover-backend checkout
./scripts/stage-prover-source.sh     # build from source + relocatability patch (CI)
./scripts/stage-prover-prebuilt.sh   # from GitHub prebuilt release artifacts
```

After staging, this dir contains `snip36`, `deps/`, and `sample-input/`. At
runtime `src-tauri/src/lib.rs` points `STRKD_SNIP36_BIN` / `STRKD_SNIP36_WORK_DIR`
here so a packaged app proves with no external checkout.

Only this `README.md` is tracked in git (so the `resources/prover/**/*` bundle
glob in `tauri.conf.json` always matches and a dev/CI build works **without** the
prover staged). Everything else here is `.gitignore`d. When the prover is not
staged, the desktop app still runs — the prover reports "not ready" / falls back
to the configured remote-or-mock backend until you set the backend to `native`
with a staged bundle.

See [`../../../../docs/code/prover.md`](../../../../docs/code/prover.md).
