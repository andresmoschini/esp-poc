// Running an npm-installed tool without going through a shell.
//
// Every tool the gate needs is a Node program, and every one of them publishes its entry point in
// its own `package.json` under `bin`. Resolving that field and running it with the same Node
// process that is running the gate is both simpler and more portable than the route npm sets up for
// humans: `node_modules/.bin/<tool>` is a symlink on Linux and macOS but a `.cmd` shim on Windows,
// and a `.cmd` cannot be spawned without a shell.
//
// ## Design notes
//
// **Not `node_modules/.bin`.** Node refuses to spawn a `.cmd` without `shell: true` — it changed in
// the 2024 fix for CVE-2024-27980 — and `shell: true` joins the command line without quoting, so a
// repository checked out under a path holding a space breaks. The `bin` field is package metadata
// rather than a filename convention, so it reads the same on every platform and needs no
// platform-specific spelling at all.
//
// **No `PATH` lookup either.** The gate must run the version `package-lock.json` pinned, and the one
// npm put in this tree is it. Reaching for `PATH` could quietly run a different one.
//
// **A missing entry point is reported, not skipped.** A tool this gate names is a tool the gate
// needs; if its `bin` field cannot be read, the answer is that `setup` has not been run, not that the
// step is quietly satisfied.

import fs from "node:fs";
import path from "node:path";

/**
 * The JavaScript entry point `packageName` publishes, relative to the workspace root.
 *
 * `bin` is a string in some packages and a map in others, and where it is a map the key is the
 * command name rather than always the package name — `editorconfig-checker` publishes two, `ec` and
 * `editorconfig-checker`. The name the step asked for is what is looked up, and there is deliberately
 * no fallback to "the only entry there is": a step that named `editorconfig-checker` and got `ec`
 * would be a step whose meaning depends on what its package happened to publish, which is the drift
 * this function exists to remove.
 *
 * @param {string} root Workspace root.
 * @param {string} packageName npm package, which is also the command name the step uses.
 * @returns {string} Absolute path to the entry point.
 * @throws {Error} When the package is not installed, or publishes no entry point under that name.
 */
export function toolEntryPoint(root, packageName) {
  const manifestPath = path.join(root, "node_modules", packageName, "package.json");

  let manifest;
  try {
    manifest = JSON.parse(fs.readFileSync(manifestPath, "utf8"));
  } catch (error) {
    throw new Error(`could not read ${manifestPath}: ${error.message}`);
  }

  const { bin } = manifest;
  const declared = typeof bin === "string" ? bin : bin?.[packageName];

  if (declared === undefined) {
    throw new Error(`${packageName} publishes no \`bin\` entry point named \`${packageName}\``);
  }

  return path.resolve(path.dirname(manifestPath), declared);
}
