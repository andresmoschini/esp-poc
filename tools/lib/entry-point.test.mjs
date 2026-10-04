// The tests for the entry-point guard, which is the one piece of `gate.mjs` whose failure is silent.
//
// The dangerous direction is the one a test run exercises: importing `gate.mjs` must not dispatch.
// Every other test in this tree that reaches for `GATE` or `FIX` depends on that holding, and a guard
// that let the import through would spawn the whole gate from inside the test run — which looks like
// a hang rather than a failure, and is why this is worth a test of its own.

import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { isEntryPoint } from "./entry-point.mjs";

const GATE = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "gate.mjs");

// The program the operator named is this program, which is the case that has to keep working: a gate
// that never dispatched would report success while checking nothing.
test("the file the operator named is the entry point", () => {
  assert.equal(isEntryPoint(GATE, GATE), true);
});

// The test runner is the program, not the module. This is the case that keeps a test run from
// dispatching whatever command line it was given.
test("another program is not the entry point", () => {
  assert.equal(isEntryPoint(process.execPath, GATE), false);
});

// Node leaves `argv[1]` unset when it runs a module without naming one, and `undefined` is not a path.
test("a run that named no file is not the entry point", () => {
  assert.equal(isEntryPoint(undefined, GATE), false);
});

// A path that does not resolve is not the entry point either. The failure here would be a stale
// command line in CI, where the answer has to be "do nothing" rather than an exception.
test("a path that does not exist is not the entry point", () => {
  assert.equal(isEntryPoint(path.join(GATE, "gone"), GATE), false);
});

// A relative path is still the same file, which is how `node tools/gate.mjs check` arrives: Node
// resolves it before it becomes `argv[1]`, and a caller who did not is answered the same way.
test("a relative path to the same file is the entry point", () => {
  const relative = path.relative(process.cwd(), GATE);

  assert.equal(path.isAbsolute(relative), false);
  assert.equal(isEntryPoint(path.resolve(relative), GATE), true);
});
