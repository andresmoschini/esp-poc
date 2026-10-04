// Whether the installed Node tooling can be used, which is the question `check` and `fix` both ask
// before they do anything else.
//
// It lives apart from `gate.mjs` because it is the one decision in there that reads the filesystem
// and answers something, and both commands refuse to start on it: a gate that ran half its steps
// would print a passing summary for half a gate, which is worse than refusing.

import fs from "node:fs";
import path from "node:path";

/**
 * Where `setup` records the lockfile it installed from.
 *
 * It lives inside `node_modules` so that it shares that directory's lifetime: `npm ci` deletes the
 * tree before reinstalling, and this record goes with it.
 *
 * @param {string} root Workspace root.
 * @returns {string} Absolute path to the record.
 */
export function installedLockfile(root) {
  return path.join(root, "node_modules", ".esp-poc-installed-lockfile.json");
}

/**
 * Checks that the Node tooling is installed and matches `package-lock.json`.
 *
 * The comparison is by content, not by timestamp. Git rewrites `package-lock.json` on checkout even
 * when its content is identical, so comparing modification times reported a stale tree after every
 * branch switch and every step of a rebase — a false alarm that costs a reinstall to clear and
 * teaches people to ignore the message. Content answers the question actually being asked, and
 * reading two files of about 130 KB costs nothing measurable next to the checks that follow.
 *
 * @param {string} root Workspace root.
 * @returns {string | undefined} What is wrong, or `undefined` when the tooling is usable.
 */
export function nodeToolingState(root) {
  const lockfile = path.join(root, "package-lock.json");
  const installed = installedLockfile(root);

  if (!fs.existsSync(installed)) {
    return `gate: the Node tooling is not installed.\n\n    ${installed} does not exist.`;
  }

  const wanted = readFile(lockfile);
  const have = readFile(installed);

  if (wanted !== undefined && have !== undefined && wanted.equals(have)) {
    return undefined;
  }

  return (
    `gate: the installed Node tooling does not match the lockfile.\n\n    ${lockfile} has changed ` +
    `since it was installed.`
  );
}

/**
 * Contents of `filePath`, or `undefined` when it could not be read.
 *
 * @param {string} filePath File to read.
 * @returns {Buffer | undefined} Its contents.
 */
function readFile(filePath) {
  try {
    return fs.readFileSync(filePath);
  } catch {
    return undefined;
  }
}
