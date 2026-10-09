// Repository automation for esp-poc.
//
// This is the single entry point for the quality gate. CI invokes `npm run check` and nothing else,
// and whoever is about to commit should invoke it too, so there is exactly one definition of what
// "green" means and no way for two answers to drift apart.
//
// ## Why this is JavaScript and not `cargo xtask`
//
// The obvious shape for a Rust repository is a host-side `xtask` crate with a `cargo xtask` alias,
// and that is what the gate started as. It cannot work here, and the reason is in
// `.cargo/config.toml`: esp-generate sets `[build] target = "riscv32imc-unknown-none-elf"` (or
// `riscv32imc-unknown-none-elf`) so that a bare `cargo build` or `cargo run` targets the chip.
// `[build] target` applies to every crate in the tree, so a `xtask` crate is built for a RISC-V
// microcontroller too — and fails, because there is no `std` there and no process to spawn.
// Measured, and the answer was `error[E0463]: can't find crate for std`.
//
// The two escapes both cost more than the crate saves. A per-crate `[target]` table does not exist,
// and Cargo discovers configuration from the current directory upward rather than from
// `--manifest-path`, so a `xtask/.cargo/config.toml` cannot un-set the root's. Passing an explicit
// `--target` for the host works, but the alias would have to hard-code one triple, which breaks on
// every machine that is not the one it was written on — including CI.
//
// Node is already a hard prerequisite of this gate: prettier, markdownlint, cspell,
// editorconfig-checker and commitlint all come from npm. Running the orchestrator on the same
// runtime costs no new requirement, needs no host triple, and runs every `cargo` step as a
// subprocess, which is what makes each one pick up `[build] target` and compile for the chip without
// the gate having to say so anywhere.
//
// ## Design notes
//
// **No dependencies.** Orchestrating a list of subprocesses and propagating their exit codes is what
// a standard library is for, and a tool guarding the project's dependency policy should not be the
// first thing to bend it. Everything here is `node:` built-ins.

import fs from "node:fs";
import path from "node:path";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";

import { toolEntryPoint } from "./lib/npm-tool.mjs";
import { check as eolCheck, fix as eolFix } from "./lib/eol.mjs";
import { isEntryPoint } from "./lib/entry-point.mjs";
import { CHIPS, chipArgs, defaultChip, defaultFeatureIsConfigured } from "./lib/chip.mjs";
import { firmwareTests } from "./lib/firmware-tests.mjs";
import { check as hookCheck, fix as hookFix } from "./lib/hooks.mjs";
import { installedLockfile, nodeToolingState } from "./lib/tooling.mjs";

// The npm executable.
//
// On Windows it has to be named with its extension: npm ships as a shell script plus `.cmd` and
// `.ps1` shims, and a bare `npm` is simply not found. Unlike the gate's own tools this one is not
// a Node program in this tree, so it cannot be resolved out of `node_modules` and is reached the way
// a shell reaches it.
const NPM = process.platform === "win32" ? "npm.cmd" : "npm";

/**
 * An external program resolved through `PATH`, run from the workspace root.
 *
 * @param {string} program Executable name.
 * @param {string[]} args Arguments passed to it.
 * @returns {{kind: "spawn", program: string, args: string[]}} The step's action.
 */
const spawnStep = (program, args) => ({ kind: "spawn", program, args });

/**
 * An npm-installed tool, run as the JavaScript entry point its own package names.
 *
 * @param {string} packageName npm package, which is also the command name.
 * @param {string[]} args Arguments passed to it.
 * @returns {{kind: "tool", packageName: string, args: string[]}} The step's action.
 */
const toolStep = (packageName, args) => ({ kind: "tool", packageName, args });

/**
 * A function of this repository's own code, given the workspace root.
 *
 * It may answer with a promise, because one of these steps has to work out what to run before it runs
 * it: the firmware tests are named after a target triple that is a property of the machine rather
 * than of this file.
 *
 * @param {(root: string) => boolean | Promise<boolean>} fn What running this step does.
 * @returns {{kind: "here", fn: (root: string) => boolean | Promise<boolean>}} The step's action.
 */
const hereStep = (fn) => ({ kind: "here", fn });

/**
 * A step that is one step per chip, for the gate to run as if it were one.
 *
 * A step's action is a value rather than a command line, so a step can be several commands. This keeps
 * that to a shape the rest of this file already understands: the gate prints one heading per step and
 * collects one pass/fail, and a loop inside a step would have to reproduce both. What the operator
 * sees is one `--- build ---` heading over one chip's output after the other's, which is the same thing
 * they saw when there was one chip and one heading.
 *
 * @param {(chip: object) => object} one The action for a single chip.
 * @returns {{kind: "each", one: (chip: object) => object}} The step's action.
 */
const forEachChip = (one) => ({ kind: "each", one });

/**
 * Reports whether the default chip in `Cargo.toml` and in `.cargo/config.toml` are the same chip.
 *
 * @param {string} root Workspace root.
 * @returns {boolean} Whether they agree.
 */
function chipsAgree(root) {
  const problems = defaultFeatureIsConfigured(root);

  if (problems.length === 0) {
    return true;
  }

  process.stderr.write(
    `\nThe default chip and the default target are not the same chip.\n` +
      `${problems.map((problem) => `  - ${problem}\n`).join("")}\n` +
      `  One is \`default\` in Cargo.toml and the other is \`target\` in .cargo/config.toml.\n` +
      `  They name one chip in tools/lib/chip.mjs, whose first entry is the default for both.\n\n`,
  );

  return false;
}

/**
 * Reports whether Git would accept the tree as it stands.
 *
 * Two questions, asked here rather than as two steps of their own because they are one answer: both
 * are about what Git thinks of this working tree, and a commit is refused or silently skips work
 * because of the tree rather than because of a file. `eol` reads `.gitattributes` matched by
 * attribute, where the party that has to agree is `git add`; `hooks` asks Git directly whether it
 * will run the hooks, which is the only way to know on a filesystem that has no executable bit.
 *
 * Both run even when the first fails, so one pass names both problems — the property every step of
 * `GATE` has, applied within this one.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<boolean>} Whether Git would accept the tree.
 */
async function gitStateIsClean(root) {
  const endings = await eolCheck(root);
  const hooks = await hookCheck(root);

  return endings && hooks;
}

/**
 * Repairs what Git would refuse at the commit, in that order.
 *
 * Line endings first because it writes files, and the executable bit second because it writes the
 * index — which is what `git add` and `git commit` read last, so a mode recorded against bytes that
 * are about to change is a mode recorded against the wrong content.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<boolean>} Whether there was nothing left to repair.
 */
async function gitStateIsRepairable(root) {
  const endings = await eolFix(root);
  const hooks = await hookFix(root);

  return endings && hooks;
}

// Every step of the quality gate, in the order they run.
//
// Steps are added here as the gate grows. Order is presentation only: all of them run on every
// invocation, so that one pass reports every problem rather than only the first.
//
// A step passes or fails on its exit code alone. This is a deliberate limit rather than an
// oversight, and it has known holes: `rustfmt` reports `can't set group_imports, unstable features
// are only available in nightly channel` and still exits 0, and Cargo reports a missing resolver the
// same way. In both cases the tool knows something is wrong, says so, and the gate does not notice.
// Closing that would mean matching on the text the tools print, which is brittle in a different and
// less obvious way.
//
// The three `cargo` steps that compile the firmware are spelled as loops over `CHIPS` rather than as
// one step each. The chip is a Cargo feature, so a feature the gate does not build is a feature
// nothing checks, and the firmware would stop compiling for the other chip at some commit whose gate
// was green. `tools/lib/chip.mjs` is where the chip/triple pairing lives; the reason it cannot just be
// `[build] target` is in the header of that file and in `.cargo/config.toml`.
export const GATE = [
  {
    name: "chips",
    // Checks the one thing no tool can: that the chip `Cargo.toml` selects by default is the chip
    // `.cargo/config.toml` builds by default. Nothing in Cargo compares a feature with a configuration
    // value, so without this step the failure of changing one without the other is `esp-metadata`
    // saying the target is wrong, or a missing `Peripherals` field — neither of which names the file to
    // open. It is first because it is the only step that can say "this repository is set up for a chip
    // it has no features for", which makes every step after it pointless.
    action: hereStep(chipsAgree),
  },
  {
    name: "git-state",
    // What Git thinks of this working tree, asked of Git rather than of the files, and the one step
    // that reads the index.
    //
    // Two things live here and they are one concern: `git add` refuses a file whose endings
    // `.gitattributes` does not ask for, and it will not run a hook whose index mode is not `100755`.
    // Both are refusals at the commit, so both are asked before anything is built — and Windows is
    // where the second is produced routinely, since `git add` records `100644` there because there is
    // no executable bit to read off the filesystem, while the working-tree copy still looks
    // executable. Only Git's answer is one Git acts on.
    //
    // It sits second only to `chips`, which can report a repository set up for a chip nothing builds
    // and make every step after it pointless.
    action: hereStep(gitStateIsClean),
  },
  {
    name: "fmt",
    action: spawnStep("cargo", ["fmt", "--all", "--check"]),
  },
  {
    name: "prettier",
    // `--ignore-unknown` makes prettier skip file types it has no parser for instead of failing on
    // them. Given a directory it already only picks up what it understands, so this changes nothing
    // today; it matters the moment anyone passes explicit paths, where prettier otherwise exits 2
    // with "No parser could be inferred" for a file like `.nvmrc`. Those files are not unchecked: the
    // editorconfig step reads them.
    action: toolStep("prettier", ["--check", "--ignore-unknown", "."]),
  },
  {
    name: "markdownlint",
    // Globs and ignores live in `.markdownlint-cli2.jsonc`, so this takes no arguments and the
    // configuration has one home. It checks structure only; prettier owns formatting.
    action: toolStep("markdownlint-cli2", []),
  },
  {
    name: "editorconfig",
    // With no file arguments it checks everything git tracks, so it needs no globs and no ignore list
    // of its own. It is the only step that looks at `Cargo.toml`, `rust-toolchain.toml` and the
    // dotfiles: prettier cannot even infer a parser for those, and rustfmt does not see them.
    //
    // Its indent-size check is turned off in `.editorconfig-checker.json` because the width of an
    // indent here is decided by the tool that owns each file and this step does not own it for any of
    // them: rustfmt for `.rs`, prettier for Markdown, JSON, YAML and the gate's own scripts, and a
    // human for the TOML and dotfiles. Running both would be two configurations for one invariant.
    // What the check is left to say about those last two is the ending of the line, the charset and
    // the final newline, which is what it still checks. That file cannot hold the reasoning itself —
    // this tool reads it as plain JSON and answers `invalid character '/'` on a comment — so it lives
    // here and in AGENTS.md instead.
    action: toolStep("editorconfig-checker", []),
  },
  {
    name: "cspell",
    action: toolStep("cspell", [
      // Everything tracked, not just Markdown and source. The narrower `**/*.md` is measurably
      // faster — measured here at about 1065 ms against 1362 ms warm — and useless: it checked 2 of
      // this repository's 17 files. Every domain word in `project-words.txt` is in a Rust, TOML, JSON
      // or JavaScript file, so a Markdown-only spell check would have needed none of them.
      //
      // `**` reaches cspell literally, which is one of the reasons this gate spawns without a shell:
      // through one, the shell would expand it against a directory that does not exist and the step
      // would check nothing.
      //
      // `--cache` is worth about 400 ms here — measured at about 1400 ms warm against about 1800 ms
      // with the cache removed — which is not the largest saving in the gate, because two full builds
      // of the firmware dominate it by an order of magnitude. It was checked for the failure that
      // would matter: editing `project-words.txt` or `cspell.jsonc` invalidates the cache and every
      // file is re-examined, so it cannot report success from a stale result after the rules change.
      "--no-progress",
      "--gitignore",
      "--cache",
      "**",
    ]),
  },
  {
    name: "clippy",
    // `--all-features` is gone, and it is the feature that changes what is built that took it away.
    // It used to mean "every feature there is", which was one chip; it is two chips now, and enabling
    // both at once is not a third thing to check but a contradiction — measured, 43 duplicate
    // definitions out of `esp-metadata-generated` before anything else is even compiled. A step that
    // has to pass for every chip to be green cannot pass for all of them at once, so this runs once
    // per chip with that chip's feature and `--no-default-features` instead.
    action: forEachChip((chip) =>
      spawnStep("cargo", [
        "clippy",
        "--workspace",
        // Before the `--`, and that is not a matter of taste: everything after it is passed to
        // clippy-driver rather than to Cargo, so `--no-default-features` there is `error: Unrecognized
        // option` from the compiler rather than a chip selection. It is the reason the chip arguments
        // are spliced in here instead of appended at the end.
        ...chipArgs(chip),
        // `--all-targets` is deliberately absent. It makes Cargo build the test harness of every
        // target, and a bare-metal `#![no_std]` `#![no_main]` binary has none: measured, it fails with
        // `error[E0463]: can't find crate for test` and `#[panic_handler] function required`. The
        // library and the binary are what this repository has to be correct about.
        "--",
        // The lints themselves live in `[lints]` in `Cargo.toml`; `-D warnings` is what turns the
        // warnings they produce into a failure here without making the editor shout while code is half
        // written.
        "-D",
        "warnings",
      ]),
    ),
  },
  {
    name: "build",
    action: forEachChip((chip) => spawnStep("cargo", ["build", "--workspace", ...chipArgs(chip)])),
  },
  {
    name: "build-release",
    // The debug and release builds are two checks rather than one. Code behind `debug_assertions` is
    // compiled out here and code behind `not(debug_assertions)` only exists there, so a build that is
    // green in one profile can fail in the other — which on a microcontroller is the difference
    // between a bug that reproduces and one that only happens on the device.
    action: forEachChip((chip) =>
      spawnStep("cargo", ["build", "--workspace", "--release", ...chipArgs(chip)]),
    ),
  },
  {
    name: "doc",
    // One chip, and only one. `cargo doc` renders the same API for both, and the documentation is not
    // `#[cfg]`'d on the chip, so a second run would produce a second copy of the same output. The
    // default chip's arguments are still passed rather than left to `.cargo/config.toml`, so that this
    // step and the three above cannot end up compiling different chips.
    action: spawnStep("cargo", [
      "doc",
      "--workspace",
      "--no-deps",
      // The rustdoc lints are set to "deny" in `[lints.rustdoc]` in `Cargo.toml`, so a broken
      // intra-doc link fails here on its own; unlike clippy, this step needs no `-D` flag.
      ...chipArgs(defaultChip()),
    ]),
  },
  {
    name: "test",
    // The tests of this repository's own automation: the parsing and the rewriting that turn what a
    // tool prints into a verdict, which is the only code in the tree that decides something rather
    // than delegating. Node's own runner is a built-in, so this adds no dependency.
    //
    // The firmware's tests are the step after this one, and they are not here because a test on
    // `esp-poc` means a test harness for a microcontroller, which needs a board or a simulator: a
    // different kind of tooling from a formatter, and not something CI can run.
    //
    // The glob is spelled out rather than given as `tools`, because `--test` treats a bare directory as
    // a module to execute rather than as something to search — measured, it fails trying to resolve
    // the directory itself as a module.
    action: spawnStep(process.execPath, ["--test", "tools/**/*.test.mjs"]),
  },
  {
    name: "test-firmware",
    // The tests that belong to the firmware rather than to the gate, run where they can run: on the
    // host. Nothing in `src/` can be tested at all, because compiling any of it for a host pulls in
    // `esp-hal` and fails; the logic that does not touch hardware lives in `crates/poc-report`
    // precisely so that this step has something to run.
    //
    // It is the one step whose command is not written out here, because `[build] target` in
    // `.cargo/config.toml` sends `cargo test` to the microcontroller and the way back is a `--target`
    // that is a property of the machine rather than of this file. `tools/lib/firmware-tests.mjs`
    // works it out from `rustc -vV`, and says there why it is not written down.
    action: hereStep(firmwareTests),
  },
];

// The steps of the gate that can fix what they find, in the order they must run.
//
// Unlike `GATE`, order here is not presentation: these steps mutate what the next one reads, so a
// later step can undo or redo what an earlier one wrote. `editorconfig` runs after the content
// formatters because it owns files none of the others touch (`Cargo.toml`, `rust-toolchain.toml`, the
// dotfiles) and otherwise only confirms what the earlier steps already left clean. `git-state` runs
// last because it is what Git itself will act on — the endings of a line, and then the executable bit
// in the index — and because every step above it writes, so the bytes and the index are the last
// thing a fixer should be deciding on.
//
// **A fixer repairs what Git would refuse at the commit; that includes the index, not only the files
// in the working tree.** The rule used to be that fixers write files, and it was the reason the
// hooks step had none: `git update-index --chmod=+x` writes to the index, so a fixer using it seemed
// to be deciding what a commit would contain. It does not — it sets a mode and leaves the content
// unstaged — and a gate that reports a problem it cannot repair leaves the operator with a command to
// remember on the exact commit where it matters. `check` refuses to start if the installed tooling
// does not match `package-lock.json` either, and that writes a file under `node_modules`, so the
// old rule was already narrower than the practice.
//
// `clippy` and `cspell` have no entry: `cspell` cannot fix a spelling at all, and `clippy --fix` can
// rewrite code in ways that need a human to read the diff, which does not fit a command meant to run
// unattended. Run it by hand:
//
//     cargo clippy --fix --workspace --allow-dirty --allow-staged -- -D warnings
export const FIX = [
  {
    name: "fmt",
    action: spawnStep("cargo", ["fmt", "--all"]),
  },
  {
    name: "prettier",
    action: toolStep("prettier", ["--write", "--ignore-unknown", "."]),
  },
  {
    name: "markdownlint",
    action: toolStep("markdownlint-cli2", ["--fix"]),
  },
  {
    name: "editorconfig",
    action: toolStep("editorconfig-checker", ["-fix"]),
  },
  {
    name: "git-state",
    // Line endings first and the executable bit second, inside one step: the first writes files, the
    // second writes the index, and the index is what `git add` and `git commit` read last.
    action: hereStep(gitStateIsRepairable),
  },
];

// Only when this file is the program. The tests import it to reach `GATE` and `FIX` and re-run these
// steps by hand, and a module that dispatched on import would have a test run spawn the whole gate.
//
// `main`'s answer becomes the process's exit code, because that is the only thing `npm run check`,
// the `pre-commit` hook and `.github/workflows/ci.yml` can act on. It used to be discarded: nothing
// here calls `process.exit`, the value of an un-awaited promise goes nowhere, and a Node process
// whose event loop has drained exits 0 whatever the answer was. The gate therefore printed
// `6 of 13 checks failed` and then exited 0 — measured, on this repository, six steps red while the
// commit they were meant to stop went through. `process.exitCode` rather than `process.exit`, so
// Node still flushes what the gate printed on the way out.
if (isEntryPoint(process.argv[1], fileURLToPath(import.meta.url))) {
  main().then((code) => {
    process.exitCode = code;
  });
}

/**
 * Reads the command and dispatches to it.
 *
 * @returns {Promise<number>} The process exit code.
 */
async function main() {
  const args = process.argv.slice(2);
  const command = args[0];

  switch (command) {
    case "check":
      return runGate();
    case "fix":
      return runFix();
    case "setup":
      return runSetup();
    case undefined:
    case "help":
    case "--help":
    case "-h":
      printUsage();
      return 0;
    default:
      process.stderr.write(`gate: unknown command \`${command}\`\n\n`);
      printUsage();
      return 1;
  }
}

/**
 * Runs every step of the gate and reports which ones failed.
 *
 * All steps run even after one fails. A gate that stops at the first problem turns one broken commit
 * into several round trips, and the steps here are cheap enough that finishing is free.
 *
 * @returns {Promise<number>} The process exit code.
 */
async function runGate() {
  const root = workspaceRoot();

  const problem = nodeToolingState(root);
  if (problem !== undefined) {
    reportStaleTooling(problem, "Nothing was checked");
    return 1;
  }

  const failed = [];

  for (const step of GATE) {
    console.log(`\n--- ${step.name} ---`);
    if (!(await run(root, step))) {
      failed.push(step.name);
    }
  }

  if (failed.length === 0) {
    console.log(`\nall ${GATE.length} checks passed`);
    return 0;
  }

  process.stderr.write(
    `\n${failed.length} of ${GATE.length} checks failed: ${failed.join(", ")}\n`,
  );
  return 1;
}

/**
 * Runs every step in `FIX`, in order, and reports which ones still have something left to fix by
 * hand.
 *
 * Steps run in sequence and each is given the chance to run even if an earlier one still has unfixed
 * issues: a step's exit code reports what it could not fix automatically, not a broken intermediate
 * state, so there is nothing later steps need protecting from. Run `npm run check` afterward to see
 * the full picture, including `clippy` and `cspell`, which this command does not touch.
 *
 * @returns {Promise<number>} The process exit code.
 */
async function runFix() {
  const root = workspaceRoot();

  const problem = nodeToolingState(root);
  if (problem !== undefined) {
    reportStaleTooling(problem, "Nothing was fixed");
    return 1;
  }

  const remaining = [];

  for (const step of FIX) {
    console.log(`\n--- ${step.name} ---`);
    if (!(await run(root, step))) {
      remaining.push(step.name);
    }
  }

  if (remaining.length === 0) {
    console.log(`\nall ${FIX.length} fixers ran clean`);
    return 0;
  }

  process.stderr.write(
    `\n${remaining.length} of ${FIX.length} fixers still have something to fix by hand: ` +
      `${remaining.join(", ")}\n`,
  );
  return 1;
}

/**
 * Runs one step from the workspace root, returning whether it succeeded.
 *
 * @param {string} root Workspace root.
 * @param {{name: string, action: object}} step The step to run.
 * @returns {Promise<boolean>} Whether it succeeded.
 */
async function run(root, step) {
  switch (step.action.kind) {
    case "spawn":
      return spawnAndReport(root, step.action.program, step.action.args, false);

    case "tool":
      return spawnAndReport(
        root,
        toolEntryPoint(root, step.action.packageName),
        step.action.args,
        true,
      );

    case "each":
      // The steps above run even after one fails, and these do not, which is a difference rather than
      // an oversight: a step is cheap and a firmware build is not, and when one chip does not compile
      // it is nearly always this repository's own code rather than something about that chip — so the
      // second build would report the same error a second time, in as many minutes. Read the failure,
      // fix it, run again.
      for (const chip of CHIPS) {
        if (!(await run(root, { ...step, action: step.action.one(chip) }))) {
          return false;
        }
      }
      return true;

    case "here":
      return step.action.fn(root);

    default:
      throw new Error(`gate: unknown action kind \`${step.action.kind}\``);
  }
}

/**
 * Runs a child process to completion with its output attached, reporting a failure to start.
 *
 * @param {string} root Workspace root.
 * @param {string} program Executable path or name.
 * @param {string[]} args Arguments passed to it.
 * @param {boolean} isNode Whether `program` is a JavaScript file to hand to this process's Node.
 * @returns {Promise<boolean>} Whether it exited successfully.
 */
function spawnAndReport(root, program, args, isNode) {
  return new Promise((resolve) => {
    const [executable, argv] = isNode ? [process.execPath, [program, ...args]] : [program, args];

    // `npm` is the one program in this file that is a Windows shim rather than a Node script, and a
    // shim cannot be spawned without a shell. The only arguments ever passed to it are `ci` and
    // `--version`, neither of which holds a space, so the unquoted command line a shell builds is
    // the command line intended. Every other step runs a Node script directly and never sees a
    // shell at all.
    const useShell = process.platform === "win32" && !isNode && program === NPM;

    const child = spawn(executable, argv, { cwd: root, stdio: "inherit", shell: useShell });

    child.on("error", (error) => {
      process.stderr.write(`gate: could not run \`${program}\`: ${error.message}\n`);
      resolve(false);
    });

    child.on("close", (code) => resolve(code === 0));
  });
}

/**
 * Explains that the Node tooling is not usable, and what to do about it.
 *
 * @param {string} problem What is wrong with the installed tooling.
 * @param {string} consequence What was skipped as a result.
 */
function reportStaleTooling(problem, consequence) {
  process.stderr.write(`${problem}\n\n    Run \`npm run setup\` and try again.\n\n`);
  process.stderr.write(
    `${consequence}. Running only the Rust steps would print a passing summary for half a gate, ` +
      `which is worse than refusing to start.\n`,
  );
}

/**
 * Installs the Node tooling exactly as `package-lock.json` describes it.
 *
 * @returns {Promise<number>} The process exit code.
 */
async function runSetup() {
  const root = workspaceRoot();

  console.log("--- setup ---");
  if (!(await spawnAndReport(root, NPM, ["ci"], false))) {
    process.stderr.write(
      `\ngate: \`${NPM} ci\` failed. If it was not found at all, install Node first; the version ` +
        `this project expects is in .nvmrc.\n`,
    );
    return 1;
  }

  // Record which lockfile this tree was installed from. `npm run check` compares against this copy
  // rather than against timestamps, so the answer survives a checkout or a rebase.
  const target = installedLockfile(root);
  try {
    fs.copyFileSync(path.join(root, "package-lock.json"), target);
  } catch (error) {
    process.stderr.write(
      `\ngate: the tooling installed, but recording the lockfile failed: ${error.message}\n` +
        `The gate will keep asking for setup until this succeeds.\n`,
    );
    return 1;
  }

  console.log("\nNode tooling installed");
  return 0;
}

/**
 * The repository root, derived from this file's own location rather than from the current directory,
 * so that every step runs against the same paths no matter where it was invoked from.
 *
 * @returns {string} Absolute path to the repository root.
 */
function workspaceRoot() {
  return path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
}

/** Prints what this program can be asked to do. */
function printUsage() {
  console.log("Repository automation for esp-poc.");
  console.log();
  console.log("Usage: node tools/gate.mjs <command>");
  console.log();
  console.log("Commands:");
  console.log("  check    Run every quality gate step; this is what CI runs, and what you run");
  console.log("  fix      Run every step of the gate that can fix what it finds");
  console.log("  setup    Install the Node tooling the gate needs, from package-lock.json");
  console.log("  help     Show this message");
  console.log();
  console.log("Every command above is also an npm script: `npm run check`, `npm run fix`,");
  console.log("`npm run setup`.");
}
