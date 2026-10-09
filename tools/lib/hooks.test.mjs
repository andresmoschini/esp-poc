// The tests for the hook check and fixer, which are the only step in the gate that reads the index.
//
// The failure they exist to catch is silent by construction: Git skips a hook it cannot run and the
// commit succeeds, so a repository can hold hooks that no commit has ever executed and every other
// step will keep reporting green. Measured here, on Windows, `git add` recorded both hooks as `100644`
// while the working-tree copies were already executable, so nothing short of asking Git would have
// noticed.
//
// Everything below is a function of a string Git printed, of bytes on disk, or of an index a real
// repository was asked to hold — and the last of those drives a real `git`, because the whole claim
// under test is what Git does, and a fabricated index would only be a claim about this file.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { spawnSync } from "node:child_process";

import { fix, parse, parseAll, problemWith } from "./hooks.mjs";

// The object name a fabricated record carries. `fix` hands it back to `git update-index
// --cacheinfo`, so it has to be a name rather than an omission.
const OBJECT = "6b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0";

// A record as `git ls-files --stage` prints one: mode, object name, stage, a tab, and the path. The
// tab has to be after three fields for the record to be one at all.
const record = (mode, relative) => `${mode} ${OBJECT} 0\t${relative}`;

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

/**
 * A temporary Git repository holding one hook, as `git add` on Windows would leave it.
 *
 * A real repository rather than a fabricated index, because the claim under test is what
 * `git update-index --chmod=+x` does — the mode it holds afterwards, and what it leaves staged — and
 * the only way to know either is to ask Git.
 *
 * @param {string} relative Path of the hook, relative to the root.
 * @param {string} contents What the file says.
 * @returns {string} The root, which the caller removes.
 */
function repositoryWith(relative, contents) {
  const root = workspaceWith(relative, contents);

  const git = (...args) => spawnSync("git", args, { cwd: root, encoding: "utf8" }).status === 0;

  assert.ok(git("init", "--quiet"), "could not create a repository to test against");

  return root;
}

/**
 * The mode Git's index holds a path at, or `null` for a path the index does not have.
 *
 * @param {string} root Repository root.
 * @param {string} relative Path, relative to the root.
 * @returns {string | null} The mode, or `null` when the path is not in the index.
 */
function modeInIndex(root, relative) {
  const listed = spawnSync("git", ["ls-files", "--stage", "--", relative], {
    cwd: root,
    encoding: "utf8",
  });

  if (listed.status !== 0) {
    return null;
  }

  const [record] = listed.stdout.split("\n").filter(Boolean);

  return record === undefined ? null : record.split(/\s+/u)[0];
}

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
    {
      relative: ".claude/git-hooks/commit-msg",
      mode: "100755",
      object: OBJECT,
    },
    { relative: ".claude/git-hooks/pre-commit", mode: "100755", object: OBJECT },
  ]);
});

// The object name is what `fix` hands back to `git update-index --cacheinfo`, so a record that lost
// it would leave the fixer with no way to write an entry that changes only the mode.
test("the object name is read out of the record", () => {
  assert.equal(parse(record("100644", ".claude/git-hooks/pre-commit")).object, OBJECT);
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

// --- The fixer, against a repository Git agrees with -----------------------------

// The mode Windows leaves behind, and the case the fixer was written for: `git add` records `100644`
// however executable the working-tree copy looks, so the step has something to repair on exactly the
// platform where the hook is most likely to be added.
test("the fixer sets the bit Git's index is missing", async () => {
  const root = repositoryWith(".claude/git-hooks/pre-commit", RUNNABLE);

  try {
    spawnSync("git", ["add", ".claude/git-hooks/pre-commit"], { cwd: root });

    const before = modeInIndex(root, ".claude/git-hooks/pre-commit");
    assert.ok(before === "100644" || before === "100755", "`git add` recorded a mode to repair");

    await fix(root);

    assert.equal(modeInIndex(root, ".claude/git-hooks/pre-commit"), "100755");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// What makes the repair worth automating, and what makes it safe: the mode changes and the content
// does not. A hook staged at one revision and then edited in the worktree must still hold the staged
// revision after `fix` — `git update-index --chmod=+x <path>` does not, which is measured in
// `tools/lib/hooks.mjs` and is why the fixer uses `--cacheinfo` instead.
test("the fixer changes the mode without staging the file's contents", async () => {
  const root = repositoryWith(".claude/git-hooks/pre-commit", RUNNABLE);

  try {
    spawnSync("git", ["add", ".claude/git-hooks/pre-commit"], { cwd: root });

    const edited = `${RUNNABLE}# edited\n`;
    fs.writeFileSync(path.join(root, ".claude/git-hooks/pre-commit"), edited);

    await fix(root);

    assert.equal(modeInIndex(root, ".claude/git-hooks/pre-commit"), "100755");
    assert.equal(
      spawnSync("git", ["cat-file", "-p", ":.claude/git-hooks/pre-commit"], {
        cwd: root,
        encoding: "utf8",
      }).stdout,
      RUNNABLE,
      "the index should still hold what was staged, not the edit",
    );
    assert.equal(fs.readFileSync(path.join(root, ".claude/git-hooks/pre-commit"), "utf8"), edited);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// The repair is sticky, so it is a fix rather than something to remember on the next commit: a later
// `git add` of the same path keeps the bit the fixer set.
test("the bit the fixer sets survives a later add", async () => {
  const root = repositoryWith(".claude/git-hooks/pre-commit", RUNNABLE);

  try {
    spawnSync("git", ["add", ".claude/git-hooks/pre-commit"], { cwd: root });

    await fix(root);
    spawnSync("git", ["add", ".claude/git-hooks/pre-commit"], { cwd: root });

    assert.equal(modeInIndex(root, ".claude/git-hooks/pre-commit"), "100755");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// A repository with nothing wrong is a pass rather than an error, and the fixer says so instead of
// running a command per hook to discover there was no work.
test("the fixer passes on a repository with nothing to repair", async () => {
  const root = repositoryWith(".claude/git-hooks/pre-commit", RUNNABLE);

  try {
    spawnSync("git", ["add", "--chmod=+x", ".claude/git-hooks/pre-commit"], { cwd: root });

    assert.equal(await fix(root), true);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

// A hook with no interpreter cannot be repaired by setting a bit, and a fixer that reported success
// on it would be the silent failure this step exists to catch. It fails rather than passing.
test("the fixer fails on a hook it cannot repair", async () => {
  const root = repositoryWith(".claude/git-hooks/pre-commit", "exec npm run check\n");

  try {
    spawnSync("git", ["add", ".claude/git-hooks/pre-commit"], { cwd: root });

    assert.equal(await fix(root), false);
    assert.equal(modeInIndex(root, ".claude/git-hooks/pre-commit"), "100755");
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});
