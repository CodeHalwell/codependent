# Codypendent desktop

The graphical client for `codypendentd`: a Tauri shell around a React app.
The shell's Rust side owns the daemon socket and the local configuration files;
the webview is a projection of daemon state and never touches the filesystem or
the network itself. The terminal client (`codypendent`) and this app attach to
the same daemon, so a session started in one is live in the other.

## First launch

1. Install Codypendent (`install.sh`, or a release bundle) so that `codypendent`
   is on your `PATH` or in one of the usual places (`~/.cargo/bin`,
   `~/.local/bin`, `/usr/local/bin`, `/opt/homebrew/bin`).
2. Open the app. If no daemon is running, the banner across the top says so
   and offers **Start daemon**: the shell runs `codypendent __daemon` for you,
   detached, with its output in `<data dir>/logs/daemon.log`, and connects the
   moment the socket answers. **Retry now** re-attempts the connection on
   demand. If the shell cannot find the program it names the terminal command
   instead, `codypendent daemon start`, and you can point it at a build with
   `CODYPENDENT_BINARY=/path/to/codypendent`.
3. **Get Started** (the first sidebar entry) checks the three things a run
   needs: a configured model, a credential that resolves, and a repository.
   Each step links to the surface that fixes it. The green "setup is complete"
   banner also says when the daemon itself is unreachable, since a complete
   configuration with no daemon still cannot run anything.

The sidebar footer shows which daemon you are attached to (`daemon 0.14.0 ·
protocol 1.4 · desktop 0.14.0`) and calls out a version mismatch, which is the
usual sign of a daemon left running across an upgrade.

## Models, providers and keys

**Models** lists every `[[model]]` in `models.toml` with two badges: credential
presence (stored, from an environment variable, or missing) and readiness. The
readiness badge is computed the way the terminal client's picker computes it —
a local endpoint (Ollama, LM Studio, vLLM) is asked for its model list, a hosted
model has its credential resolved without touching the network, and an ACP
agent is left to the daemon to check when a run starts. **Test** asks the
provider over the network and turns `unverified` into `ready` or an error that
names the cause. **Use** pins a model for the next run; the choice, and the mode
chosen under **Mode**, persist across launches.

**Providers** lists the built-in catalog layered with your `providers.toml`.
Rows this build cannot execute are shown and disabled with their reason rather
than hidden. Coding agents that speak the Agent Client Protocol (Claude Code,
Codex, Gemini CLI, Kimi Code, Amp, Cline and the rest of the registry) are
added from a terminal — `codypendent acp list`, then `codypendent acp connect
claude-code` — and then appear under Models here.

**API Keys** shows presence only. A key you enter goes one way, into
`auth.json` at mode `0600`; nothing in the app can read it back.

## During a run

- The composer strip always names the model and mode staged for the next run
  and, while a run is live, what it is doing: `working…`, `running shell.run…`,
  `waiting for your approval`, or `retrying (2/5) · provider is overloaded ·
  next attempt in 8s` when the provider is backing off.
- The transcript shows a pulsing working row between visible updates, policy
  denials (`Blocked by policy: …`), budget warnings, and the measured usage
  once the run ends (`1,234 in · 567 out · $0.0034`; an unmeasured dimension is
  absent, never zero).
- An approval card offers Approve and Reject. A question card offers the
  agent's options (radio or check boxes), a typed answer where the prompt
  allows one, and a rejection with feedback; the answer is sent as the
  protocol's own `ResolveQuestion`.
- A failed run is a red card with the cause in plain words, the sanitized
  provider response folded underneath, and the next step as a button: **Open
  API keys** for an authentication refusal, **Choose a model** when none is
  configured, **Retry** with the same objective.
- The draft survives a refused submission: the composer clears only once the
  daemon has accepted the run.

## Keyboard

`⌘K` / `Ctrl-K` opens the command palette from anywhere, `/` does the same when
you are not typing in a field, and `Esc` closes the topmost overlay or goes
back to the previous view. The palette's **Keyboard shortcuts** entry lists the
rest. Every button has a keyboard equivalent, keyboard focus is always visible,
and the app honours `prefers-reduced-motion`.

## Theme

Colours are semantic tokens in `src/theme.css` (`--cody-bg`, `--cody-text`,
`--cody-accent`, `--cody-danger`, …), following the terminal client's theme
groups. The dark palette is the default; a light palette follows the OS
preference (`prefers-color-scheme: light`) or an explicit `data-theme="light"`
on the document root.

Components reference tokens only, never a colour literal: text on a solid
accent or status button is `--cody-on-accent`, tinted panels pair a `*-bg`
token with its text token (`--cody-warning-bg` with `--cody-warning`), and
`test/theme-tokens.test.ts` fails the build if a hex literal returns or a
component uses a token that one of the palettes does not define. To add a
colour, add the token to all three blocks in `theme.css`.

## Development

```bash
# The SDK packages the app links, built once:
(cd ../../sdk/ui && npm ci && npm run build)
(cd ../../sdk/protocol && npm ci && npm run build)
(cd ../../sdk/remote-ui && npm ci)

npm ci
npm run check      # tsc
npm test           # vitest
npm run build      # vite → dist/, embedded by the Tauri binary
npm run tauri:dev  # the shell with hot reload (needs the Tauri Linux deps in CI's desktop-tauri job)
```

The Rust side is its own Cargo workspace (`src-tauri/`), checked, linted and
tested separately: `cargo test --manifest-path src-tauri/Cargo.toml`.

## Knowledge surfaces

The **Skills**, **Memory**, **Docs** and **Plugins** views read through one
typed transport (`src/components/knowledgeTransport.ts`), which the shell
implements in two halves. The lists — registry items, memories, learnings,
documents — are the shell's own read of the daemon's SQLite database
(`src-tauri/src/knowledge.rs`), opened read-only and mapped to cards exactly
as the terminal client's harness maps them, so both clients show the same
facts; before the daemon has ever run there is no database, and the panel says
so rather than showing an empty list. The mutations — correcting or forgetting
a memory, creating, editing and publishing a document, the Remote UI plugin
lifecycle — are protocol commands on the live connection (`daemon.rs`), and a
document edit is composed as the TUI composes it: lease the block, apply the
edit, release the lease. Memory commands are scoped to the selected repository,
so they ask for one when none is selected.

Outside the shell (a browser tab, a test) there is no transport; the four
views then render an explicit unavailable panel naming the commands they need
and wear a "not in this build" badge in the sidebar and palette.
