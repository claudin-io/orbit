# Orbit

**Orbit** is a Rust CLI that runs an autonomous coding loop. One binary, three commands, one AI session. You give it a spec, it prompts an AI to plan, code, and evaluate — looping with feedback until the result passes or attempts run out. No daemon, no database, no state persistence.

```
orbit run --spec spec.md   # autonomous coding loop
orbit acp handshake         # test ACP agent connectivity
orbit git commit            # AI-assisted git commits
```

## Install

One-liner that grabs the latest release binary for your platform and drops it in
`/usr/local/bin`:

```bash
curl -fsSL "https://github.com/claudin-io/orbit/releases/latest/download/orbit-$([ "$(uname)" = Darwin ] && echo arm64-macos || echo x86_64-linux)" -o orbit && chmod +x orbit && sudo mv orbit /usr/local/bin/orbit
```

Then check it works:

```bash
orbit --version
```

Prefer to fetch a specific platform by hand? The latest release always exposes
these assets:

| Platform | Asset |
|---|---|
| macOS (Apple Silicon) | `orbit-arm64-macos` |
| Linux (x86_64) | `orbit-x86_64-linux` |

```bash
# macOS (arm64)
curl -fsSL https://github.com/claudin-io/orbit/releases/latest/download/orbit-arm64-macos -o orbit && chmod +x orbit && sudo mv orbit /usr/local/bin/orbit

# Linux (x86_64)
curl -fsSL https://github.com/claudin-io/orbit/releases/latest/download/orbit-x86_64-linux -o orbit && chmod +x orbit && sudo mv orbit /usr/local/bin/orbit
```

Building from source instead? See [Development](#development).

## How it works

A single `for` loop over agent turns (native Claude Code or ACP):

```
Prompter ──(plan + rubric)──→ Coder ──(output)──→ Evaluator
   ↑                                                  │
   │                                              approved?
   │                                            yes│    │no
   │                                                │    ↓
   │←──── (feedback + diagnosis) ──────────────┘←──┘ Prompter revises
```

Three roles, one loop. The Prompter produces a plan + weighted rubric, the Coder implements against the target project, and the Evaluator scores the result. If rejected, the Prompter revises with feedback and the cycle repeats.

## Simplicity

Orbit is deliberately minimal. Every design decision removes something:

- **One binary, no daemon** — run it, it exits. No background process, no crash recovery, no checkpointing. State is a Rust struct on the stack.
- **No TUI framework** — ANSI escape codes to stdout via a single `tokio::spawn`. Pipeable (`orbit run | tee log`), killable, testable without a terminal.
- **No database, no cache** — no SQLite, no Redis, no file-based persistence. Nothing to corrupt, vacuum, or migrate.
- **No test runner** — verification is delegated to the Evaluator ACP agent against a rubric. No `cargo test` or `npm test` integration.
- **Same pattern, different task** — `orbit git commit` follows the identical loop: Planner → Evaluator (with revision) → Committer. No shared state, no new infrastructure.
- **Version scheme** — `0.1.YYMMDD` from `build.rs`. The date tells you how fresh the build is.

The complexity budget goes where it matters: JSON sanitization, rubric-based evaluation with weighted criteria, and retry with revision feedback.

## Usage

### `orbit run` — autonomous coding loop

```bash
orbit run --spec spec.md                # from a spec file (native claude)
orbit run --goal "Add JWT auth"         # inline goal (writes /tmp/orbit/spec-*.md)
orbit run                                # opens $EDITOR to write a spec
orbit run --spec spec.md --acp "opencode acp"   # use ACP agent instead
orbit run --spec spec.md --max-attempts 3       # limit retries
orbit run -v --spec spec.md                      # verbose output
orbit run --spec spec.md --debug                 # stderr debug logs
```

| Flag | Description | Default |
|---|---|---|
| `--spec` / `-s` | Path to spec file | opens editor if missing |
| `--goal` | Inline goal string | — |
| `--target` | Target project directory | `.` |
| `--config` | Explicit config file path | — |
| `--acp` | ACP agent command | `claude` |
| `--max-attempts` | Max eval-feedback loops | `5` |
| `-v`, `--verbose` | Verbose output | `false` |
| `--debug` (global) | Internal debug logs to stderr | `false` |

### `orbit config` — interactive configuration wizard

```bash
orbit config   # guided setup, validates each ACP, writes .orbit automatically
```

Walks you through:
1. **Where to save** — global (`~/.orbit/config.orbit`) or project (`<cwd>/.orbit/config.orbit`).
2. **ACP mode** — the same agent for every step, or per-step (`plan`/`code`/`eval`).
3. **Type each ACP command** — pressing Enter runs a handshake immediately. On
   failure you can rewrite, save anyway, or cancel. In per-step mode you give a
   base command first; leaving a step blank reuses the base.

Existing `max_attempts`/`timeout`/comment lines in the file are preserved.

### `orbit acp` — manage ACP agent

```bash
orbit acp set-default "opencode acp"   # save default to ~/.orbit/config.orbit
orbit acp handshake                     # test connectivity
```

### `orbit git commit` — AI-assisted commits

```bash
orbit git commit          # analyze unstaged changes
orbit git commit --all    # stage everything first
orbit git commit -y       # skip confirmation
```

Runs Planner → Evaluator (loop) → Committer. The Planner reads git status + diff, proposes a commit structure, the Evaluator scores it (revisions if needed), then the Committer executes.

### `orbit git worktree` — manage git worktrees

```bash
orbit git worktree list            # list all worktrees
orbit git worktree add ../hotfix    # create worktree at ../hotfix on current branch
orbit git worktree add -b fix/api ../fix   # create worktree with new branch
orbit git worktree remove ../hotfix # remove a worktree (with confirmation)
```

Direct `git worktree` wrappers with orbit's colored output. No AI agent — just shell commands with consistent formatting.

## Supported agents

Orbit supports two harness types:

**Native Claude Code** (default, no ACP adapter needed). Talks to the `claude` CLI
directly over its stream-json protocol:

```
claude --print --input-format stream-json --output-format stream-json \
       --verbose --permission-mode bypassPermissions
```

Auto-detected: any command whose file stem is `claude` (not `claude-code-acp`).
The process stays alive across turns — one persistent conversation per session.

**ACP** — [Agent Client Protocol](https://opencode.ai/docs/acp/) over stdio
JSON-RPC. Any ACP-compatible agent works:

- [`claude-code-acp`](https://github.com/zed-industries/claude-code-acp)
- [OpenCode ACP](https://opencode.ai/docs/acp/) (`opencode acp`)
- Codex (`codex acp`)
- Gemini CLI (`--experimental-acp`)

## Configuration

Config lives in `.orbit/config.orbit` files using orbit's own line-based format.
Run `orbit config` for a guided wizard that validates each agent and writes the
file for you, or edit it by hand as below.

Resolution order (later overrides earlier):

1. Built-in defaults (`claude`, 5 attempts, 1200s timeout, 3 ACP retries, 1000ms base delay)
2. `~/.orbit/config.orbit` — user-level (global)
3. `<target>/.orbit/config.orbit` — project-level
4. `--config` path — explicit config file
5. CLI flags — `--acp` and `--max-attempts` win everything

```
# .orbit/config.orbit
harness claude                  # native stream-json (no ACP needed)

# Per-step agent (optional). Steps: plan, code, eval (alias: evaluation).
step plan = claude --acp        # uses ACP bridge instead of native
step code = opencode --acp      # ACP agent for coding step
step eval = pi.dev --acp        # ACP agent for evaluation

max_attempts 5
timeout 1200                    # prompt_timeout_secs

acp_retry_max_attempts 3        # retries on transient ACP errors (connection drops, timeouts)
acp_retry_base_delay_ms 1000    # base backoff delay; each retry multiplies by attempt#

fallback opencode acp           # fallback harness used when session limit is reached
```

**Per-step agents:** each step of the loop can run a different agent. `plan` →
the Prompter (and git Planner), `code` → the Coder (and git Committer), `eval` →
the Evaluator (and git plan Reviewer). A step with no override falls back to
`harness`. When every step resolves to the same command, they share one session
(the default behavior). Inside `step ...`, `command` is required — the first
token is the binary, the rest are its arguments.

**Resilience directives:**
- `acp_retry_max_attempts` — number of retries when an ACP call fails with a
  transient error (connection reset, timeout, read failure). Fatal errors
  (command parse failures) are never retried. Default: `3`.
- `acp_retry_base_delay_ms` — initial backoff in milliseconds. Each subsequent
  retry multiplies the delay by the attempt number. Default: `1000`.
- `fallback <command>` — harness to switch to when the current harness hits a
  session limit. Orbit prompts the user before switching. When unset, falls back
  to the global `harness`.

## Architecture

### Session model

Communication uses the `HarnessSession` trait. A `SessionRouter` lazily starts
one persistent session per distinct resolved command and routes each step's turns
to it — so with the default config all steps share a single session, while
per-step overrides get their own. The session flavor depends on the harness:

- **Native Claude** — a single `claude` subprocess kept alive across turns via
  stream-json stdin/stdout. One conversation, no reconnection.
- **ACP** — a persistent JSON-RPC session via the ACP library. If the session
  task dies, the handle detects the death and re-spawns transparently on the
  next turn.

On session start, the harness emits the resolved command or reported model name
so the user sees which agent is active before the first turn.

### Runtime artifacts

Written to `.orbit/` in the target project:

```
.orbit/
  debug/         # Agent output dumps (on parse failure)
  logs/          # Structured tracing logs
  project-map.md # Project structure map
  state.json     # Current run state
  summary.md     # Run verdict and key events
  tests.md       # Test plan from evaluator rubric
  lessons.md     # Accumulated lessons across runs
```

### Resilience

Orbit handles two classes of failure without crashing the run:

**ACP transient error retry.** When an ACP agent call fails with a transient
error (connection reset, timeout, broken pipe), the `AcpHarness` retries up to
`acp_retry_max_attempts` times with linear backoff
(`acp_retry_base_delay_ms * attempt_number`). Fatal errors (command parse
failures) are never retried. Each retry attempt appears as a notice in the
rendered output.

**Session-limit fallback.** When the Claude harness receives a `session limit`
or `rate limit` error from the API, the orchestrator emits a warning and prompts
the user: *"Session limit hit. Switch to fallback harness and retry?"* If
confirmed, the `SessionRouter` switches the current role to the `fallback`
harness, starts a new session, and retries the turn. If declined, the run fails
with a `SessionLimit` error. The fallback defaults to the global `harness` when
no explicit `fallback` is configured.

### Source layout

```
src/
  main.rs              # Entry point, tracing, renderer
  cli.rs               # CLI argument parsing (clap)
  config.rs            # Config loading and merging
  types.rs             # Core types (Role, PrompterOutput, EvalVerdict)
  error.rs             # Error types with exit codes
  events.rs            # Event channel for renderer
  orchestrator.rs      # Top-level loop (dispatch, run_simple_loop)
  prompts.rs           # Prompt templates with {{var}} rendering
  render.rs            # Headless ANSI renderer
  tool_format.rs       # ACP tool call display formatter
  git.rs               # Git commit 3-agent loop
  git_worktree.rs      # Git worktree commands (list/add/remove)
  harness/
    mod.rs             # Harness + HarnessSession traits, SessionRouter
    acp.rs             # ACP stdio JSON-RPC client with retry + session recovery
    claude.rs          # Native Claude Code stream-json harness
    fake.rs            # Fake harness for hermetic tests
agents/
  foundation.md        # Design foundation (agent context)
```

## Development

```bash
cargo build                     # build
cargo test                      # run all tests
cargo clippy -- -D warnings     # lint
cargo fmt                       # format
```

Orbit uses the standard Rust test framework with `#[cfg(test)]` inline tests. The fake ACP harness enables hermetic orchestration tests without a real LLM. No integration test framework — all tests are co-located with their source.

## License

MIT
