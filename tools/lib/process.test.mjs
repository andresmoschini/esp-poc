// The tests for the subprocess wrappers, which every other part of this repository's automation is
// built on: `setup` drives `npm` through them, `eol` drives `git`, and every gate step that is not a
// repository function is one of these wrapped around a program.
//
// They spawn this Node rather than a fixture program, which is what keeps them honest: there is no
// fake command whose behavior has to be maintained alongside the tests, and the exit codes and the
// output are the real ones a real program produces.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { capture, captureUntrimmed, runVisible } from "./process.mjs";

// Every spawn here runs from the workspace root, which is what the callers do; the value is only
// there to prove the two agree. A program that prints it is enough to tell them apart.
const ROOT = process.cwd();

/**
 * A Node program that prints to standard output.
 *
 * @param {string} source The body of an `-e` program.
 * @returns {string[]} Arguments naming it.
 */
const printing = (source) => ["-e", source];

/**
 * `text` as a regular expression, with the characters a path is likely to hold already escaped.
 *
 * @param {string} text Literal text.
 * @returns {string} The escaped text.
 */
const escape = (text) => text.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");

// The output a command's output is read for has had the newline `println` adds taken off it, because
// every caller wants the value rather than the line.
test("captured output is trimmed", async () => {
  assert.equal(await capture(ROOT, process.execPath, printing("console.log('alpha')")), "alpha");
});

// The one caller that reads a file out of `origin/main` needs every byte, because trimming a leading
// blank line would shift every line number reported to the operator by one.
test("output can be read without trimming it", async () => {
  const program = printing("process.stdout.write('\\nalpha\\n')");

  assert.equal(await captureUntrimmed(ROOT, process.execPath, program), "\nalpha\n");
  assert.equal(await capture(ROOT, process.execPath, program), "alpha");
});

// Standard error is reported when a command fails and is not mistaken for its output when it
// succeeds: a caller reading a value must not find a warning where the value should be.
test("standard error is kept out of the captured output", async () => {
  const program = printing("console.error('a warning'); console.log('alpha')");

  assert.equal(await capture(ROOT, process.execPath, program), "alpha");
});

// A failure names the command, its arguments and its exit code, because the operator is looking at
// the message alone: `npm run check` failing on `editorconfig-checker` says nothing about which
// arguments it was given. The exit code is the one the program chose rather than a fixed number,
// since what has to survive is the reporting of it and not any particular tool's convention.
test("a command that fails is reported with what it was asked to do", async () => {
  const failure = await capture(ROOT, process.execPath, printing("process.exit(4)"))
    .then(() => undefined)
    .catch((error) => error);

  assert.ok(failure instanceof Error, "a failing command should reject");
  assert.match(failure.message, new RegExp(`\`${escape(process.execPath)} -e`, "u"));
  assert.match(failure.message, /exited with 4/u);
});

// Whatever the command wrote to standard error is the explanation, so it is in the message rather
// than lost with the child's output.
test("a failure carries the explanation the command printed", async () => {
  const failure = await capture(
    ROOT,
    process.execPath,
    printing("console.error('the reason'); process.exit(3)"),
  )
    .then(() => undefined)
    .catch((error) => error);

  assert.match(failure.message, /exited with 3/u);
  assert.match(failure.message, /the reason/u);
});

// A program that is not installed is a different problem from a program that failed, and it is
// reported as the first: the answer is to install it, not to read its output.
test("a command that cannot be started is reported", async () => {
  const failure = await capture(ROOT, "esp-poc-no-such-program", [])
    .then(() => undefined)
    .catch((error) => error);

  assert.ok(failure instanceof Error, "a missing program should reject");
  assert.match(failure.message, /could not run `esp-poc-no-such-program`/u);
});

// The visible runner answers a question rather than a value: exit code zero is success and anything
// else is a step that failed, which is the whole contract the gate is built on.
test("the visible runner answers from the exit code", async () => {
  assert.equal(await runVisible(ROOT, process.execPath, printing("")), true);
  assert.equal(await runVisible(ROOT, process.execPath, printing("process.exit(1)")), false);
});

// A step whose program is missing is a failed step and not a crashed gate. `npm run check` has to
// come back with a summary either way, or a missing tool in CI stops the run without saying which
// step wanted it.
test("the visible runner reports a program it cannot start", async () => {
  assert.equal(await runVisible(ROOT, "esp-poc-no-such-program", []), false);
});

// The one step that has to run from somewhere else. The directory is where the child runs, not what it
// is given, so a program that prints its own directory is the only way to see which one it was.
test("the visible runner can run a step from another directory", async () => {
  const elsewhere = fs.mkdtempSync(path.join(os.tmpdir(), "gate-elsewhere-"));

  try {
    const printed = await capture(
      elsewhere,
      process.execPath,
      printing("console.log(process.cwd())"),
    );

    assert.equal(path.resolve(printed), fs.realpathSync(elsewhere));
  } finally {
    fs.rmSync(elsewhere, { recursive: true, force: true });
  }
});
