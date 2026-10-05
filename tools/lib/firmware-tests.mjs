// Running the firmware's own tests, which is a step rather than a line in the `GATE` array because
// what it runs is not known until it runs, and not from where it runs.
//
// The firmware is built for `riscv32imc-unknown-none-elf` or `riscv32imac-unknown-none-elf`, a
// microcontroller with no process to run a test binary in, so its tests cannot be run the way the
// orchestrator's are. What can be tested is the part of the firmware that does not talk to hardware,
// and that part is a crate of its own: it has no dependencies and is `#![no_std]`, so it builds for
// the host as well as for the chip.
//
// ## Why Cargo is run from outside the repository
//
// Two settings in the repository's `.cargo/config.toml` are right for the firmware and fatal for a host
// test, and neither can be overridden from the command line:
//
// - `[build] target` sends `cargo test` to the microcontroller, where it compiles and then cannot run.
//   The escape is an explicit `--target`, and it has to be a triple: it is a property of the machine,
//   so one written down here is green on the machine it was written on and broken on CI.
// - `[unstable] build-std = ["core", "alloc"]` rebuilds `core` from source, which is what lets the
//   firmware have an `alloc` on a target with no `std`. A host test links the toolchain's own `std`,
//   and two definitions of the same language items in one binary is `error[E0152]: duplicate lang item
//   in crate core` — measured, every time.
//
// Neither is negotiable on the command line: Cargo *merges* configuration arrays rather than replacing
// them, so `--config 'unstable.build-std=[]'` adds nothing to the two units already there, and a
// `.cargo/config.toml` closer to the manifest is concatenated with the root's rather than winning.
// Building `std` from source as well does work, and costs 61 s cold against 6.6 s — measured, same
// machine — which is a minute added to every CI run for a crate whose tests take microseconds.
//
// What Cargo does read is the configuration from the *current directory* upward, never from
// `--manifest-path`. Run from a fresh empty directory, the only configuration in effect is the one that
// comes with the toolchain, and the two settings above are simply not in play. The manifest is named
// with `--manifest-path`, and artifacts are kept out of the chip's target directory with `--target-dir`
// so a host `core` can never be mistaken for the firmware's.
//
// If that assumption ever fails, it fails loudly: a missing `std` or a duplicated `core` is an error,
// not a test that quietly passes.

import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { hostTriple } from "./host.mjs";
import { runVisible } from "./process.mjs";

/** The crate holding the firmware logic that does not need a chip. */
const CRATE = "crates/poc-report";

/**
 * Builds the firmware's host-side tests and runs them.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<boolean>} Whether every test passed.
 */
export async function firmwareTests(root) {
  let triple;
  try {
    triple = await hostTriple(root);
  } catch (error) {
    process.stderr.write(`gate: ${error.message}\n`);
    return false;
  }

  console.log(`--- target ${triple} ---`);

  // Empty, and removed afterwards: Cargo only needs somewhere with no configuration above it, and it
  // writes nothing here because the target directory is named explicitly.
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), "esp-poc-tests-"));

  try {
    return await runVisible(
      root,
      "cargo",
      [
        "test",
        "--manifest-path",
        path.join(root, CRATE, "Cargo.toml"),
        "--target",
        triple,
        "--target-dir",
        path.join(root, "target", "host"),
      ],
      outside,
    );
  } finally {
    fs.rmSync(outside, { recursive: true, force: true });
  }
}
