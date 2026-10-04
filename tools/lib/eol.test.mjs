// The tests for `eol`, which is the part of the gate that makes a decision rather than reporting
// someone else's.
//
// They are written against the parsing and rewriting directly, with the file Git reports fabricated,
// because that is the half that can be wrong in a way nobody notices: a record read by position
// instead of by prefix stops working the moment Git reports a field this code has not seen.

import assert from "node:assert/strict";
import test from "node:test";

import { needsRewrite, offenders, parse, toLf } from "./eol.mjs";

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

// What the `check` step reports is what the `fix` step rewrites, and both decide from the same
// function: a record named here and not rewritten there would be a check that passes on a repository
// `fix` is still changing, which is the gap the check was added to close.
const lf = "i/lf    w/lf    attr/text=auto eol=lf\tsrc/wifi.rs";
const crlf = "i/lf    w/crlf  attr/text=auto eol=lf\tsrc/bin/main.rs";
const mixed = "i/lf    w/mixed attr/text=auto eol=lf\tREADME.md";
const batch = "i/crlf  w/crlf  attr/text eol=crlf\ttools/build.cmd";
const binary = "i/none  w/-text attr/\tfirmware.elf";

test("a copy carrying CRLF is one the check reports", () => {
  assert.deepEqual(offenders(crlf), [{ relative: "src/bin/main.rs", endings: "crlf" }]);
});

// A mixed copy has no one ending left to keep, so it is reported under the endings Git gave it
// rather than being passed over as "not CRLF".
test("a copy carrying both endings is reported as mixed", () => {
  assert.deepEqual(offenders(mixed), [{ relative: "README.md", endings: "mixed" }]);
});

// The batch file is CRLF on purpose, and `.gitattributes` is what says so. Reporting it would make
// the step fail on the one file whose endings are correct.
test("a copy .gitattributes wants in CRLF is not reported", () => {
  assert.deepEqual(offenders(batch), []);
});

// A file already correct, and one Git calls binary, are both left out: the first needs nothing and the
// second is not a decision this code gets to make.
test("a correct copy and a binary one are not reported", () => {
  assert.deepEqual(offenders(`${lf}\0${binary}`), []);
});

// Records arrive NUL-separated and the paths among them are what the operator is given, so only the
// offending ones are listed and in the order Git reported them.
test("only the offending records are reported, in order", () => {
  const records = [lf, crlf, batch, mixed, binary].join("\0");

  assert.deepEqual(
    offenders(records).map((file) => file.relative),
    ["src/bin/main.rs", "README.md"],
  );
});

// Git reports nothing at all for a repository with no files, which is a clean answer rather than a
// missing one.
test("no records is nothing to report", () => {
  assert.deepEqual(offenders(""), []);
  assert.deepEqual(offenders("\0"), []);
});
