#!/usr/bin/env python3
"""Make the RC.6 sequencer resolve the bundled sierra-compile relative to the
running binary, so a relocated strkd bundle finds `shared_executables` next to
the runner.

RC.6's `out_dir()` returns the build-time-baked `RUNTIME_ACCESSIBLE_OUT_DIR` (an
absolute path frozen via the compile-time `env!()` macro), which points at the
build machine's path and does not exist on a user's machine — so a relocated
bundle fails `Failed to compile Sierra to Casm: No such file or directory`.

This backports the relocatability of the newer `ac43943` sequencer WITHOUT its
larger refactor. `apollo_compilation_utils::paths::shared_folder_dir` computes
`out_dir.ancestors().nth(3)/shared_executables`, and the staged bundle keeps the
sierra-compile binary in that dir next to the runner. So returning a path three
levels below `current_exe()`'s directory makes `nth(3)` land on the runner's
directory and resolve the binary as its sibling. Falls back to the baked path for
in-tree (non-relocated) runs.

Idempotent. Usage: patch-sequencer-relocatable.py <sequencer_dir>
"""
import sys
import pathlib

OLD = '''fn out_dir() -> PathBuf {
    env!("RUNTIME_ACCESSIBLE_OUT_DIR").into()
}'''

NEW = '''fn out_dir() -> PathBuf {
    // strkd relocatable-bundle patch: resolve shared_executables relative to the
    // running binary. shared_folder_dir() = out_dir.ancestors().nth(3)/shared_executables,
    // and the binary is staged in that dir next to the runner, so return a path
    // three levels below the executable's directory. Fall back to the baked build
    // path for in-tree (non-relocated) runs.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("build").join("relocated").join("out");
        }
    }
    env!("RUNTIME_ACCESSIBLE_OUT_DIR").into()
}'''


def main() -> None:
    if len(sys.argv) != 2:
        sys.exit("usage: patch-sequencer-relocatable.py <sequencer_dir>")
    seq = pathlib.Path(sys.argv[1])
    targets = [
        seq / "crates/apollo_compile_to_casm/src/compiler.rs",
        seq / "crates/apollo_compile_to_native/src/compiler.rs",
    ]
    patched = 0
    for f in targets:
        text = f.read_text()
        if "current_exe()" in text:
            print(f"already patched: {f}")
            continue
        if OLD not in text:
            sys.exit(f"ERROR: out_dir() pattern not found in {f} — sequencer version drift?")
        f.write_text(text.replace(OLD, NEW, 1))
        print(f"patched: {f}")
        patched += 1
    print(f"done ({patched} file(s) patched)")


if __name__ == "__main__":
    main()
