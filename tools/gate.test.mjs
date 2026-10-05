// The tests for the gate's own shape, which is the thing CI trusts to mean what it says.
//
// Nothing here runs a step. What it checks are the properties CONTRIBUTING.md asks every change to
// `GATE` and `FIX` to keep, read back out of the arrays rather than remembered: a step whose name is
// repeated reports twice under one heading, a fixer for a step that no longer exists runs a command
// nobody checks the answer to, and a test file the glob cannot reach is a file whose tests never ran
// while the gate still said they did.

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { CHIPS } from "./lib/chip.mjs";
import { FIX, GATE } from "./gate.mjs";

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/**
 * The pattern a glob stands for, as something that can be matched against a path.
 *
 * `**` crosses directories and `*` does not, and the pattern is compared rather than run because the
 * claim under test is that a name is reachable by the pattern — running the glob would only prove
 * that today's files pass.
 *
 * @param {string} pattern Glob as the gate spells it, with `/` separators.
 * @returns {RegExp} A matcher for the paths it reaches.
 */
function globToRegExp(pattern) {
  const segments = pattern.split("/");
  let source = "";
  let separatorPending = false;

  // `**` is a whole segment here, and it carries the separator that follows it: `a/**/b` is `a/`,
  // then any number of whole directories each ending in one, then `b`. Emitting that separator after
  // the group as well would ask for `a//b` in the zero-directory case, which is the case the gate's
  // own pattern depends on — `tools/**/*.test.mjs` has to reach a test file sitting directly in
  // `tools/`.
  for (const [at, segment] of segments.entries()) {
    if (separatorPending) {
      source += "/";
    }

    if (segment === "**") {
      source += at === segments.length - 1 ? "(?:[^/]+/)?" : "(?:[^/]+/)*";
      separatorPending = false;
    } else {
      source += segment.replace(/[.+?^${}()|[\]\\]/gu, "\\$&").replace(/\*/gu, "[^/]*");
      separatorPending = true;
    }
  }

  return new RegExp(`^${source}$`, "u");
}

// The summary line names how many steps ran, so two steps under one name would make that count wrong
// as well as the output ambiguous. Neither has ever happened here, which is why it needs a test
// rather than a habit.
test("no two steps of the gate share a name", () => {
  const names = GATE.map((step) => step.name);

  assert.deepEqual(
    names.filter((name, at) => names.indexOf(name) !== at),
    [],
    "a repeated step name makes the summary count wrong",
  );
});

// Every fixer corresponds to a check. A fixer with no check is a command that rewrites files on a
// schedule nothing verifies, and the gate would report green either way.
test("every fixer is a step the gate checks", () => {
  const checked = GATE.map((step) => step.name);

  for (const step of FIX) {
    assert.ok(
      checked.includes(step.name),
      `\`fix\` has a \`${step.name}\` step that \`check\` does not run`,
    );
  }
});

// A step is either a command or a function of this repository's code, and a third shape is a step
// `run()` cannot execute: it throws on an unknown kind, which would stop the gate mid-pass rather than
// report that step as failed.
test("every step is an action `run` knows how to execute", () => {
  const kinds = ["spawn", "tool", "here", "each"];

  for (const step of GATE) {
    assert.ok(
      kinds.includes(step.action.kind),
      `step \`${step.name}\` has action kind \`${step.action.kind}\``,
    );
  }
});

// The one step that is several commands rather than one has to be several commands for *every* chip.
// A step that reads a chip's arguments out of anything but `chipArgs` has restated the chip somewhere
// that no test covers, and the failure is a build error about a triple rather than a wrong answer.
test("every multi-command step asks for each chip by name", () => {
  const perChip = GATE.filter((step) => step.action.kind === "each");

  assert.ok(perChip.length > 0, "no step runs per chip, so no chip is checked by the gate");

  for (const step of perChip) {
    for (const chip of CHIPS) {
      const action = step.action.one(chip);

      assert.ok(
        action.args.includes(chip.feature) && action.args.includes(chip.triple),
        `step \`${step.name}\` does not pass ${chip.feature} and ${chip.triple} to ${action.program}`,
      );
    }
  }
});

// Everything after a bare `--` is the compiler's, not Cargo's. Putting a chip's selection on the wrong
// side of it produces `error: Unrecognized option: 'no-default-features'` from clippy-driver, which
// names neither the chip nor the file to open — measured, on the step that had it.
test("no step puts a chip's arguments after a `--`", () => {
  for (const step of GATE) {
    if (step.action.kind !== "spawn") {
      continue;
    }

    const separator = step.action.args.indexOf("--");
    const chips = step.action.args.filter((arg) => CHIPS.some((chip) => arg === chip.triple));

    if (separator !== -1 && chips.length > 0) {
      assert.fail(`step \`${step.name}\` names a triple after \`--\`, where Cargo never reads it`);
    }
  }
});

// The steps that run an npm-installed tool have to name a package this repository depends on, or the
// step fails at run time over a tool that was never installed.
test("every tool step names a package this repository depends on", () => {
  const declared = Object.keys(
    JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).devDependencies,
  );

  for (const step of GATE.filter((candidate) => candidate.action.kind === "tool")) {
    assert.ok(
      declared.includes(step.action.packageName),
      `step \`${step.name}\` runs \`${step.action.packageName}\`, which package.json does not list`,
    );
  }
});

// The glob is what decides which tests run. A test file that does not match it is dead weight that
// still reads as coverage, and nothing else in the gate would notice: the step would pass on the
// files it did find.
//
// The pattern is compared literally rather than by running the glob, because the claim under test is
// "this name is reachable by this pattern", and running it would only prove that today's files pass.
test("every test file in the tree is one the test step reaches", () => {
  const pattern = GATE.find((step) => step.name === "test").action.args[1];
  const matcher = globToRegExp(pattern);

  const found = fs
    .readdirSync(path.join(ROOT, "tools"), { recursive: true, withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".test.mjs"))
    .map((entry) => path.join(entry.parentPath, entry.name).split(path.sep).join("/"))
    .map((absolute) => path.relative(ROOT, absolute).split(path.sep).join("/"));

  assert.ok(found.length > 0, "the glob reached no test files at all");

  for (const file of found) {
    assert.match(file, matcher, `${file} is not one the \`test\` step runs`);
  }
});

// The two commands that have a script have to keep the same name in both places, because
// CONTRIBUTING.md and the README tell the reader to run `npm run check` while CI runs
// `node tools/gate.mjs check`.
test("every npm script is a command the gate dispatches", () => {
  const source = fs.readFileSync(path.join(ROOT, "tools", "gate.mjs"), "utf8");
  const scripts = JSON.parse(fs.readFileSync(path.join(ROOT, "package.json"), "utf8")).scripts;

  for (const [name, command] of Object.entries(scripts)) {
    assert.match(
      command,
      new RegExp(`gate\\.mjs ${name}\\b`, "u"),
      `npm script \`${name}\` does not call \`gate.mjs ${name}\``,
    );
    assert.match(source, new RegExp(`case "${name}":`, "u"), `gate.mjs dispatches no \`${name}\``);
  }
});
