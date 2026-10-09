// Rewriting the line endings of every file Git would stage.
//
// `.gitattributes` sets one rule for everything Git considers text — `* text=auto eol=lf` — and
// `core.safecrlf` is on, so a file written with CRLF is one `git add` refuses rather than converts.
// Any tool or editor that writes CRLF on Windows puts those two rules in conflict, and Git is the
// only party that knows both of them.
//
// ## Design notes
//
// **Git answers both questions this step has to ask.** `git ls-files --eol` reports the endings in
// the worktree copy of every tracked and every untracked-but-not-ignored file, and the attributes
// `.gitattributes` gives it. Whether those bytes are text at all is Git's own decision, spelled
// `-text`; which files are wanted in CRLF is `*.bat` and `*.cmd` and nothing else. A list of
// extensions written here would be a second thing to keep in step with the one Git enforces, and
// keeping it in step is exactly the work this step exists to remove.
//
// **The decision is made from what Git reported, not from what was read.** A copy reported as `crlf`
// or `mixed`, whose attributes do not ask for CRLF, is rewritten; the run after that reports it `lf`
// and finds nothing to do. Idempotence is a consequence of the decision rather than something a
// test has to notice.

import fs from "node:fs";
import path from "node:path";

import { captureUntrimmed, reportFailure } from "./process.mjs";

// The pair of bytes a CRLF line ending is made of.
const CRLF = Buffer.from("\r\n");

/**
 * `check`'s step: reports every worktree copy that carries CRLF the repository does not ask for, and
 * rewrites nothing.
 *
 * It is the same decision `fix` makes, asked without the writing, and it exists because `fix` alone
 * leaves a hole: a fixer no check verifies will happily rewrite files on a schedule nothing looks at
 * the result of. The line endings are the one property in this repository that two configurations
 * disagree about — `.gitattributes`, matched by attribute, and `.editorconfig`, matched by glob — and
 * `git add` is what obeys the first one. An `editorconfig` that said LF is not an answer about what
 * Git will accept.
 *
 * @param {string} root Workspace root.
 * @returns {boolean} Whether the step passed.
 */
export function check(root) {
  return reportFailure(verify(root));
}

/**
 * Reads what Git reports and throws when any of it needs rewriting.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<void>}
 */
async function verify(root) {
  const records = await askGit(root);
  const wrong = offenders(records);

  if (wrong.length === 0) {
    console.log("every file Git would stage already ends its lines the way .gitattributes asks");
    return;
  }

  const listed = wrong.map((file) => `  ${file.relative} (${file.endings})`).join("\n");

  throw new Error(
    `gate: ${wrong.length} file(s) end their lines in a way .gitattributes does not ask for:\n` +
      `${listed}\n\n    Run \`npm run fix\` to rewrite them.`,
  );
}

/**
 * The records that need rewriting before `git add` will take them.
 *
 * The decision needs nothing but what Git printed, so this is a function of a string rather than of
 * the filesystem — which is what lets it be tested against records nobody had to damage a repository
 * to produce.
 *
 * @param {string} records NUL-separated records, as `git ls-files --eol` printed them.
 * @returns {Array<{relative: string, endings: string}>} What has to be rewritten.
 */
export function offenders(records) {
  const wrong = [];

  for (const record of records.split("\0").filter((record) => record !== "")) {
    const file = parse(record);
    if (needsRewrite(file.endings, file.wantsCrlf)) {
      wrong.push({ relative: file.relative, endings: file.endings });
    }
  }

  return wrong;
}

/**
 * `fix`'s step: rewrites every worktree copy that carries CRLF the repository does not ask for.
 *
 * It sits beside `editorconfig` rather than inside it, and last rather than first.
 * `editorconfig-checker` answers from `.editorconfig`, matched by glob; this step answers from
 * `.gitattributes`, matched by attribute, and `git add` is what obeys the second one. The two
 * configurations can disagree, and when they do it is `git add` that breaks rather than a formatter.
 * `*.bat` and `*.cmd` are the live case: both configurations agree they are CRLF on purpose, and
 * only Git's answer is asked. And every step before this one writes, so the ending of a line is the
 * last thing a byte should be decided on.
 *
 * @param {string} root Workspace root.
 * @returns {boolean} Whether the step passed.
 */
export function fix(root) {
  return reportFailure(rewrite(root));
}

/**
 * Rewrites every worktree copy that needs it, and reports which those were.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<void>}
 */
async function rewrite(root) {
  const records = await askGit(root);
  const rewritten = [];

  for (const record of records.split("\0").filter((record) => record !== "")) {
    const file = parse(record);
    if (!needsRewrite(file.endings, file.wantsCrlf)) {
      continue;
    }
    const lines = rewriteOne(root, file);
    if (lines !== null) {
      rewritten.push(`  ${file.relative}: ${lines} line(s) now end in LF`);
    }
  }

  if (rewritten.length === 0) {
    console.log("every file Git would stage already ends its lines the way .gitattributes asks");
    return;
  }

  for (const line of rewritten) {
    console.log(line);
  }
  console.log(`${rewritten.length} file(s) rewritten`);
}

/**
 * Asks Git for every file it would stage, and what it found in each worktree copy.
 *
 * `--cached --others --exclude-standard` is the tracked files plus the new ones that are not
 * ignored: the set `editorconfig-checker` and `cspell` read, so a file this step never sees is a
 * file no step in the gate has an opinion about either.
 *
 * @param {string} root Workspace root.
 * @returns {Promise<string>} NUL-separated records, as `git ls-files` printed them.
 */
function askGit(root) {
  return captureUntrimmed(root, "git", [
    "ls-files",
    "-z",
    "--eol",
    "--cached",
    "--others",
    "--exclude-standard",
  ]).catch((error) => {
    throw new Error(`gate: ${error.message}`);
  });
}

/**
 * Reads one NUL-separated record of `git ls-files --eol` output.
 *
 * A record is three space-padded fields, a tab, and the path, and `-z` is what keeps a path holding
 * a space, a quote or a newline intact between the two:
 *
 * ```text
 * i/lf    w/crlf  attr/text=auto eol=lf<TAB>docs/integration.json
 * ```
 *
 * @param {string} record One record.
 * @returns {{relative: string, endings: string, wantsCrlf: boolean}} What it says.
 */
export function parse(record) {
  const tab = record.indexOf("\t");
  if (tab === -1) {
    throw new Error(
      `gate: \`git ls-files --eol\` printed a record with no path in it: ${record}\n\n    The ` +
        `fields and the path are separated by a tab, and this one has none.`,
    );
  }

  const fields = record.slice(0, tab);
  const relative = record.slice(tab + 1);

  // The worktree field is the one that decides anything, and it is found by its own prefix rather
  // than by position: the index field is empty for a file nothing has staged yet, and the attribute
  // field holds however many attributes the file has.
  let endings = "";
  let wantsCrlf = false;

  for (const field of fields.split(/\s+/).filter(Boolean)) {
    if (field.startsWith("w/")) {
      endings = field.slice("w/".length);
    } else if (field === "eol=crlf") {
      wantsCrlf = true;
    }
  }

  return { relative, endings, wantsCrlf };
}

/**
 * Whether a worktree copy has to be rewritten to the endings this repository asks for.
 *
 * Git spells the endings `lf`, `crlf`, `mixed`, `none` or `-text`, and `-text` is its answer to "are
 * these bytes text", so a file it calls binary is never rewritten and nothing here has to decide
 * that again. `crlf` and `mixed` are the two that carry CRLF; a mixed file has no one ending left to
 * keep, and under `eol=lf` the only right answer is LF throughout.
 *
 * @param {string} endings What Git reported for the worktree copy.
 * @param {boolean} wantsCrlf Whether `.gitattributes` asks for CRLF for this file.
 * @returns {boolean} Whether it must be rewritten.
 */
export function needsRewrite(endings, wantsCrlf) {
  return (endings === "crlf" || endings === "mixed") && !wantsCrlf;
}

/**
 * Rewrites one worktree copy, answering how many lines it changed, or `null` when there was nothing
 * in it to change.
 *
 * @param {string} root Workspace root.
 * @param {{relative: string}} file What Git reported about it.
 * @returns {number | null} Lines changed, or `null` when there was nothing to change.
 */
function rewriteOne(root, file) {
  const filePath = path.join(root, file.relative);

  // A file the worktree no longer has is reported with the endings in the index, and staging the
  // deletion is not this step's job.
  let bytes;
  try {
    bytes = fs.readFileSync(filePath);
  } catch (error) {
    if (error.code === "ENOENT") {
      return null;
    }
    throw new Error(`gate: could not read ${file.relative}: ${error.message}`);
  }

  const [fixed, lines] = toLf(bytes);
  if (lines === 0) {
    return null;
  }

  try {
    fs.writeFileSync(filePath, fixed);
  } catch (error) {
    throw new Error(`gate: could not write ${file.relative}: ${error.message}`);
  }

  return lines;
}

/**
 * Replaces every CRLF with a bare LF, answering how many there were.
 *
 * A lone CR is left where it is: nothing here produces one, `.gitattributes` has no rule for one,
 * and a file carrying one is a file whose bytes belong to something this step cannot see.
 *
 * @param {Buffer} bytes Contents of the file as they are.
 * @returns {[Buffer, number]} The rewritten contents and how many line endings changed.
 */
export function toLf(bytes) {
  const fixed = Buffer.alloc(bytes.length);
  let index = 0;
  let written = 0;
  let lines = 0;

  while (index < bytes.length) {
    if (bytes.subarray(index, index + CRLF.length).equals(CRLF)) {
      fixed[written] = 0x0a;
      written += 1;
      index += CRLF.length;
      lines += 1;
    } else {
      fixed[written] = bytes[index];
      written += 1;
      index += 1;
    }
  }

  return [fixed.subarray(0, written), lines];
}
