<!-- markdownlint-configure-file { "MD041": false } -->
<!-- A pull request body is not a document: its title is the pull request's, and a top-level heading
     here renders oversized on GitHub. Every other rule still applies. -->

<!--
  One template, because this repository has one shape of change: proof of concept code for one chip,
  plus the tooling around it. The sections below are the two questions a reviewer of a device that
  cannot be attached to a machine actually has.

  Before opening: `npm run fix`, then `npm run check`. A red gate costs a rewrite; `fix` is cheap.

  Every section stays. A section with nothing to say says "None." — a missing section reads as an
  oversight and costs the reviewer a question, where "None." is an answer.
-->

## What changes

<!-- One or two lines. What is different after this merges. For firmware, which peripherals, timer or
     peripheral behaviour is now exercised — a diff of "Hello world" against itself tells a reviewer
     nothing. -->

## Why now

<!-- What forced it. "Proving out the toolchain" and "the board arrived" are both forcing reasons;
     "it seemed like a good use of an afternoon" is an answer too, and worth saying so it can be
     argued with. -->

## Verified

<!-- What was run, and what was observed — not what is expected to happen. Say whether the gate was
     green and, if the change touches the firmware, whether it was flashed to a device and what the
     serial monitor showed. "Builds" is not verification for a repository whose output is a chip
     blinking. For a new gate step, the run where it was made to fail on purpose, and the run where it
     was restored. -->

## Blast radius

<!-- What breaks for someone who pulls this: a dependency that moved, a generator parameter that
     changed, a toolchain pin that had to be bumped. "Nothing; it is additive." is common and is worth
     stating rather than leaving to be inferred.

     Say it explicitly when this touches a file esp-generate owns — Cargo.toml, rust-toolchain.toml,
     .cargo/config.toml, build.rs, src/, .github/ — because re-running the generator will overwrite
     those. AGENTS.md lists them and says what to put back. -->
