// The chips this firmware can be built for, and the one thing about a chip that cannot be a Cargo
// feature.
//
// ## Why a table and not a feature
//
// The chip is a Cargo feature — `esp32c3` and `esp32c6` in `[features]` in `Cargo.toml`, each naming
// the same six direct dependencies — and everything that a feature can reach is reached through it.
// The reserved pin list in `src/bin/main.rs` is `#[cfg]`'d on it, so one branch builds for both.
//
// One thing cannot be reached: `[build] target` in `.cargo/config.toml`, which is what makes a bare
// `cargo build` the firmware rather than a host binary. Cargo has no way to read configuration out of
// a feature, so the triple is a second thing that has to be said, and saying it in the wrong place or
// with the wrong chip is a build error rather than a wrong answer: `esp-metadata` refuses a
// triple its features do not match, with "Seems you are building for an unsupported or wrong
// target". Measured, on both chips.
//
// So the pairing lives here, once, and the gate reads it rather than repeating it — the same
// reasoning as `host.mjs`, which refuses to write down the host triple for the same reason. What is
// *not* here is anything a feature already carries: the chip's dependencies and its pin list are in
// the manifest, and adding a chip to this table without adding the feature builds nothing.
//
// ## What is deliberately absent
//
// `esp-config` is not asked how wide an atomic is, even though the `imc`/`imac` difference in the two
// triples is exactly that and it is tempting to put a `portable-atomic` answer here. The firmware
// already encodes the consequence by not naming a wider atomic than a word, and `esp-hal` enables
// `portable-atomic/unsafe-assume-single-core` from the chip feature on its own. A table that carried
// a third answer to "which chip is this" would be one more thing to keep in step with the other two
// for no gate to catch it going stale.

import fs from "node:fs";
import path from "node:path";

/**
 * The chips this repository builds firmware for.
 *
 * Ordered so that `DEFAULT` is first, because everything that reads this without asking wants the
 * chip a bare `cargo build` produces.
 *
 * @type {{feature: string, chip: string, triple: string, board: string}[]}
 */
export const CHIPS = [
  {
    // The default. `.cargo/config.toml` names this triple in `[build] target` and `Cargo.toml` names
    // this feature in `default`; those two are the only places the pairing is allowed to be restated,
    // and `defaultFeatureIsConfigured` below is what holds them to it.
    feature: "esp32c3",
    chip: "esp32c3",
    triple: "riscv32imc-unknown-none-elf",
    board: "esp32-c3-devkitm-1",
  },
  {
    feature: "esp32c6",
    chip: "esp32c6",
    triple: "riscv32imac-unknown-none-elf",
    board: "esp32-c6-devkitc-1",
  },
];

/**
 * The chip a bare `cargo build` produces.
 *
 * Read by index rather than by a name spelled out here, because "the first one" and the name of the
 * first one cannot drift apart.
 *
 * @returns {{feature: string, chip: string, triple: string, board: string}} The default chip.
 */
export function defaultChip() {
  return CHIPS[0];
}

/**
 * Builds the arguments that select one chip, as a step needs them.
 *
 * `--target` is always passed, and `--no-default-features` with it. Passing the default chip's
 * feature explicitly is what keeps this correct if the features are ever made non-additive, and
 * `--target` on its own would otherwise build the chip the feature names for the triple
 * `.cargo/config.toml` happens to default to — which `esp-metadata` rejects, loudly, having said so.
 *
 * @param {{feature: string}} chip The chip to select.
 * @returns {string[]} The arguments to append to a `cargo` invocation.
 */
export function chipArgs(chip) {
  return ["--no-default-features", "--features", chip.feature, "--target", chip.triple];
}

/**
 * Checks the two places that are allowed to name the default chip outside this table.
 *
 * Neither can read this module — one is a Cargo manifest and one is a Cargo configuration file, and
 * Cargo offers no way to ask either of them what a feature or a key holds. So this parses both, and
 * it is the reason a step in the gate exists that runs before anything builds. Without it, changing
 * the first entry of `CHIPS` and nothing else turns the gate red with a message about `target_has_atomic`
 * or about a missing field in `Peripherals`, neither of which says what was wrong.
 *
 * @param {string} root Workspace root.
 * @returns {string[]} What is out of step, empty when nothing is.
 */
export function defaultFeatureIsConfigured(root) {
  const problems = [];

  const manifest = read(path.join(root, "Cargo.toml"));
  const configured = /^default\s*=\s*\[([^\]]*)\]/mu.exec(manifest);

  if (configured === null) {
    problems.push("Cargo.toml has no `default = [...]`, so no chip is selected by a bare build");
  } else {
    // Exactly one entry, and it is this chip. "Contains the chip" would pass for
    // `default = ["esp32c3", "esp32c6"]`, which is the one case here that cannot build at all.
    const selected = configured[1]
      .split(",")
      .map((entry) => entry.trim().replace(/^"|"$/gu, ""))
      .filter((entry) => entry !== "");

    if (selected.length !== 1 || selected[0] !== defaultChip().feature) {
      problems.push(
        `Cargo.toml selects \`default = [${selected.join(", ")}]\`, ` +
          `which is not this table's first chip alone: \`${defaultChip().feature}\``,
      );
    }
  }

  const config = read(path.join(root, ".cargo", "config.toml"));
  const target = /^target\s*=\s*"([^"]+)"/mu.exec(config);

  if (target === null) {
    problems.push(
      '.cargo/config.toml has no `target = "..."`, so a bare `cargo build` is the host',
    );
  } else if (target[1] !== defaultChip().triple) {
    problems.push(
      `.cargo/config.toml sets \`target = "${target[1]}"\`, ` +
        `which is not this table's first chip \`${defaultChip().triple}\``,
    );
  }

  return problems;
}

/**
 * Reads a file, reporting rather than throwing when it is not there.
 *
 * A step that reads two files to check they agree should report a missing one as a disagreement, not
 * as a stack trace: the answer is the same either way and the wording is what the operator reads.
 *
 * @param {string} file Absolute path to the file.
 * @returns {string} Its contents, or the empty string.
 */
function read(file) {
  try {
    return fs.readFileSync(file, "utf8");
  } catch {
    return "";
  }
}
