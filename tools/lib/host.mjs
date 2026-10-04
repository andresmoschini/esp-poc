// The target triple the host-side tests are built for, and why the gate cannot simply write one down.
//
// `.cargo/config.toml` sets `[build] target` so that a bare `cargo build` is the firmware rather
// than a host binary, and that setting applies to every crate in the tree. A `cargo test` without an
// explicit `--target` therefore builds the test harness for `riscv32imac-unknown-none-elf` — which
// compiles, and then cannot run anywhere. The escape is `--target`, and the trap is hard-coding the
// triple: the answer is a property of the machine, so a gate that spells one out is green on the
// machine it was written on and broken on every other one, CI included.
//
// `rustc -vV` prints the triple this toolchain was built for, which is by definition the one that
// can run what it compiles.

import { capture } from "./process.mjs";

/**
 * The triple the toolchain was built for.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<string>} The triple, for example `x86_64-pc-windows-msvc`.
 */
export function hostTriple(root) {
  return capture(root, "rustc", ["-vV"]).then(parseHostTriple);
}

/**
 * Reads the host triple out of what `rustc -vV` prints.
 *
 * The parsing is separate from the running so that it can be tested against the output rather than
 * against whatever toolchain happens to be installed. `rustc -vV` prints one `key: value` per line,
 * and `host` is the only one this reads.
 *
 * @param {string} versionOutput What `rustc -vV` printed.
 * @returns {string} The triple.
 */
export function parseHostTriple(versionOutput) {
  for (const line of versionOutput.split("\n")) {
    const found = /^host:\s*(\S+)\s*$/u.exec(line);

    if (found !== null) {
      return found[1];
    }
  }

  throw new Error("`rustc -vV` printed no `host:` line, so the host triple is unknown");
}
