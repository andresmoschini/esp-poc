// Installs this repository's git hooks, which nothing but a session would otherwise install.
// `.claude/settings.json` does the same through a `SessionStart` entry; this is the same
// arrangement for OpenCode, and it exists for a reason worth stating: a commit made with the hooks
// absent runs no gate and says nothing, which is worse than a hook that fails loudly.
//
// OpenCode V2 shape: a default export carrying an `id`, with `setup` holding the work. The V1
// function-export shape this replaced did not run at all on V2.
//
// The two flags on the command are load-bearing. `--local` and `-C` together mean a session whose
// working directory is not a checkout writes nothing at all, where a bare `git config` outside a
// repository would set the global config and change every other repository on the machine.
//
// The log line is deliberate, not decoration. Nothing in the gate can execute this file, so a
// session that starts has exactly one chance to say whether it loaded. It reaches the TUI and not
// `~/.local/share/opencode/log/opencode.log`: measured across every session on this machine, that
// file has never held a `[esp-poc]` line — not in a session where this file loaded, and not in
// one where it died — because `console.log` inside a plugin is not the log. What that file does
// hold is the loader's own `loading plugin` and `failed to load plugin` entries, and the second
// one carries the cause, which is where a dead import is legible. Its absence here is not evidence
// either way, and treating it as evidence is what sent the search after the API instead.
//
// There is no `import { Plugin } from "@opencode/plugin"` here, and its absence is deliberate.
// OpenCode 2.0.16 requires that package to be resolvable from this file's directory, and nothing in
// the tree provides it: `.opencode/plugins/` sits above a `package.json` that never listed it, so
// every session start died at module resolution with `Cannot find package '@opencode/plugin'`
// before `setup` was entered. The loader's own contract, from the message it prints when a plugin
// does not fit — "Plugin must export a default definition with an id and an effect or setup
// function" — is a default export and nothing else, and `Plugin.define` in that package is
// `define(plugin) { return plugin }`. The import bought type-checking that no step in the gate
// performs, and cost the whole plugin.

const HOOKS_PATH = ".claude/git-hooks";

export default {
  id: "esp-poc.git-hooks",
  async setup(ctx) {
    const directory = ctx.location.directory;
    try {
      const child = Bun.spawn(
        ["git", "-C", directory, "config", "--local", "core.hooksPath", HOOKS_PATH],
        {
          stdout: "ignore",
          stderr: "pipe",
        },
      );
      const code = await child.exited;
      if (code !== 0) {
        throw new Error(await new Response(child.stderr).text());
      }
      console.log(`[esp-poc] git hooks installed: core.hooksPath=${HOOKS_PATH} in ${directory}`);
    } catch (error) {
      // Logged and swallowed on purpose: a hook installer that can stop a session is worse than
      // the gap it closes. The boundary that actually holds is CI.
      console.warn(
        `[esp-poc] could not set core.hooksPath; commits from this session run no gate.\n` +
          `  ${error}\n  Run: git config core.hooksPath ${HOOKS_PATH}`,
      );
    }
  },
};
