// The tests for `eol`, which is the part of the gate that makes a decision rather than reporting
// someone else's.
//
// They are written against the parsing and rewriting directly, with the file Git reports fabricated,
// because that is the half that can be wrong in a way nobody notices: a record read by position
// instead of by prefix stops working the moment Git reports a field this code has not seen.

import assert from "node:assert/strict";
import test from "node:test";

import { needsRewrite, parse, toLf } from "./eol.mjs";

// A tracked file whose worktree copy has been written on Windows is rewritten: Git reports `crlf`,
// and the rule asks for `lf`.
test("a CRLF file is rewritten", () => {
  assert.equal(needsRewrite("crlf", false), true);
});

// A file Git read and called binary is left alone, whatever bytes it holds.
test("a binary file is left alone", () => {
  assert.equal(needsRewrite("-text", false), false);
});

// A file already in LF has nothing to do, and neither does one with no line ending at all.
test("a file with nothing to normalize is left alone", () => {
  assert.equal(needsRewrite("lf", false), false);
  assert.equal(needsRewrite("none", false), false);
});

// `*.bat` and `*.cmd` are the one place CRLF is correct, and the Windows batch interpreter needs it.
// This is the answer this step would give without asking Git.
test("a file .gitattributes wants in CRLF is left alone", () => {
  assert.equal(needsRewrite("crlf", true), false);
  assert.equal(needsRewrite("mixed", true), false);
});

// A file carrying both endings has no one ending left to keep, and LF throughout is the only answer
// `eol=lf` allows.
test("a mixed file is rewritten", () => {
  assert.equal(needsRewrite("mixed", false), true);
});

// The record is read by its own prefixes, because the index field is empty for a file nothing has
// staged yet and the attribute field holds however many attributes there are.
test("a record is read by prefix rather than by position", () => {
  const file = parse("i/      w/mixed attr/text=auto eol=lf\tdocs/integration.json");

  assert.equal(file.relative, "docs/integration.json");
  assert.equal(file.endings, "mixed");
  assert.equal(file.wantsCrlf, false);
});

// A batch file arrives with `eol=crlf` among its attributes, and that is what saves it.
test("a batch file is recognized by its attribute", () => {
  const file = parse("i/      w/crlf  attr/text eol=crlf\ttools/build.cmd");

  assert.equal(file.endings, "crlf");
  assert.equal(file.wantsCrlf, true);
  assert.equal(needsRewrite(file.endings, file.wantsCrlf), false);
});

// A path holding a space or a tab survives `-z`, and the tab between the fields and the path is the
// first one there is.
test("a path holding a space and a tab is read whole", () => {
  const file = parse("i/lf    w/lf    attr/text=auto eol=lf\ta file\twith a tab.md");

  assert.equal(file.relative, "a file\twith a tab.md");
});

// A record with no tab in it is Git's format having changed under this step, and it is reported
// rather than skipped: a record nobody reads is a file nobody fixes.
test("a record with no path in it is reported", () => {
  assert.throws(
    () => parse("i/lf w/lf attr/text=auto eol=lf"),
    /no path/u,
    "a record with no tab in it should be reported",
  );
});

// The whole content is replaced, and the count is what a report prints.
test("every CRLF becomes a bare LF", () => {
  const [fixed, lines] = toLf(Buffer.from("alpha\r\nbeta\r\n"));

  assert.deepEqual(fixed, Buffer.from("alpha\nbeta\n"));
  assert.equal(lines, 2);
});

// A file with nothing to replace comes back with the same bytes, which is what makes a second run
// find no work.
test("a file with no CRLF comes back unchanged", () => {
  const [fixed, lines] = toLf(Buffer.from("alpha\nbeta\n"));

  assert.deepEqual(fixed, Buffer.from("alpha\nbeta\n"));
  assert.equal(lines, 0);
});

// A lone carriage return is left where it is: nothing this step produces one, `.gitattributes` has
// no rule for one, and a file carrying one is a file whose bytes belong to something this step cannot
// see. Two in a row followed by a newline is still one line ending, not two.
test("a lone carriage return is not a line ending", () => {
  const [fixed, lines] = toLf(Buffer.from("alpha\r\r\nbeta"));

  assert.deepEqual(fixed, Buffer.from("alpha\r\nbeta"));
  assert.equal(lines, 1);
});

// An empty file stays empty.
test("an empty file stays empty", () => {
  const [fixed, lines] = toLf(Buffer.alloc(0));

  assert.equal(fixed.length, 0);
  assert.equal(lines, 0);
});
