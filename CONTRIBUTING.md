# Contributing

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

The `pre-commit` hook runs `npm run check`, and the `commit-msg` hook checks the message against
commitlint. Both are in `.claude/git-hooks/`; [The hooks](#the-hooks) says how they get installed,
what they cost, and how to tell that Git is not running them at all.

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

## Adding a chip, or a step that depends on the chip

The chip is a Cargo feature (`[features]` in `Cargo.toml`), so a step that compiles the firmware has
to compile it once per chip or it checks the default one and calls that green. The three `cargo`
steps do this by being `forEachChip(...)` steps, which expand to one command per entry in
`tools/lib/chip.mjs`. Two things to know before writing a fourth:

- Chip arguments go **before** a bare `--` on a `cargo clippy` line. Everything after it is the
  compiler's, and `--no-default-features` there is `error: Unrecognized option` from clippy-driver.
- The chip and its target triple are **two separate things** and Cargo will not connect them. Build
  both with `chipArgs(chip)` rather than spelling them out; `tools/gate.test.mjs` fails a step that
  names a triple after a `--`, and one that builds a chip without both of its arguments.

`[build] target` in `.cargo/config.toml` and `default` in `Cargo.toml` are the two places the
default chip is restated outside that table, and nothing in Cargo compares them. The gate's `chips`
step is what does, which is why it runs first: it is the only step that can report "this repository
is set up for a chip it has no features for". Change `target` in `.cargo/config.toml` and `default`
in `Cargo.toml` together, and add the chip to `tools/lib/chip.mjs` in the same commit.

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
`wokwi.toml`, `diagram.json`, `.vscode/` and `.github/workflows/`, and it will overwrite them. Eight
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
7. the `[features]` block in `Cargo.toml`, and the chip's features taken off each dependency's own
   `features` list — see below
8. the second chip's `runner` table in `.cargo/config.toml`, the second target in
   `rust-toolchain.toml`, and `wokwi.c6.toml` / `diagram.c6.json`, which the generator has no
   opinion about because it only ever emits one chip

**Delete `.github/workflows/rust_ci.yml` if it comes back.** The `-o ci` flag makes the generator
recreate it, and it builds on `stable` — which cannot build this project, because `-Z build-std` and
`-Z stack-protector=all` in `.cargo/config.toml` are refused by stable Cargo. The regenerated
workflow fails next to a passing `ci.yml` instead of replacing it, and the failure says nothing
about your change.

**Check `git diff Cargo.toml` after generating.** Two of the items on the list are there. The
`[lints]` blocks are the one that can go missing without anything turning red: with them removed,
`cargo clippy -- -D warnings` is silent, because there are no lints left to warn. Pedantic clippy
and `missing_docs` would stop being checked on the firmware and on everything added later, and no
run would mention it. A green gate that is checking less is worse than a red one, because it is
believed. The `[features]` block goes the other way and is loud, at the build step, with a message
about a chip's features rather than about a manifest — the generator puts one chip inline on each
dependency and the other chip stops existing.

**Regenerating for a chip you already support is the wrong way to reach it.** esp-generate writes
one chip's features inline and one chip's reserved pins, so regenerating to move between two chips
this repository already builds means taking two things away as well as one thing back. Change
`[features]` and the `#[cfg]`'d block in `src/bin/main.rs` by hand; the gate builds every chip, so
it will say if one of them no longer compiles.

Run `npm run check` after generating and put back whatever it reports. The table in AGENTS.md is the
list.

## Commits and pull requests

- **Conventional Commits**, enforced twice: by the `commit-msg` hook before the commit exists, and
  by commitlint over the commit range in `.github/workflows/commitlint.yml`. `fix: ...`,
  `feat: ...`, `docs: ...`, `chore: ...`.
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

## The hooks

`pre-commit` runs the gate. `commit-msg` checks the message with commitlint and stamps which agent
session produced the commit. Both live in `.claude/git-hooks/`.

**They only run if they were installed, and a session is what installs them.** A commit made from a
plain terminal in a clone where no session has opened runs no hooks at all, and nothing says so —
the commit simply succeeds. That is deliberate: CI runs the same gate on every push and pull
request, so the boundary that actually holds is there, and the hooks are in front of it. Check yours
with `git config core.hooksPath`.

**The pre-commit hook runs the whole gate, which makes a commit the slowest thing in this
repository, and that is the price of learning about a broken commit before it exists rather than
after.** `build-std` in `.cargo/config.toml` rebuilds `core` for the target, so the gate compiles
the firmware for two chips in two profiles, and a commit is measured in minutes. Two things keep
that short: `npm run fix` first, which is what brings a commit near a minute rather than three, and
committing less often.

**The pre-commit hook checks your working tree, not what you staged.** With unstaged changes
present, or after `git add -p`, it verifies files that are not the ones being committed, so a commit
can pass and still be broken. Stashing to close that gap risks losing work if the hook is
interrupted, which is the worse failure. If you stage selectively, run `npm run check` on a clean
tree before trusting it.

**`git commit --no-verify` is never used.** A bypassed gate is worse than no gate, because the log
then claims a green history that was never checked, and on a gate this slow the temptation is the
real cost. When the hook fails, fix the cause or stop and report.

**A hook Git will not run is the failure this repository is best placed to hide.** Git skips a hook
that is not executable in the index, or that names no interpreter, and says nothing when it does.
Windows produces that routinely: `git add` records `100644`, because there is no executable bit to
read off the filesystem, while the working-tree copy still looks executable — so `ls` and Git
disagree and only Git is the one that matters. The gate's `hooks` step asks Git rather than the
filesystem, so this is a red step rather than a mystery. A hook that is newly added or re-added
needs

```sh
git update-index --chmod=+x .claude/git-hooks/*
```

once, and the reason it is not a step in `FIX` is in `tools/lib/hooks.mjs`: it writes to the index
rather than to a file, and a fixer that staged would change what a commit is about to contain.

### The session trailer

A commit made from an agent session can carry up to three trailers, and each is a different handle
on the same conversation rather than the same one twice.

- **`Claude-Resume`** holds a Claude Code session's local id. Reopen the conversation with
  `claude --resume <id>`. The `commit-msg` hook writes it.
- **`Claude-Session`** holds a URL that opens a Claude Code session in a browser. Claude Code writes
  it itself when Remote Control is enabled, which is a setting outside this repository — so it is
  present sometimes and absent otherwise.
- **`OpenCode-Session`** holds an OpenCode session's id, stamped by the same hook from the value
  `.opencode/plugins/session-trailer.js` injects as `ESP_POC_SESSION_ID`.

**They are separate keys on purpose because neither client's id resumes the other**, and one key
whose shape depends on the client would make a commit's provenance unreadable after the fact. List
them with:

```sh
git log --format='%h %(trailers:key=Claude-Resume,valueonly)'
```

It is a convenience, not a record. Transcripts live outside the repository and do not survive a new
machine, so the reasoning that matters belongs in the commit body or in the pull request. **If a
commit body only makes sense with the transcript open, the body is wrong.**

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
