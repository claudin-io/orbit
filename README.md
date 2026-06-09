# Orbit

**Orbit** is a Rust CLI that runs an autonomous coding loop. One binary, one loop, three ACP agents. Give it a spec, it prompts an AI to plan the implementation, codes it against your project, and evaluates the result. If the evaluator rejects the output, it retries with feedback. No human intervention mid-run, no TUI, no database, no state machine persistence. Just a prompt-eval loop over stdio JSON-RPC.

## Key idea

> A spec in, working code out.

Orbit is not a framework. It serializes a spec into a prompt, dispatches it through the Agent Client Protocol, and iterates until the evaluator approves or attempts run out. The entire state fits in a `for` loop.

## How it works

```
User -> Orbit:      orbit run --spec spec.md
Orbit -> Prompter:  read spec, produce implementation plan + evaluation rubric
Orbit -> Coder:     implement the plan against the target project
Orbit -> Evaluator: score result against rubric
                      |
         +------------+------------+
         |                         |
      approved                  rejected
         |                         |
      done                Prompter revises prompt
                              with evaluator feedback
                                    |
                              Coder re-implements
                                    |
                              Evaluator re-checks
                                    |
                              (loop until approved
                               or max_attempts hit)
```

## Simplicity

Orbit is one binary (`orbit`), one loop (`for`), three ACP agents (Prompter → Coder → Evaluator). Every design decision removes something so the remaining thing works well.

- **One binary, one loop** — no daemon, no background process, no persistent state. You invoke `orbit run`, it reads a spec, loops through three ACP agents, and exits. Process goes away, everything goes away.
- **No TUI** — the renderer writes ANSI escape codes to stdout with no ncurses, no ratatui, no interactive widgets. A single `tokio::spawn` redraws a fixed number of lines. Killable, pipeable (`orbit run | tee log`), testable without a terminal.
- **No resume, no checkpoint** — every run starts from scratch. State is a Rust struct on the stack. No crash recovery, no serialization, no migration.
- **No database, no cache** — no SQLite, no Redis, no file-based persistence. Nothing to corrupt, vacuum, or migrate.
- **No test runner** — Orbit does not execute `cargo test` or `npm test`. Verification is delegated to the Evaluator ACP agent against a rubric.
- **One ACP dependency** — `agent-client-protocol` v0.14 over subprocess stdio JSON-RPC. No HTTP server, no gRPC, no SDK wrapper.
- **Version scheme** — `0.1.YYMMDD` from `build.rs`. No semantic versioning treadmill; the date tells you how fresh the build is.

The complexity budget goes where it matters: JSON sanitization, rubric-based evaluation with weighted criteria, and retry with revision feedback.

## Installation

### Quick install

```bash
curl -sSfL -o /tmp/orbit "https://github.com/claudin-io/orbit/releases/latest/download/orbit-$(uname -s | tr '[:upper:]' '[:lower:]' | sed 's/darwin/arm64-macos/;s/linux/x86_64-linux/')" && chmod +x /tmp/orbit && mkdir -p ~/.local/bin && mv /tmp/orbit ~/.local/bin/orbit
```

> Make sure `~/.local/bin` is in your `PATH`. Add this to your shell profile (`~/.zshrc`, `~/.bashrc`):
> ```bash
> export PATH="$HOME/.local/bin:$PATH"
> ```

### Build from source

```bash
# Prerequisites: Rust toolchain (1.85+ for edition 2024)
cargo install --path .
```

## Usage

### `orbit run` — start an autonomous coding loop

```bash
# From a spec file
orbit run --spec spec.md --target ./my-project

# With an inline goal (writes to /tmp/orbit/spec-*.md)
orbit run --goal "Add user authentication with JWT"

# Opens $EDITOR to write your spec on the spot
orbit run

# Specify which ACP agent to use
orbit run --spec spec.md --acp "opencode acp"

# Set max attempts before giving up
orbit run --spec spec.md --max-attempts 3

# Enable verbose output
orbit run -v --spec spec.md
```

#### Flags

| Flag | Description | Default |
|---|---|---|
| `--spec` / `-s` | Path to specification file | opens editor if missing |
| `--goal` | Inline goal string (written to `/tmp/orbit/spec-*.md`) | — |
| `--target` | Target project directory | `.` |
| `--config` | Path to a config file (loaded before `./orbit.toml`) | — |
| `--acp` | ACP agent command (e.g. `"opencode acp"`) | `claude-code-acp` (falls back to `~/.orbit/config.toml` if set) |
| `--max-attempts` | Max attempts before giving up | `5` |
| `-v`, `--verbose` | Enable verbose output | `false` |

### `orbit acp` — manage ACP agent configuration

```bash
# Save a default ACP command to ~/.orbit/config.toml
orbit acp set-default "opencode acp"

# Test ACP agent connectivity (uses the resolved default)
orbit acp handshake
```

| Subcommand | Description |
|---|---|
| `set-default <cmd>` | Save default ACP agent command to `~/.orbit/config.toml` |
| `handshake` | Test ACP agent connectivity |

### Interactive spec editor

When neither `--spec` nor `--goal` is provided — or when `--spec` points to a non-existent file — Orbit opens an editor so you can write your spec interactively. The editor is resolved in this order:

1. `$ORBIT_EDITOR` — Orbit-specific override
2. `$EDITOR` — standard Unix convention
3. `$VISUAL` — fallback
4. `nano` — default

```bash
# Examples:
export ORBIT_EDITOR="code -w"   # VS Code (use -w to wait)
export ORBIT_EDITOR="vim"
export ORBIT_EDITOR="nvim"
```

Save and exit the editor; Orbit reads the content and proceeds normally.

## Configuration

### Project-level (`orbit.toml`)

Place in your project root (or point with `--config`):

```toml
[harness]
command = "claude-code-acp"
args = []

[loop]
max_attempts = 5
prompt_timeout_secs = 1200
```

### User-level (`~/.orbit/config.toml`)

Created by `orbit acp set-default`:

```toml
[harness]
command = "opencode"
args = ["acp"]
```

### Resolution order

Sources are merged in this order (later sources override earlier ones):

1. **Built-in defaults** — `claude-code-acp`, 5 attempts, 1200s timeout
2. **`~/.orbit/config.toml`** — user-level ACP fallback (only applied when the harness command is still the built-in default)
3. **`--config` path** — explicit config file via CLI flag
4. **`./orbit.toml`** — project-level config in the target directory
5. **CLI flags** — `--acp` and `--max-attempts` override everything

Merging is field-by-field. A TOML file only overrides fields it explicitly sets; unspecified fields fall through to the next source. CLI flags always win.

## Architecture

Runtime artifacts are written to `.orbit/` in the target project:

```
.orbit/
  debug/           # Per-run debug dumps (agent output on parse failure)
  logs/            # Structured tracing logs per run
  project-map.md   # Project structure map generated before the coding loop
  state.json       # Current run state (phase, attempt, agent output)
  summary.md       # Run summary (verdict, attempts, key events)
  tests.md         # Test plan extracted from the evaluator rubric
  lessons.md       # Lessons learned across runs (accumulated)
```

Source layout:

```
agents/
  foundation.md        # Design foundation document (read by agents)
src/
  main.rs              # Entry point, tracing init, event channel
  cli.rs               # CLI argument parsing (clap)
  config.rs            # orbit.toml loading and merging
  types.rs             # Core types (Role, PrompterOutput, EvalVerdict, RunPhase)
  error.rs             # Error types with exit codes
  events.rs            # Event channel for headless renderer
  lib.rs               # Module declarations
  harness/
    mod.rs             # Harness trait (run_turn)
    acp.rs             # ACP protocol client (stdio JSON-RPC)
    fake.rs            # Fake agent for hermetic testing
  prompts.rs           # Prompt templates with {{var}} rendering
  orchestrator.rs      # Top-level orchestration (dispatch, run_simple_loop)
  render.rs            # ANSI terminal renderer (headless — no TUI dependency)
  tool_format.rs       # ACP tool call formatter for display
```

## Supported ACP agents

Orbit speaks **ACP (Agent Client Protocol)** over stdio JSON-RPC. Any ACP-compatible agent works:

- [`claude-code-acp`](https://github.com/zed-industries/claude-code-acp)
- [OpenCode ACP](https://opencode.ai/docs/acp/)
- Codex (`codex acp`)
- Gemini CLI (`--experimental-acp`)

## Development

```bash
# Build
cargo build

# Run all tests
cargo test

# Run a specific test
cargo test test_name

# Lint
cargo clippy -- -D warnings

# Check formatting
cargo fmt --check

# Format
cargo fmt
```

### Testing

Orbit uses the standard Rust test framework with inline tests (`#[cfg(test)] mod tests` in source files). The fake ACP harness enables hermetic orchestration tests without a real LLM. There are no integration tests — all tests are co-located with their source.

## License

MIT
