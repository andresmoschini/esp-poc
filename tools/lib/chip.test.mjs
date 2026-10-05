// The tests for the chip table.
//
// Only the pure part is tested: what `chipArgs` builds, and what `defaultFeatureIsConfigured` reads
// out of a manifest and a configuration file. The table's own contents are not asserted against a
// second copy of them, because a copy is exactly the drift this file exists to remove — a second
// list of chips would agree with `CHIPS` and disagree with `Cargo.toml`, which is the problem.
//
// What *is* pinned here is the property that makes the table worth having.
// `defaultFeatureIsConfigured` is the only thing standing between a change to `CHIPS` and a gate that
// fails with a message about `target_has_atomic` or about a missing field in `Peripherals`, so the
// cases below are the ones where that check has to answer rather than the ones where it happens to
// agree today.

import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { CHIPS, chipArgs, defaultChip, defaultFeatureIsConfigured } from "./chip.mjs";

// What the two files look like when they agree with the table's first chip.
const IN_STEP = {
  manifest: '[package]\nname = "esp-poc"\n\n[features]\ndefault = ["esp32c3"]\n',
  config: '[build]\nrustflags = []\n\ntarget = "riscv32imc-unknown-none-elf"\n',
};

/**
 * Writes the two files `defaultFeatureIsConfigured` reads into a throwaway directory.
 *
 * It is a real directory rather than a fixture object, because the check reads through `node:fs` at
 * paths under the root: `.cargo/config.toml` is two levels down and `Cargo.toml` is at the top, and a
 * test that could not see that layout would not be testing the thing that runs.
 *
 * @param {{manifest: string, config: string}} files Contents to write.
 * @returns {string} The root written to, for the caller to delete.
 */
function scratch(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "esp-poc-chip-"));

  fs.mkdirSync(path.join(root, ".cargo"), { recursive: true });
  fs.writeFileSync(path.join(root, "Cargo.toml"), files.manifest, "utf8");
  fs.writeFileSync(path.join(root, ".cargo", "config.toml"), files.config, "utf8");

  return root;
}

/**
 * Runs `fn` against a scratch directory holding `files`, and removes it afterwards.
 *
 * @param {{manifest: string, config: string}} files Contents to write.
 * @param {(root: string) => void} fn What to check.
 */
function withScratch(files, fn) {
  const root = scratch(files);

  try {
    fn(root);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
}

// The chip a bare `cargo build` produces is the first entry, and the two are read from the same place
// rather than one being spelled out, so they cannot drift apart.
test("the default chip is the first one in the table", () => {
  assert.equal(defaultChip(), CHIPS[0]);
});

// The whole reason this is a table rather than a constant: every chip gets the same four arguments, so
// a chip added later cannot arrive with a different spelling of the same selection.
test("selecting a chip passes its feature and its triple together", () => {
  for (const chip of CHIPS) {
    assert.deepEqual(chipArgs(chip), [
      "--no-default-features",
      "--features",
      chip.feature,
      "--target",
      chip.triple,
    ]);
  }
});

// `--no-default-features` is what makes the non-default chip *replace* the default one rather than
// join it. Two chip features at once is a build error rather than a warning — measured, 43 errors out
// of `esp-metadata-generated` — and this argument is what avoids reaching it.
test("selecting a chip turns the default feature off", () => {
  assert.ok(chipArgs(CHIPS[1]).includes("--no-default-features"));
});

// The two triples differ only in the atomic extension, and `esp-metadata` checks the feature against
// the triple rather than either against the other. Both triples have to be real: a typo in one does
// not fail the gate, it fails on a machine that has never installed that target.
test("no two chips build for the same triple", () => {
  const triples = CHIPS.map((chip) => chip.triple);

  assert.equal(new Set(triples).size, CHIPS.length);
  assert.ok(triples.every((triple) => triple.startsWith("riscv32im")));
});

// The case the check exists for: the files agree with each other and not with the table, which is what
// happens when the first entry changes and nothing else does.
test("a triple that disagrees with the table is reported", () => {
  const config = '[build]\ntarget = "riscv32imac-unknown-none-elf"\n';

  withScratch({ ...IN_STEP, config }, (root) => {
    const problems = defaultFeatureIsConfigured(root);

    assert.equal(problems.length, 1);
    assert.match(problems[0], /config\.toml sets `target = "riscv32imac[^`]*`/u);
  });
});

// The other half. `Cargo.toml` is where a feature lives, so this is the likelier of the two to be
// forgotten: adding a chip to the table and not to `[features]` builds nothing at all.
test("a default feature that disagrees with the table is reported", () => {
  const manifest = '[features]\ndefault = ["esp32c6"]\n';

  withScratch({ ...IN_STEP, manifest }, (root) => {
    const problems = defaultFeatureIsConfigured(root);

    assert.equal(problems.length, 1);
    assert.match(problems[0], /default = \[esp32c6\]/u);
  });
});

// Both are reported. A change that moves the default usually moves both, and reporting one would send
// the operator looking for a second failure that this has already explained.
test("both files disagreeing is two reports rather than one", () => {
  const wrong = {
    manifest: '[features]\ndefault = ["esp32c6"]\n',
    config: '[build]\ntarget = "riscv32imac-unknown-none-elf"\n',
  };

  withScratch(wrong, (root) => {
    assert.equal(defaultFeatureIsConfigured(root).length, 2);
  });
});

// A feature list with more than one entry is the one case here that cannot build at all, and "the
// list contains the default chip" would wave it through. The wording names what was found so the
// operator does not have to open the file to see which entry is the wrong one.
test("a default feature list naming something else is reported by name", () => {
  const manifest = '[features]\ndefault = ["esp32c3", "something-else"]\n';

  withScratch({ ...IN_STEP, manifest }, (root) => {
    const problems = defaultFeatureIsConfigured(root);

    assert.equal(problems.length, 1);
    assert.match(problems[0], /esp32c3, something-else/u);
  });
});

// The two chips in one default list is the failure the check above exists for, and it is worth naming
// on its own because it is what "add a chip" looks like if it is done in the wrong file.
test("two chips in the default list is reported", () => {
  const manifest = '[features]\ndefault = ["esp32c3", "esp32c6"]\n';

  withScratch({ ...IN_STEP, manifest }, (root) => {
    assert.equal(defaultFeatureIsConfigured(root).length, 1);
  });
});

// Whitespace inside the list is not a disagreement. A manifest written as `default = [ "esp32c3" ]` is
// the same manifest, and reporting it would teach the operator to ignore this check.
test("whitespace inside the list is not a disagreement", () => {
  const manifest = '[features]\ndefault = [ "esp32c3" ]\n';

  withScratch({ ...IN_STEP, manifest }, (root) => {
    assert.deepEqual(defaultFeatureIsConfigured(root), []);
  });
});

// A missing file is reported as a disagreement rather than thrown. A gate step has one shape of
// failure — a list of names and a non-zero exit — and an exception is not that shape; `read` returns
// the empty string so that a missing file reads as "the key is not there", which is what it is.
test("a missing file is reported rather than thrown", () => {
  withScratch(IN_STEP, (root) => {
    fs.rmSync(path.join(root, "Cargo.toml"), { force: true });
    fs.rmSync(path.join(root, ".cargo", "config.toml"), { force: true });

    const problems = defaultFeatureIsConfigured(root);

    assert.equal(problems.length, 2);
    assert.ok(problems.every((problem) => problem.includes("no `")));
  });
});

// Only the `default` key is read out of the manifest, and `target` only out of the configuration file.
// Both files carry more of the same syntax — `features` on dependencies, `[target.<triple>]` tables —
// and reading the wrong key is how a parser starts passing cases it should not.
test("a triple named somewhere other than [build] is not taken for the default", () => {
  const config =
    "[target.riscv32imc-unknown-none-elf]\n" +
    'runner = "espflash flash --chip esp32c3"\n' +
    "\n" +
    "[build]\n" +
    'target = "riscv32imac-unknown-none-elf"\n';

  withScratch({ ...IN_STEP, config }, (root) => {
    const problems = defaultFeatureIsConfigured(root);

    assert.equal(problems.length, 1);
    assert.match(problems[0], /riscv32imac/u);
  });
});

// The check is a no-op on the files as they are committed. This is the assertion that keeps it honest:
// without it, a parser that had quietly stopped matching would still pass every disagreement case above
// and would report nothing at all, which is the failure mode a guard against drift cannot have.
test("the files as committed agree with the table", () => {
  assert.deepEqual(defaultFeatureIsConfigured(process.cwd()), []);
});
