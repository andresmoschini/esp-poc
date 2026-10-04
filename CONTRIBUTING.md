# Contributing

Firmware proof of concept for the `esp32c6`, plus the tooling around it.

Two files, one job each. The **README** is for arriving: what the project is, what you need, how to
get it onto a board. This file is for changing it. The rules an agent cannot infer from the code are
in [AGENTS.md](AGENTS.md); this is the process around them.

## The gate

```sh
npm run fix      # every fixer, in the one order that works
npm run check    # the whole gate — the same thing CI runs
```

`fix` rewrites files, so read the diff. `check` is read-only. Both refuse to start until the Node
tooling matches `package-lock.json`, so a fresh clone begins with `npm run setup`.

There is **no pre-commit hook**, which is a choice rather than an omission: a hook that runs the
whole gate turns a five-second commit into a two-minute one, and a hook is the wrong place for a
check that takes minutes. The cost of that choice is that CI reports a broken commit after it exists
rather than before, so a red build costs a rewrite rather than a fix.

## Adding a step to the gate

Edit the `GATE` array in `tools/gate.mjs`. Nothing else: `.github/workflows/ci.yml` runs
`npm run check` and nothing else, so the array is the single definition of green. If the new step
can fix what it finds, add it to `FIX` as well — and read that array's comment first, because its
order is not presentation: the fixers write the same files the gate only reads.

The properties the array has to keep:

- **Every step runs even after one fails.** A gate that stops at the first problem turns one broken
  commit into several round trips.
- **A step passes or fails on its exit code alone.** Nothing matches on the text a tool prints. It
  is less strict than it could be and deliberately so — matching on output is brittle in a less
  obvious way — but it means a tool that fails while exiting 0 is a hole. If you add such a step,
  say so in its comment.
- **The reason a step exists lives in its comment.** A step with no comment is a step nobody dares
  remove.

## Bumping the toolchain

`rust-toolchain.toml` pins a dated nightly, because `-Z build-std` and `-Z stack-protector=all` in
`.cargo/config.toml` are refused by stable Cargo. Nightly moves every six weeks, so expect this:

```sh
rustup toolchain install nightly-<date>
```

Update the date in `rust-toolchain.toml` in a commit of its own, then run the full gate on a clean
`target/`. A nightly bump invalidates every host artifact — the proc macros are rebuilt — and a
stale one can fail to link rather than fail loudly, so `cargo clean` first if anything looks odd.

## Re-running esp-generate

The generator owns `Cargo.toml`, `rust-toolchain.toml`, `.cargo/config.toml`, `build.rs`, `src/`,
`wokwi.toml`, `diagram.json`, `.vscode/` and `.github/workflows/`, and it will overwrite them. Six
things have been added to those files by hand, and nothing the generator does will tell you they
were gone:

1. the `[lints]` blocks in `Cargo.toml` — removing these does not fail the gate, it makes the gate
   quieter while checking less (see below)
2. the `[workspace]` block in `Cargo.toml` and the path dependency on `crates/poc-report` — removing
   these fails loudly, at the build step, with `error[E0432]: unresolved import poc_report`
3. the exact channel, and `rustfmt`, `clippy` and `rust-src`, in `rust-toolchain.toml`
4. a crate-level `//!` doc comment in `build.rs`, `src/lib.rs` and `src/bin/main.rs`
5. the formatter settings in `.vscode/settings.json` and the extension list in
   `.vscode/extensions.json`
6. prettier's reformatting of `.vscode/*.json`, which the generator writes with trailing commas

**Delete `.github/workflows/rust_ci.yml` if it comes back.** The `-o ci` flag makes the generator
recreate it, and it builds on `stable` — which cannot build this project, because `-Z build-std` and
`-Z stack-protector=all` in `.cargo/config.toml` are refused by stable Cargo. The regenerated
workflow fails next to a passing `ci.yml` instead of replacing it, and the failure says nothing
about your change.

**Check `git diff Cargo.toml` after generating.** The `[lints]` blocks are the one item on the list
that can go missing without anything turning red: with them removed, `cargo clippy -- -D warnings`
is silent, because there are no lints left to warn. Pedantic clippy and `missing_docs` would stop
being checked on the firmware and on everything added later, and no run would mention it. A green
gate that is checking less is worse than a red one, because it is believed.

Run `npm run check` after generating and put back whatever it reports. The table in AGENTS.md is the
list.

## Commits and pull requests

- **Conventional Commits**, enforced by commitlint over the commit range in
  `.github/workflows/commitlint.yml`. `fix: ...`, `feat: ...`, `docs: ...`, `chore: ...`.
- **In a commit body, never let a colon-terminated word start a line.** commitlint reads `word:` at
  line start as a footer token, splits the message there and warns that the footer has no blank line
  before it. It is only a warning, so it lands unnoticed. Reword with an em dash, or re-wrap so the
  word sits mid-line.
- **The closing keyword goes in the pull request body, never in a commit message.** Only the body
  shows the link before the merge, a wrong number is an edit rather than a history rewrite, and no
  single commit is "the" one closing work that took several.
- **A commit should leave the gate green, and nothing stops a commit that does not.**
- **_Verified_ means what was observed, not that the gate passed.** The gate cannot see the firmware
  run — [why is in the README](README.md#a-green-gate-does-not-mean-the-firmware-works) — so a
  change that touches what the firmware does says what the board or the simulator showed. "Builds"
  is not verification, and neither is a green summary line.

One template, `.github/pull_request_template.md`, because this repository has one shape of change.
Four sections; a section with nothing to say says **"None."** rather than being deleted, because a
missing section reads as an oversight and costs the reviewer a question.

## Documentation

- **Every module carries a design note**: why it is the way it is, what was considered and rejected,
  what would break if it changed. In Rust that is the `//!` comment; in `tools/` it is the
  `Design notes` section of the module header. Reasoning behind code belongs there rather than in a
  chat transcript or a comment three lines down.
- **Write down what was measured, and re-measure it.** Several claims in this repository's own
  configuration — that `cargo test` cannot work, that `--all-targets` breaks, that a batch file
  needs CRLF — are in comments because they were checked. If a number or a claim in a comment moves,
  update it in the same commit, and prefer a sentence about the mechanism over a number that will
  not hold.
- `cspell` runs over every tracked file, and the repository is **American English only**. A British
  spelling gets changed, not added to `project-words.txt`. Add a word there only if it is a real
  domain term and no dictionary in `cspell.jsonc` covers the domain.
