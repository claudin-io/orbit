# Contributing to Orbit

Thank you for your interest in contributing to Orbit! This document outlines the development workflow and conventions.

## Project overview

Orbit is a Rust CLI that orchestrates coding agents through an autonomous test-driven development loop. It speaks ACP (Agent Client Protocol) over stdio JSON-RPC.

## Development setup

**Prerequisites:** Rust toolchain 1.75+

```bash
# Clone and build
git clone <repo-url>
cd orbit
cargo build

# Run tests
cargo test

# Install locally
cargo install --path .
```

## Code conventions

- **Formatting:** `rustfmt` with `max_width=120`, `tab_spaces=4`. Run `cargo fmt` before committing.
- **Linting:** `cargo clippy -- -D warnings` must pass.
- **Error handling:** Use `anyhow::Result` for application code; `OrbitError` enum for typed errors with exit codes.
- **Async:** Use tokio runtime; `#[tokio::test]` for async tests.
- **Naming:** `snake_case` for functions, modules, and test names.
- **Testing:** Unit tests inline (`#[cfg(test)] mod tests`) in source files; integration tests in `tests/*.rs`.
- **State persistence:** Use atomic write pattern (`.tmp` + `rename`).
- **Event pattern:** Use `emit!` macro from `src/events.rs` for cross-thread dispatch.
- **Template rendering:** Use the custom `render()` in `src/prompts.rs` (`{{var}}`, `{{#if var}}...{{/if}}`).

## Module layout

| Path | Purpose |
|---|---|
| `src/main.rs` | Entry point, tracing init, event channel, dispatch to orchestrator |
| `src/cli.rs` | CLI argument parsing (clap derive) — Run, Resume, Status, Harness, Acp commands |
| `src/config.rs` | `orbit.toml` loading, merging, and CLI flag overrides |
| `src/types.rs` | Core types — `RunPhase`, `TestCase`, `TestStatus`, `RunState`, `OrbitEvent`, `Role` |
| `src/error.rs` | `OrbitError` enum with typed exit codes |
| `src/events.rs` | Cross-thread event channel, `EventSink`, `emit!` macro |
| `src/harness/mod.rs` | `Harness` trait, `HarnessSession` |
| `src/harness/acp.rs` | ACP stdio JSON-RPC client with timeout |
| `src/harness/fake.rs` | Fake agent for hermetic unit tests |
| `src/prompts.rs` | Prompt templates + `render()` ({{var}}, {{#if var}}...{{/if}}) |
| `src/explorer.rs` | Project analysis — writes `.orbit/project-map.md` via ACP agent |
| `src/test_plan.rs` | Test plan generation, markdown parser, revision applier |
| `src/verify.rs` | Shell verification command runner |
| `src/analyzer.rs` | Failure analysis, coverage review, JSON verdict parser |
| `src/test_loop.rs` | Per-test attempt loop — implement → verify → analyze → retry |
| `src/state.rs` | State persistence (atomic JSON), `next_pending_test()`, `append_lessons()` |
| `src/orchestrator.rs` | Top-level orchestration — `dispatch()`, `run_with()`, `resume_with()` |
| `src/committer.rs` | Git commit via ACP agent session |
| `src/summary.rs` | End-of-run summary report |
| `src/render.rs` | Terminal ANSI renderer (headless, no TUI dependency) |
| `src/git.rs` | Git workspace setup — branch, worktree, merge, dirty-check |
| `src/interact.rs` | Simple stdin `read_choice()` for interactive prompts |
| `src/tool_format.rs` | ACP tool call formatter for display |
| `tests/*.rs` | Integration tests (19 files) |
| `src/bin/orbit-fake-agent.rs` | Standalone fake ACP agent binary for integration tests |

## Testing

```bash
# Run all tests
cargo test

# Run a specific integration test
cargo test --test test_name

# Run a specific unit test
cargo test test_name

# Update snapshots (insta)
cargo insta review
```

We use `insta` for snapshot testing and `tempfile::TempDir` for filesystem isolation. Integration tests use the fake ACP agent binary for hermetic orchestration tests.

## Pull request process

1. Ensure `cargo test`, `cargo clippy -- -D warnings`, and `cargo fmt --check` all pass.
2. Write tests for new functionality.
3. Keep changes focused — one feature or fix per PR.
4. Update the README if user-facing behaviour changes.

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
