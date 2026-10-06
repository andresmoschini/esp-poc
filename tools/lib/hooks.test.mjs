// The tests for the hook check, which is the only step in the gate that reads the index.
//
// The failure it exists to catch is silent by construction: Git skips a hook it cannot run and the
// commit succeeds, so a repository can hold hooks that no commit has ever executed and every other
// step will keep reporting green. Measured here, on Windows, `git add` recorded both hooks as `100644`
// while the working-tree copies were already executable, so nothing short of asking Git would have
// noticed.
//
// Everything below is therefore a function of a string Git printed or of bytes on disk, and the tests
// drive it with both rather than by damaging a repository.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { parse, parseAll, problemWith } from "./hooks.mjs";

// A record as `git ls-files --stage` prints one. The object name is not read by anything here, so it
// is short, but it is there because the tab has to be after three fields for the record to be one.
const record = (mode, relative) => `${mode} 6b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 0\t${relative}`;

/**
 * A temporary workspace holding one hook file.
 *
 * @param {string} relative Path of the hook, relative to the root.
 * @param {string} contents What the file says.
 * @returns {string} The root, which the caller removes.
 */
function workspaceWith(relative, contents) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "esp-poc-hooks-"));
  const filePath = path.join(root, relative);
  fs.mkdirSync(path.dirname(filePath), { recursive: true });
  fs.writeFileSync(filePath, contents);
  return root;
}

// A hook Git will run: executable in the index, and naming its interpreter.
const RUNNABLE = "#!/bin/sh\nexec npm run check\n";

// --- Parsing what Git printed --------------------------------------------------

// Two hooks and two records, which is the shape the step sees on a healthy checkout. `-z` is what
// keeps a record intact, so the parse is over a NUL-separated string rather than over lines.
test("every tracked hook is read out of the records", () => {
  const found = parseAll(
    [
      record("100755", ".claude/git-hooks/pre-commit"),
      record("100755", ".claude/git-hooks/commit-msg"),
    ].join("\0"),
  );

  assert.deepEqual(found, [
    { relative: ".claude/git-hooks/commit-msg", mode: "100755" },
    { relative: ".claude/git-hooks/pre-commit", mode: "100755" },
  ]);
});

// The records come back sorted, so the failure a run prints is in the same order every time rather
// than in whatever order the filesystem happened to answer in.
test("the hooks come back sorted by path", () => {
  const found = parseAll(
    [
      record("100755", ".claude/git-hooks/pre-commit"),
      record("100755", ".claude/git-hooks/commit-msg"),
    ].join("\0"),
  );

  assert.deepEqual(
    found.map((hook) => hook.relative),
    [".claude/git-hooks/commit-msg", ".claude/git-hooks/pre-commit"],
  );
});

// An empty answer is a repository holding no hooks, which is a failure rather than a pass: a commit
// that finds no hook runs no gate. The step reports it; all this has to agree is that nothing is
// invented from an empty string.
test("no records means no hooks", () => {
  assert.deepEqual(parseAll(""), []);
  assert.deepEqual(parseAll("\0"), []);
});

// The tab is the only thing separating the mode from the path, and a record without one is output this
// step cannot read. Saying so beats reading `undefined` as a mode and reporting it as a missing bit.
test("a record with no path in it is refused", () => {
  assert.throws(() => parse("100755 deadbeef 0"), /no path in it/);
});

// A path is whatever follows the tab, spaces included. Splitting a record on whitespace instead is
// what would break on a directory holding a space in its name, and `git ls-files` is quoting nothing.
test("a path holding a space survives the parse", () => {
  const found = parse(record("100755", ".claude/git-hooks/a hook with spaces"));

  assert.equal(found.relative, ".claude/git-hooks/a hook with spaces");
  assert.equal(found.mode, "100755");
});

// --- The two failures, one at a time -------------------------------------------

// The one this step was written for: the index says Git will not run it, however executable the
// working-tree copy looks. Windows is where this is produced, because it has no bit to record.
test("a hook the index does not hold executable is reported", () => {
  const root = workspaceWith(".claude/git-hooks/pre-commit", RUNNABLE);

  try {
    const problem = problemWith({ relative: ".claude/git-hooks/pre-commit", mode: "100644" }, root);

    assert.match(problem, /100644 in the index, so Git will not run it/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// The other failure, and it is independent of the mode: a file can be executable in the index and
// still have no interpreter, which an edit that dropped the first line would leave behind.
test("a hook with no interpreter line is reported even when it is executable", () => {
  const root = workspaceWith(".claude/git-hooks/pre-commit", "exec npm run check\n");

  try {
    const problem = problemWith({ relative: ".claude/git-hooks/pre-commit", mode: "100755" }, root);

    assert.match(problem, /no interpreter to run it with/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// The honest answer for both: nothing is wrong, and the check says so rather than passing silently
// on a file it never opened.
test("a runnable hook has no problem", () => {
  const root = workspaceWith(".claude/git-hooks/commit-msg", RUNNABLE);

  try {
    assert.equal(
      problemWith({ relative: ".claude/git-hooks/commit-msg", mode: "100755" }, root),
      null,
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// A hook Git tracks and the worktree no longer holds is not one this repository can run, and saying
// "could not be read" is more useful than passing on a file that is not there.
test("a tracked hook that is not on disk is reported", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "esp-poc-hooks-"));

  try {
    const problem = problemWith({ relative: ".claude/git-hooks/pre-commit", mode: "100755" }, root);

    assert.match(problem, /tracked but could not be read/);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// --- The one thing a shell would have caught -----------------------------------

// A CRLF first line ends in a carriage return, so a byte-exact comparison against `#!/bin/sh` fails
// on a file that is otherwise perfect. That is not a hypothetical on this repository: it is Windows,
// and it is why the `eol` step exists. Reading the line has to tolerate the ending Git is about to
// convert anyway, or the check fails on every fresh clone there.
test("a CRLF line ending does not make a good hook look broken", () => {
  const root = workspaceWith(".claude/git-hooks/pre-commit", "#!/bin/sh\r\nexec npm run check\r\n");

  try {
    assert.equal(
      problemWith({ relative: ".claude/git-hooks/pre-commit", mode: "100755" }, root),
      null,
    );
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
