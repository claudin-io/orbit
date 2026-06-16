# Orbit

**Orbit** is a Rust CLI that runs an autonomous coding loop. One binary, three commands, one ACP session. You give it a spec, it prompts an AI to plan, code, and evaluate — looping with feedback until the result passes or attempts run out. No daemon, no database, no state persistence.

```
orbit run --spec spec.md   # autonomous coding loop
orbit acp handshake         # test ACP agent connectivity
orbit git commit            # AI-assisted git commits
```

## How it works

A single `for` loop over ACP agent turns:

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
orbit run --spec spec.md                # from a spec file
orbit run --goal "Add JWT auth"         # inline goal (writes /tmp/orbit/spec-*.md)
orbit run                                # opens $EDITOR to write a spec
orbit run --spec spec.md --acp "opencode acp"   # choose ACP agent
orbit run --spec spec.md --max-attempts 3       # limit retries
orbit run -v --spec spec.md                      # verbose output
```

| Flag | Description | Default |
|---|---|---|
| `--spec` / `-s` | Path to spec file | opens editor if missing |
| `--goal` | Inline goal string | — |
| `--target` | Target project directory | `.` |
| `--config` | Explicit config file path | — |
| `--acp` | ACP agent command | `claude-code-acp` |
| `--max-attempts` | Max eval-feedback loops | `5` |
| `-v`, `--verbose` | Verbose output | `false` |

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

## Supported ACP agents

Orbit speaks [ACP (Agent Client Protocol)](https://opencode.ai/docs/acp/) over stdio JSON-RPC. Any ACP-compatible agent works:

- [`claude-code-acp`](https://github.com/zed-industries/claude-code-acp)
- [OpenCode ACP](https://opencode.ai/docs/acp/) (`opencode acp`)
- Codex (`codex acp`)
- Gemini CLI (`--experimental-acp`)

## Configuration

Config lives in `.orbit/config.orbit` files using orbit's own line-based format.

Resolution order (later overrides earlier):

1. Built-in defaults (`claude-code-acp`, 5 attempts, 1200s timeout)
2. `~/.orbit/config.orbit` — user-level (global)
3. `<target>/.orbit/config.orbit` — project-level
4. `--config` path — explicit config file
5. CLI flags — `--acp` and `--max-attempts` win everything

```
# .orbit/config.orbit
harness claude-code-acp        # fallback for any step without an override

# Per-step ACP agent (optional). Steps: plan, code, eval (alias: evaluation).
step plan = claude --acp
step code = opencode --acp
step eval = pi.dev --acp

max_attempts 5
timeout 1200                   # prompt_timeout_secs
```

**Per-step ACP:** each step of the loop can run a different ACP agent. `plan` →
the Prompter (and git Planner), `code` → the Coder (and git Committer), `eval` →
the Evaluator (and git plan Reviewer). A step with no override falls back to
`harness`. When every step resolves to the same command, they share one ACP
session (the default behavior). Inside `step ...`, `command` is required —
the first token is the binary, the rest are its arguments.

## Architecture

### Session model

ACP communication uses persistent sessions (`HarnessSession` trait). A `SessionRouter` lazily starts one session per distinct ACP command and routes each step's turns to it — so with the default config all steps share a single session, while per-step overrides get their own. No reconnect, no per-turn negotiation.

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
    mod.rs             # Harness + HarnessSession traits
    acp.rs             # ACP stdio JSON-RPC client
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
