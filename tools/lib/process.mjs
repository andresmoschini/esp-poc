// The subprocess wrappers the repository's automation shares.
//
// `setup` drives `npm` and `eol` drives `git`, and both want the same two shapes: a command whose
// progress the operator watches, and a command whose output is read back. Keeping them here is what
// lets the logic that decides *what* to run stay in plain functions that take values and return
// `Result`, in the module that owns the decision.

import { spawn } from "node:child_process";

/**
 * Runs `program` with `args` from `cwd`, inheriting the child's stdio so its progress streams to the
 * operator as it happens. Used for the gate steps, which are worth watching live.
 *
 * @param {string} root Workspace root, which is where the child runs unless `cwd` says otherwise.
 * @param {string} program Executable name, resolved through `PATH`.
 * @param {string[]} args Arguments passed to `program`.
 * @param {string} [cwd] Directory to run from, for the one step that must not be run from the root.
 * @returns {Promise<boolean>} Whether it exited successfully.
 */
export function runVisible(root, program, args, cwd = root) {
  return new Promise((resolve) => {
    const child = spawn(program, args, { cwd, stdio: "inherit" });

    child.on("error", (error) => {
      process.stderr.write(`gate: could not run \`${program}\`: ${error.message}\n`);
      resolve(false);
    });

    child.on("close", (code) => resolve(code === 0));
  });
}

/**
 * Runs `program` with `args` from `root` and returns its trimmed standard output.
 *
 * @param {string} root Workspace root.
 * @param {string} program Executable name, resolved through `PATH`.
 * @param {string[]} args Arguments passed to `program`.
 * @returns {string} The captured standard output, trimmed.
 */
export function capture(root, program, args) {
  return captureUntrimmed(root, program, args).then((output) => output.trim());
}

/**
 * Runs `program` with `args` from `root` and returns its standard output as it came.
 *
 * Reading a file out of `origin/main` is the one caller that needs this: trimming would drop a
 * leading blank line and shift every line number reported back to the operator by one.
 *
 * @param {string} root Workspace root.
 * @param {string} program Executable name, resolved through `PATH`.
 * @param {string[]} args Arguments passed to `program`.
 * @returns {Promise<string>} The captured standard output.
 */
export function captureUntrimmed(root, program, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
    const out = [];
    const err = [];

    child.stdout.on("data", (chunk) => out.push(chunk));
    child.stderr.on("data", (chunk) => err.push(chunk));

    child.on("error", (error) =>
      reject(new Error(`could not run \`${program}\`: ${error.message}`)),
    );

    child.on("close", (code) => {
      if (code === 0) {
        resolve(Buffer.concat(out).toString("utf8"));
      } else {
        reject(
          new Error(
            `\`${program} ${args.join(" ")}\` exited with ${code}: ${Buffer.concat(err)
              .toString("utf8")
              .trim()}`,
          ),
        );
      }
    });
  });
}
