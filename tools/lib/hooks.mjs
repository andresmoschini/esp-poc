// Whether the git hooks this repository tracks are ones Git will actually run.
//
// ## Why this step exists
//
// Git runs a hook only when two things are true of it: the file carries the executable bit in the
// index, and its first line names an interpreter. A hook missing either is not an error — Git skips
// it and the commit succeeds, which is exactly the failure this repository's rules call the worst
// one: a commit that ran no gate and said nothing.
//
// Measured on this repository, on Windows: `git add` recorded both hooks as `100644`, because
// Windows has no executable bit and Git for Windows cannot invent one from the filesystem. The
// working-tree copies were already executable and stayed that way, so a directory listing said the
// hooks were runnable while the index said Git would not run them. Nothing in the gate, and nothing
// in CI, could see it, because the gate reads files and not the index. Only `git update-index
// --chmod=+x` set the bit, and that is not something a contributor is going to remember on the
// commit where they add a third hook.
//
// So this step asks Git, rather than the filesystem, and it is the only step in the gate that reads
// the index.
//
// ## Design notes
//
// **The list of hooks comes from Git, not from a constant written here.** `git ls-files` answers
// which files under `.claude/git-hooks/` this repository tracks, so a hook added tomorrow is checked
// without this file being edited. Spelling the two names out would be a second thing to keep in step
// with the first, and keeping it in step is the work this step exists to remove.
//
// **The interpreter is read from the working-tree copy, not from `git show`.** The index says
// whether Git will run the file; what the file starts with is a property of its bytes, and
// `git show :path` would answer about the staged snapshot instead. That is the same distinction the
// `pre-commit` hook itself lives with, and it is documented under `The hooks` in CONTRIBUTING.md.
//
// **The repair writes the index rather than a file, and that is the point of `fix` being a fixer.**
// `FIX` used to hold only steps that wrote files, and the array's comment says the rule it follows;
// the rule is now the real one, which is that a fixer repairs what Git would refuse at the commit —
// and refusing a hook Git will not run is refusing the commit, not the file.
//
// **The repair is `--cacheinfo`, not `--chmod=+x`, and it is not enough on its own.** Both set the
// bit and both are sticky across a later `git add` — but only on Windows, which has no executable
// bit to read back. Measured on Linux: `--chmod=+x <path>` re-reads the working-tree copy and stages
// it, so a hook edited since it was staged would go into the commit unasked; and on either platform
// `--cacheinfo` alone is undone by the next `git add`, because Git reads the mode back from the file.
// So the fixer writes the index entry and marks the file executable, and the object name it writes
// comes from the `git ls-files --stage` record the mode came from rather than from anywhere else.

import fs from "node:fs";
import path from "node:path";

import { captureUntrimmed, reportFailure, runVisible } from "./process.mjs";

// The directory the hooks live in, relative to the workspace root. It is named here rather than
// passed in because it is a fact about this repository rather than about anything a caller decides,
// and it is the one path in this file that would otherwise be spelled out twice.
const HOOKS_DIRECTORY = ".claude/git-hooks";

// The mode Git reports for a file it will run as a hook. Git has one other regular-file mode,
// `100644`, and it means the same thing here as it does anywhere else: not executable.
const EXECUTABLE_MODE = "100755";

// The first line a file needs for the kernel to know how to start it. Git hands the rest to `sh`,
// and the shebang says so.
const SHEBANG = "#!/bin/sh";

/**
 * `check`'s step: reports every tracked hook that Git would not run, and rewrites nothing.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<boolean>} Whether the step passed.
 */
export function check(root) {
  return reportFailure(verify(root));
}

/**
 * `fix`'s step: sets the executable bit on every tracked hook Git would otherwise skip.
 *
 * It runs after `eol` because it writes the index, and the index is what `git add` and `git commit`
 * read last: a mode set before a file's endings are settled would be a mode recorded against bytes
 * that are about to change. It verifies by re-reading the index rather than by trusting the command's
 * exit code — a fixer that does not check its own work is a command that reports success on a
 * repository it did not repair.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<boolean>} Whether the step passed.
 */
export function fix(root) {
  return reportFailure(repair(root));
}

/**
 * Reads what Git reports and throws when any of it needs repairing.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<void>}
 */
async function verify(root) {
  const found = await askGit(root);

  assertThereAreHooks(found);

  const broken = found.map((hook) => problemWith(hook, root)).filter((problem) => problem !== null);

  if (broken.length === 0) {
    report(found.length);
    return;
  }

  throw new Error(
    `gate: ${broken.length} hook(s) Git would not run:\n` +
      `${broken.map((problem) => `  - ${problem}\n`).join("")}\n` +
      `    Git runs a hook only if the index says ${EXECUTABLE_MODE} and the file starts with\n` +
      `    \`${SHEBANG}\`. Windows has no executable bit, so \`git add\` records 100644 there\n` +
      `    and the working-tree copy looking executable is not an answer to Git.\n\n` +
      `    \`npm run fix\` sets a missing bit. A missing \`${SHEBANG}\` is the file's own doing:\n` +
      `    restore the line and run it again. See CONTRIBUTING.md, under The hooks.\n`,
  );
}

/**
 * Sets the executable bit on every tracked hook whose index mode is not `100755`.
 *
 * `git update-index --cacheinfo <mode>,<object>,<path>` rather than `--chmod=+x <path>`, because the
 * first rewrites the index entry the mode and object name came from and the second re-reads the
 * working-tree copy and stages it. The header says why that difference is the whole point; the short
 * of it is that a fixer which decides what a commit contains is not a fixer.
 *
 * A hook Git does not track is not repaired here, and could not be: `--cacheinfo` needs an object
 * name, and there is no index entry to read one from. `git add --chmod=+x <path>` tracks a file and
 * sets the bit in one command, and the failure message names it — staging a new file is a decision
 * about what a commit holds, and that belongs to whoever is committing.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<void>}
 */
async function repair(root) {
  const found = await askGit(root);

  assertThereAreHooks(found);

  const missingBit = found.filter((hook) => hook.mode !== EXECUTABLE_MODE);

  if (missingBit.length === 0) {
    report(found.length);
    return;
  }

  for (const hook of missingBit) {
    const entry = `${EXECUTABLE_MODE},${hook.object},${hook.relative}`;
    const ok = await runVisible(root, "git", ["update-index", "--cacheinfo", entry]);

    if (!ok) {
      throw new Error(
        `gate: could not set the executable bit on ${hook.relative}.\n` +
          `    If the hook is not tracked, \`git add --chmod=+x ${hook.relative}\` tracks it and\n` +
          `    sets the bit together.`,
      );
    }

    // The index is one half, and on a filesystem that has an executable bit the copy on disk is
    // the other: `git add` reads the mode back from the file, so a repair to the index alone is one
    // the next add undoes. Measured on Linux — `--cacheinfo` to `100755`, then `git add`, gives
    // `100644` again — and not measurable on Windows, which has no such bit and therefore could not
    // produce the failure. Both halves or the repair is not one.
    //
    // `chmod` is harmless on Windows, where it only toggles the read-only attribute and `0o755`
    // has the write bits set.
    try {
      fs.chmodSync(path.join(root, hook.relative), 0o755);
    } catch (error) {
      throw new Error(
        `gate: set the executable bit on ${hook.relative} in the index, but not on the file\n` +
          `    itself: ${error.code ?? error.message}\n` +
          `    Git reads the mode back from the file on the next \`git add\`, so this repair will\n` +
          `    not hold until the file is executable too.`,
      );
    }
  }

  const after = await askGit(root);

  // The index, read back. Everything about the mode this step can fix, it can fix, so a hook still
  // wrong here is one whose contents are wrong — and the operator is the one who has to see that
  // rather than a fixer that reported success on a hook Git will skip.
  const stillBroken = after
    .map((hook) => problemWith(hook, root))
    .filter((problem) => problem !== null);

  if (stillBroken.length > 0) {
    throw new Error(
      `gate: ${stillBroken.length} hook(s) Git would still not run after setting the bit:\n` +
        `${stillBroken.map((problem) => `  - ${problem}\n`).join("")}\n` +
        `    The bit is set; what is left is the file itself. Run \`npm run check\` and read what\n` +
        `    it says about them.`,
    );
  }

  for (const hook of missingBit) {
    console.log(`  ${hook.relative}: ${hook.mode} → ${EXECUTABLE_MODE}`);
  }
  console.log(`${missingBit.length} hook(s) marked executable in the index`);
}

/**
 * Refuses a repository holding no tracked hook at all, which is a failure rather than nothing to do.
 *
 * A fixer cannot repair that one: there is no file to set a bit on, and inventing one would be a
 * decision about what this repository's commits run. The check is where the answer belongs, and this
 * says so rather than passing on an empty answer.
 *
 * @param {Array<{relative: string, mode: string}>} found What Git reported.
 */
function assertThereAreHooks(found) {
  if (found.length > 0) {
    return;
  }

  throw new Error(
    `gate: no hook is tracked under ${HOOKS_DIRECTORY}/.\n\n` +
      `    A commit that finds no hook runs no gate, and says nothing. Restore them with\n` +
      `    \`git checkout ${HOOKS_DIRECTORY}\`, or see CONTRIBUTING.md, under The hooks.`,
  );
}

/**
 * Says that nothing here needed doing, naming how many hooks that was.
 *
 * @param {number} count How many hooks Git tracks.
 */
function report(count) {
  console.log(
    `every hook Git tracks under ${HOOKS_DIRECTORY}/ is one Git will run (${count} of them)`,
  );
}

/**
 * Asks Git which hook files this repository tracks, and what mode the index holds each at.
 *
 * `--stage 0` is what makes the answer about one entry rather than about a merge in progress, where
 * a conflicted path has an entry per stage and the modes can disagree.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<Array<{relative: string, mode: string}>>} One entry per tracked hook.
 */
async function askGit(root) {
  const records = await captureUntrimmed(root, "git", [
    "ls-files",
    "-z",
    "--stage",
    "--",
    HOOKS_DIRECTORY,
  ]).catch((error) => {
    throw new Error(`gate: ${error.message}`);
  });

  return parseAll(records);
}

/**
 * Reads every NUL-separated record of `git ls-files --stage` output.
 *
 * `-z` is what keeps a path holding a space, a quote or a newline intact between records, which is
 * the same reason `eol` asks the same question the same way.
 *
 * @param {string} records NUL-separated records, as `git ls-files` printed them.
 * @returns {Array<{relative: string, mode: string}>} One entry per tracked hook.
 */
export function parseAll(records) {
  return records
    .split("\0")
    .filter((record) => record !== "")
    .map(parse)
    .sort((left, right) => left.relative.localeCompare(right.relative));
}

/**
 * Reads one NUL-separated record of `git ls-files --stage` output.
 *
 * A record is a mode, an object name and a stage, then a tab, then the path:
 *
 * ```text
 * 100755 6b2c1d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0 0<TAB>.claude/git-hooks/commit-msg
 * ```
 *
 * The object name is read and carried although only `fix` uses it, because it is the same record:
 * reading the mode out of it and looking the name up again later would be two questions asked of Git
 * where one record answers both.
 *
 * @param {string} record One record.
 * @returns {{relative: string, mode: string, object: string}} What it says.
 */
export function parse(record) {
  const tab = record.indexOf("\t");
  if (tab === -1) {
    throw new Error(
      `gate: \`git ls-files --stage\` printed a record with no path in it: ${record}\n\n    The ` +
        `mode, object name and stage are separated from the path by a tab, and this one has none.`,
    );
  }

  const [mode, object] = record.slice(0, tab).split(/\s+/);

  return { relative: record.slice(tab + 1), mode, object };
}

/**
 * What is wrong with one hook, or `null` when nothing is.
 *
 * The two failures are reported separately rather than as one answer, because they have different
 * fixes and only one of them is the mode: a file can be executable and still name no interpreter, and
 * a file can be perfect in the index and have lost its first line in an edit that nothing else here
 * would notice.
 *
 * @param {{relative: string, mode: string}} hook What Git reported.
 * @param {string} root Workspace root.
 * @returns {string | null} The problem, or `null` when the hook is one Git will run.
 */
export function problemWith(hook, root) {
  if (hook.mode !== EXECUTABLE_MODE) {
    return `${hook.relative} is ${hook.mode} in the index, so Git will not run it`;
  }

  const filePath = path.join(root, hook.relative);

  let firstLine;
  try {
    firstLine = fs.readFileSync(filePath, "utf8").split("\n", 1)[0].replace(/\r$/, "");
  } catch (error) {
    // A hook staged for deletion has no working-tree copy to read, and a hook Git tracks that is not
    // on disk is not a hook this repository can run. Reporting it is the honest answer; the operator
    // either restores the file or stages the deletion, and either way the next run is right.
    return `${hook.relative} is tracked but could not be read (${error.code ?? error.message})`;
  }

  if (firstLine !== SHEBANG) {
    return (
      `${hook.relative} starts with ${JSON.stringify(firstLine)} rather than \`${SHEBANG}\`, ` +
      `so Git has no interpreter to run it with`
    );
  }

  return null;
}
