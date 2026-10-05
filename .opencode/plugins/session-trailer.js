// Carries the OpenCode session id into the commit message, the way the `commit-msg` hook already
// arranges it for Claude Code. That hook reads `CLAUDE_CODE_SESSION_ID`, which Claude Code exports into
// the environment of its own shell tools; OpenCode exports nothing of the kind, which was measured
// rather than assumed — the only `OPENCODE_*` variable in a session's environment is
// `OPENCODE_TERMINAL`. So the id is captured here and injected into the shells the agent creates.
//
// The two hooks do one job between them. `context` runs immediately before an agent model request
// and carries the session id, so the value below is the session that is about to act rather than
// whichever one started last. `shell.create.before` then puts it in the environment of every shell
// the agent spawns, and `git commit` inherits it as a child of that shell. The alternative — writing
// the id to a file and having `commit-msg` read it — is the wrong shape here for the same reason it
// is for Claude Code, and the reasoning holds unchanged: several sessions can be open on one
// checkout, and "the last id written" is not the same thing as "the one committing".
//
// Two registrations rather than one, because a plugin file that throws takes its own behaviour
// down with it: install-git-hooks.js must keep working whether or not this one loads.
//
// There is no `import { Plugin } from "@opencode/plugin"` here, for the reason
// install-git-hooks.js gives at length: on 2.0.16 that package is unresolvable from this
// directory, so importing it stopped this file from loading at all. A default export carrying
// `id` and `setup` is the whole contract.

const VARIABLE = "ESP_POC_SESSION_ID";

export default {
  id: "esp-poc.session-trailer",
  setup(ctx) {
    let sessionID;

    ctx.session.hook("context", (event) => {
      if (event.sessionID) sessionID = event.sessionID;
    });

    ctx.shell.hook("create.before", (event) => {
      if (sessionID) event.env[VARIABLE] = sessionID;
    });

    console.log(
      `[esp-poc] session trailer armed: ${VARIABLE} reaches every shell the agent spawns`,
    );
  },
};
