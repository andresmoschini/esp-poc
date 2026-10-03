// The tests for the npm-tool resolver, which is the one piece of this repository's own code on the
// path of every gate step that is not a `cargo` one.
//
// Both shapes of `bin` are covered because both are real: prettier publishes a string and the rest
// publish a map, and a resolver written for only one of them fails on a step it silently never ran.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { toolEntryPoint } from "./npm-tool.mjs";

/**
 * Builds a throwaway tree holding one fake installed package, and returns the workspace root holding
 * it.
 *
 * The resolver reads only `package.json`, so the entry point never has to exist: what is under test
 * is where the resolver decides to run Node, not what the program there does.
 *
 * @param {string} manifest What to write as the package's `package.json`.
 * @returns {string} The workspace root, with `node_modules/tool/package.json` written into it.
 */
function fixture(manifest) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "gate-fixtures-"));
  const packageRoot = path.join(root, "node_modules", "tool");

  fs.mkdirSync(packageRoot, { recursive: true });
  fs.writeFileSync(path.join(packageRoot, "package.json"), JSON.stringify(manifest));

  return root;
}

// A package that publishes a single command under a name of its own still resolves, because `bin` is
// a string and there is nothing to look a key up in. This is prettier's shape.
test("a bin published as a string resolves", () => {
  const root = fixture({ name: "tool", bin: "./bin/tool.cjs" });

  assert.equal(
    toolEntryPoint(root, "tool"),
    path.join(root, "node_modules", "tool", "bin", "tool.cjs"),
  );
});

// A package that publishes several commands resolves the one that was asked for, which is the case
// `editorconfig-checker` is: it publishes `ec` and `editorconfig-checker` from one entry point.
test("a bin published as a map resolves the requested command", () => {
  const root = fixture({ name: "tool", bin: { ec: "./dist/index.js", tool: "./dist/index.js" } });

  assert.equal(
    toolEntryPoint(root, "tool"),
    path.join(root, "node_modules", "tool", "dist", "index.js"),
  );
});

// A command name the package does not publish is an error rather than a guess, because silently
// running a different command than the step names is how a step stops meaning anything. `bin` holding
// exactly one entry is not treated as agreement: that entry may be named for something else.
test("a command the package does not publish is reported", () => {
  const root = fixture({ name: "tool", bin: { other: "./dist/index.js" } });

  assert.throws(() => toolEntryPoint(root, "tool"), /no `bin` entry point named `tool`/u);
});

// A package that is not installed at all is an error too, and names the file that was missing so the
// answer is `npm run setup` rather than a reinstall of something unrelated.
test("a package that is not installed is reported", () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "gate-fixtures-"));

  assert.throws(() => toolEntryPoint(root, "tool"), /could not read/u);
});
