// Whether the module asking is the program the operator invoked.
//
// `gate.mjs` dispatches on the command line when it is run and does nothing when it is imported, and
// the tests import it to reach the `GATE` and `FIX` arrays. Getting that backwards would not be a
// small mistake: a test run would dispatch whatever command line the runner happened to pass, which
// for this gate means spawning every step of the gate from inside the test run.
//
// The comparison is on real paths because the two sides can disagree about case on Windows, and that
// failure is silent — the CLI would do nothing at all and report nothing, which reads exactly like a
// green gate that checked nothing.

import fs from "node:fs";

/**
 * Whether `invoked` is the file this process was started with.
 *
 * @param {string | undefined} invoked `process.argv[1]`, which is absent when Node runs a module
 *   without naming one on the command line.
 * @param {string} file Absolute path to the module asking.
 * @returns {boolean} Whether they are the same file.
 */
export function isEntryPoint(invoked, file) {
  if (invoked === undefined) {
    return false;
  }

  try {
    return fs.realpathSync(invoked) === fs.realpathSync(file);
  } catch {
    // Either side can name a file that is not there, and a path that does not resolve is not the
    // entry point. Reporting rather than throwing keeps a stale command line from stopping the gate.
    return false;
  }
}
