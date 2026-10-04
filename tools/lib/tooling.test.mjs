// The tests for the check that decides whether the gate may run at all.
//
// Both `check` and `fix` refuse to start on it, so this is the one piece of the gate whose answer is
// "stop" rather than "pass" or "fail", and it is asked before anything else happens. The property
// that matters most is the one that replaced an earlier version of this: the comparison is by
// content, because comparing timestamps reported a stale tree after every branch switch — a false
// alarm that costs a reinstall to clear and teaches people to ignore the one message the gate cannot
// work without.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { installedLockfile, nodeToolingState } from "./tooling.mjs";

const LOCKFILE = '{"lockfileVersion": 3}\n';

/**
 * A tree holding a lockfile and whatever `setup` would have recorded beside the install.
 *
 * @param {string | undefined} recorded What the install record holds, or `undefined` for a tree
 *   where `setup` has never run.
 * @returns {string} The workspace root.
 */
function fixture(recorded) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "gate-tooling-"));

  fs.writeFileSync(path.join(root, "package-lock.json"), LOCKFILE);
  if (recorded !== undefined) {
    fs.mkdirSync(path.join(root, "node_modules"), { recursive: true });
    fs.writeFileSync(installedLockfile(root), recorded);
  }

  return root;
}

// A tree nobody has installed into is told to run `setup`, and is told which file is missing, because
// the two failures an operator meets — never installed, and installed from a different lockfile — are
// fixed by different readings of that one sentence.
test("a tree with no install record is reported as not installed", () => {
  const root = fixture(undefined);

  assert.match(nodeToolingState(root), /not installed/u);
  assert.ok(nodeToolingState(root).includes(installedLockfile(root)));
});

// The record lives inside `node_modules` because `npm ci` deletes that directory before reinstalling,
// so a record anywhere else would outlive the tree it describes and report a match for a tree that is
// gone.
test("the install record lives inside the tree npm replaces", () => {
  assert.equal(
    path.dirname(installedLockfile("C:/repo")),
    path.join("C:/repo", "node_modules").replace(/[\\/]/gu, path.sep),
  );
});

// Installed from this lockfile: nothing to say, which is the answer both commands need in order to
// proceed.
test("a tree installed from this lockfile is usable", () => {
  assert.equal(nodeToolingState(fixture(LOCKFILE)), undefined);
});

// The record has to live inside `node_modules` to be there at all, so a mismatch is not something a
// missing file causes: it is the lockfile having moved on.
test("a record from a different lockfile is reported", () => {
  const problem = nodeToolingState(fixture('{"lockfileVersion": 3, "packages": {}}\n'));

  assert.match(problem, /does not match the lockfile/u);
});

// This is the one that is easy to get wrong: Git rewrites `package-lock.json` on checkout with
// content it did not change, and the earlier timestamp comparison reported that as a stale tree on
// every branch switch and every rebase.
test("a record of the same content is usable however old it is", () => {
  const root = fixture(LOCKFILE);
  const longAgo = new Date("2001-01-01T00:00:00Z");

  fs.utimesSync(installedLockfile(root), longAgo, longAgo);
  fs.utimesSync(path.join(root, "package-lock.json"), longAgo, longAgo);

  assert.equal(nodeToolingState(root), undefined);
});
