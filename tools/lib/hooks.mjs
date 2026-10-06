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
// **This step fixes nothing.** `git update-index --chmod=+x` could set the bit, and it is the right
// command, but it writes to the index rather than to a file — and every other fixer in `FIX` writes
// to files and leaves staging to the operator. A fixer that stages would make `npm run fix` change
// what a commit is about to contain, which is a decision rather than a repair. The failure message
// names the command instead.

import fs from "node:fs";
import path from "node:path";

import { captureUntrimmed } from "./process.mjs";

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
export async function check(root) {
  try {
    const records = await askGit(root);
    const found = parseAll(records);

    if (found.length === 0) {
      throw new Error(
        `gate: no hook is tracked under ${HOOKS_DIRECTORY}/.\n\n` +
          `    A commit that finds no hook runs no gate, and says nothing. Restore them with\n` +
          `    \`git checkout ${HOOKS_DIRECTORY}\`, or see CONTRIBUTING.md, under The hooks.`,
      );
    }

    const broken = found
      .map((hook) => problemWith(hook, root))
      .filter((problem) => problem !== null);

    if (broken.length === 0) {
      console.log(
        `every hook Git tracks under ${HOOKS_DIRECTORY}/ is one Git will run ` +
          `(${found.length} of them)`,
      );
      return true;
    }

    throw new Error(
      `gate: ${broken.length} hook(s) Git would not run:\n` +
        `${broken.map((problem) => `  - ${problem}\n`).join("")}\n` +
        `    Git runs a hook only if the index says ${EXECUTABLE_MODE} and the file starts with\n` +
        `    \`${SHEBANG}\`. Windows has no executable bit, so \`git add\` records ${"100644"} there\n` +
        `    and the working-tree copy looking executable is not an answer to Git.\n\n` +
        `    Run: git update-index --chmod=+x ${HOOKS_DIRECTORY}/*\n`,
    );
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    return false;
  }
}

/**
 * Asks Git which hook files this repository tracks, and what mode the index holds each at.
 *
 * `--stage 0` is what makes the answer about one entry rather than about a merge in progress, where
 * a conflicted path has an entry per stage and the modes can disagree.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<string>} NUL-separated records, as `git ls-files` printed them.
 */
function askGit(root) {
  return captureUntrimmed(root, "git", ["ls-files", "-z", "--stage", "--", HOOKS_DIRECTORY]).catch(
    (error) => {
      throw new Error(`gate: ${error.message}`);
    },
  );
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
 * 100755 6b2c... 0<TAB>.claude/git-hooks/commit-msg
 * ```
 *
 * @param {string} record One record.
 * @returns {{relative: string, mode: string}} What it says.
 */
export function parse(record) {
  const tab = record.indexOf("\t");
  if (tab === -1) {
    throw new Error(
      `gate: \`git ls-files --stage\` printed a record with no path in it: ${record}\n\n    The ` +
        `mode, object name and stage are separated from the path by a tab, and this one has none.`,
    );
  }

  const [mode] = record.slice(0, tab).split(/\s+/);

  return { relative: record.slice(tab + 1), mode };
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
