// The tests for reading the host triple.
//
// Only the parsing is tested, against the output rather than against the installed toolchain: a test
// that shells out to `rustc` passes on the machine it runs on and proves nothing about the parsing,
// which is the half that can be wrong. A triple that fails to parse takes the whole gate with it,
// and CI runs on a different triple than the machine the gate was written on.

import assert from "node:assert/strict";
import test from "node:test";

import { parseHostTriple } from "./host.mjs";

// What `rustc -vV` prints on this toolchain, with the parts this does not read left in.
const VERSION = [
  "rustc 1.99.0-nightly (14a2b1c6f 2026-09-15)",
  "binary: rustc",
  "commit-hash: 14a2b1c6f9d1e2a3b4c5d6e7f80910203040506",
  "commit-date: 2026-09-15",
  "host: x86_64-pc-windows-msvc",
  "release: 1.99.0-nightly",
  "LLVM version: 21.1.4",
].join("\n");

// The triple is read out of the middle of the output, where `host:` sits between two lines that also
// carry a colon, and it is not the first, the last, or the whole line.
test("the host triple is read out of what rustc prints", () => {
  assert.equal(parseHostTriple(VERSION), "x86_64-pc-windows-msvc");
});

// A different machine answers with a different triple, which is the entire reason this is parsed
// rather than written down. The Linux runner is the case that breaks a hard-coded triple.
test("another machine's triple is read the same way", () => {
  const linux = VERSION.replace("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu");

  assert.equal(parseHostTriple(linux), "x86_64-unknown-linux-gnu");
});

// The line is read by its prefix, not by its position: a future `rustc` that prints another line
// first has not broken this.
test("the line is found wherever it is", () => {
  assert.equal(parseHostTriple(`host: aarch64-apple-darwin\n${VERSION}`), "aarch64-apple-darwin");
});

// Output with no `host:` line is reported rather than answered with a guess. The caller builds a
// `--target` out of this, and a wrong triple fails in a way that reads like a build problem.
test("output with no host line is reported", () => {
  assert.throws(() => parseHostTriple("rustc 1.99.0-nightly\nrelease: 1.99.0"), /no `host:`/u);
});

// A line that mentions a host without being the key is not a match. `hostname` is the near-miss worth
// pinning down, because a prefix match would take it for the answer.
test("a line whose key merely mentions a host is not one", () => {
  assert.throws(() => parseHostTriple("hostname: build.example"), /no `host:`/u);
});
