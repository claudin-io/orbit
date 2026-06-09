# Orbit — Foundation Document

> This document is the single source of truth for implementing Orbit. On approval, it will be committed verbatim to `./agents/foundation.md` in the repo (WB-00) so every future agent session can load it as context.

---

## 1. Project Overview

**Orbit** is a Rust CLI/TUI that orchestrates coding agents through a **fully autonomous, agentic test-driven loop**. Given a spec file and a base test command, it:

1. Explores the target project (structure + existing code).
2. Generates an initial test plan (`tests.md`) from the spec.
3. For each test, loops — *focused prompt → harness execution → deterministic verification → failure analysis → improved prompt* — until the test passes or attempts are exhausted.
4. **Evolves the plan while running**: the loop may add, split, or rewrite *pending* tests whenever analysis shows the current plan doesn't cover the spec well; after all tests pass, a final coverage review may add missing tests before the run is declared done.
5. Produces `summary.md`.

There is **no human validation anywhere mid-run**: Orbit generates the plan, implements, tests, analyzes, and improves both the code **and the tests themselves**, end to end.

```
User -> Orbit:          orbit run --spec spec.md
Orbit -> Explorer:      explore_project()            -> project structure + existing code
Orbit -> TestGenerator: generate_test_plan(spec)     -> tests.md (list of tests)
loop for each test in tests.md
    Orbit -> TestLoop:  process_test(current_test)
    loop until test passes (or max_attempts)
        TestLoop -> Harness:   run with focused prompt
        Harness  -> TestLoop:  execution result + logs
        TestLoop -> Analyzer:  analyze_failure(result)
        Analyzer -> TestLoop:  feedback + lessons
        TestLoop -> TestLoop:  update state & improve prompt
    end
    TestLoop -> Orbit:  test completed
end
Orbit -> User:          summary.md generated
```

### Core decisions (locked)

| Decision | Choice |
|---|---|
| Harness integration | **ACP (Agent Client Protocol)** — Orbit is an ACP *client*; any ACP agent works (claude-code-acp, Codex, OpenCode, Gemini CLI) |
| Pass/fail criterion | **Hybrid** — a shell verification command's exit code decides pass/fail; the Analyzer LLM runs only on failure to extract lessons |
| Target projects | **Language-agnostic** — Orbit is Rust; targets can be any stack |
| Scope | **MVP + TUI** (ratatui), with `--no-tui` headless mode |
| Explorer/TestGenerator/Analyzer | **Prompts on the same ACP harness** (separate sessions), not separate processes |
| Human-in-the-loop | **None** — fully autonomous from `orbit run` to `summary.md`; permissions answered by policy, never by a person |
| Test plan | **Living document** — input is spec + base test command; the loop may add/modify/split *pending* tests as understanding improves (bounded by `max_tests` / `max_plan_revisions`) |

---

## 2. Specification

### 2.1 CLI

```
orbit run --spec <spec.md> --test-cmd "<base test command>" [--target <dir>]
          [--config <orbit.toml>] [--no-tui] [--max-attempts <N>] [--from-test <ID>]
orbit resume [--target <dir>] [--no-tui]      # continue an interrupted run from .orbit/state.json
orbit harness check [--config <orbit.toml>]   # connect to the ACP agent, send a trivial prompt, print updates
orbit status [--target <dir>]                 # print run state as a table (no harness needed)
```

- `--target` defaults to the current working directory.
- `--test-cmd` is the project's base test runner (e.g. `pytest`, `cargo test`, `npm test --`). It seeds `[verify] base_command`; the TestGenerator derives each test's single-test `verify:` command from it. Required on `run` unless set in `orbit.toml`.
- Exit codes: `0` all tests passed · `1` some tests failed (attempts exhausted) · `2` configuration/protocol/runtime error.

### 2.2 Configuration — `orbit.toml`

Looked up in `--config`, then `<target>/orbit.toml`, then defaults.

```toml
[harness]
command = "claude-code-acp"   # any ACP agent binary
args = []                     # e.g. ["acp"] for `codex acp`
env = {}                      # extra env vars for the agent process

[permissions]
mode = "auto"                 # auto  = approve every permission request whose paths stay inside <target>
                              # strict = approve reads anywhere under <target>, deny writes outside it,
                              #          deny terminal commands not on the allowlist below
allow_commands = []           # strict mode: command prefixes the agent may run (e.g. ["cargo", "pytest"])

[loop]
max_attempts = 5              # per test
on_exhausted = "continue"     # continue | abort  (what to do when a test exhausts attempts)
prompt_timeout_secs = 600     # per ACP prompt turn; on timeout: cancel session, count as failed attempt
max_tests = 30                # hard cap on total tests in the living plan (runaway guard)
max_plan_revisions = 10       # hard cap on analyzer/coverage-driven plan changes per run
coverage_review = true        # after all tests pass, run one coverage review that may add tests

[verify]
base_command = ""             # project's base test runner; usually given via --test-cmd
timeout_secs = 300            # per verification command run
shell = "sh"                  # verification commands run via `sh -c "<cmd>"` in <target>
```

### 2.3 Artifacts — `.orbit/` directory inside the target project

| File | Producer | Purpose |
|---|---|---|
| `project-map.md` | Explorer | Project structure, stack, test framework, conventions |
| `tests.md` | TestGenerator | Ordered test plan; checkboxes updated live by Orbit |
| `lessons.md` | Analyzer (via Orbit) | Accumulated lessons, injected into subsequent prompts |
| `state.json` | Orbit | Resumable run state (schema in §2.6) |
| `summary.md` | Orbit | Final report |
| `logs/orbit.log` | tracing | Structured logs (TUI owns the terminal) |
| `logs/T<ID>-attempt<N>.log` | TestLoop | Full harness transcript + verification output per attempt |

### 2.4 `tests.md` format (machine-parseable contract)

The TestGenerator is instructed to emit exactly this shape; Orbit's parser enforces it.

```markdown
# Test Plan
<!-- orbit:format=v1 -->

## [ ] T01: short-kebab-name
- desc: One-sentence statement of the behavior this test proves.
- verify: `cargo test t01_short_name`
- depends: (none)

## [ ] T02: another-test
- desc: ...
- verify: `pytest -k test_another -x`
- depends: T01
```

Parser rules:
- A test = `## [ ] T<NN>: <name>` heading + `desc:`/`verify:`/`depends:` bullets. `verify` is a single backtick-quoted shell command. `depends` is `(none)` or a comma-separated ID list.
- IDs must be unique and sequential-ish; missing `verify` is a hard parse error → Orbit re-prompts the TestGenerator once with the parse errors appended, then aborts with exit 2.
- Orbit rewrites `[ ]` → `[x]` (passed) or `[!]` (exhausted) in place as the run progresses.

**Living-plan rules.** `tests.md` is mutable during the run:
- Orbit applies `plan_revision` verdicts (§2.5) and coverage-review additions (§2.5b) by rewriting the file; new tests get the next free ID and are appended (or inserted after a named ID).
- Only `[ ]` (pending) tests may be modified or removed; `[x]`/`[!]` entries are immutable history.
- The Implementer may edit the `verify:` line of *its own current test* when explicitly authorized (`bad-test-command` flow).
- Orbit **re-parses `tests.md` after every attempt and every verdict**; the parsed plan is the schedule. Caps: total tests ≤ `max_tests`, applied revisions ≤ `max_plan_revisions` — beyond the cap, revisions are logged but ignored.

### 2.5 Analyzer output contract

The Analyzer must reply with a single fenced JSON block (last one in the message wins):

```json
{
  "diagnosis": "Root cause in one or two sentences.",
  "feedback": "Concrete instruction for the next attempt (what to change, where).",
  "lessons": [
    "Reusable, project-level insight worth remembering for ALL future tests."
  ],
  "category": "wrong-implementation | missing-dependency | bad-test-command | environment | flaky | other",
  "plan_revision": {
    "reason": "Why the current plan fails to cover the spec (omit the whole field if no change needed).",
    "add": [
      { "after": "T03", "name": "short-kebab-name", "desc": "...", "verify": "..." }
    ],
    "modify": [
      { "id": "T05", "desc": "...", "verify": "..." }
    ],
    "remove": ["T06"]
  }
}
```

- `feedback` goes only into the next attempt's prompt for the same test.
- `lessons` are appended to `lessons.md` (deduplicated, with `[T<ID>/a<N>]` provenance tag) and injected into every later prompt.
- If `category == "bad-test-command"`, the next attempt's prompt explicitly authorizes the agent to fix the test invocation, and Orbit re-reads `verify:` from `tests.md` after the attempt (the agent may edit it).
- `plan_revision` is optional; when present, Orbit applies it to `tests.md` under the living-plan rules (§2.4) and emits a `PlanRevised` event.
- Unparseable JSON → retry the Analyzer once; still failing → use raw text as `feedback`, no lessons, no plan revision.

### 2.5b Coverage review (autonomous plan completion)

When every test in the plan is `[x]` and `coverage_review = true`, Orbit runs one extra harness turn (CoverageReviewer prompt, §4.5) that compares the spec against the implemented tests and replies with the same JSON contract (only `plan_revision` and `lessons` are honored). If it adds tests (within caps), the orchestrator loops back into the per-test phase; if not, the run is complete. The review runs **at most once per batch of newly added tests** and never re-opens passed tests.

### 2.6 `state.json` schema

```json
{
  "version": 1,
  "spec_path": "spec.md",
  "spec_hash": "sha256:...",
  "target": "/abs/path",
  "phase": "exploring | planning | testing | reviewing-coverage | summarizing | done",
  "started_at": "2026-06-09T21:00:00Z",
  "base_test_cmd": "pytest",
  "plan_revisions_applied": 2,
  "coverage_reviews_done": 1,
  "tests": [
    {
      "id": "T01",
      "name": "short-kebab-name",
      "status": "pending | running | passed | exhausted",
      "attempts": [
        { "n": 1, "stop_reason": "end_turn", "verify_exit": 1,
          "duration_secs": 184, "log": "logs/T01-attempt1.log" }
      ]
    }
  ]
}
```

`orbit resume` rules: state is rewritten atomically (temp file + rename) after every attempt. Resume re-parses `tests.md`, trusts `state.json` for statuses, restarts the first `pending`/`running` test at attempt `n+1` context (previous feedback is not persisted — resume restarts that test's attempt counter at the recorded count but with lessons.md only).

### 2.7 ACP integration requirements

- Spawn `harness.command` as a child process; speak ACP (JSON-RPC over stdio) using the `agent-client-protocol` crate (~0.14): `initialize` (ProtocolVersion::V1) → `session/new` with `cwd = <target>` → `session/prompt` → consume `session/update` notifications until the turn's stop reason.
- Orbit implements the client side: answer `session/request_permission` per `[permissions]` policy; answer `fs/read_text_file` / `fs/write_text_file` if the agent requests client-side fs (serve them directly, honoring the same path policy).
- One **fresh session per role invocation** (explore, plan, each test attempt, each analysis) — clean context; continuity comes from Orbit's prompt assembly, not session history.
- Ctrl-C: `session/cancel`, kill child, persist state, restore terminal.
- The agent child process is reused across sessions; if it dies, restart it once and replay `initialize`.

### 2.8 TUI (ratatui)

```
┌ Orbit ─ run: spec.md ─ harness: claude-code-acp ──────────────────┐
│ Tests              │ T03 · attempt 2/5 · phase: harness           │
│ ✔ T01 parse-args   │ ───────────────────────────────────────────  │
│ ✔ T02 load-config  │ <streaming agent output: message chunks,     │
│ ▶ T03 run-explore  │  tool call titles, terminal output>          │
│ ○ T04 ...          │                                              │
│ ✖ T05 (exhausted)  │                                              │
├────────────────────┴──────────────────────────────────────────────┤
│ elapsed 12:40 · passed 2 · failed 0 · pending 3        q to quit  │
└───────────────────────────────────────────────────────────────────┘
```

- Left: test list with status glyphs (`○` pending, `▶` running, `✔` passed, `✖` exhausted). Right: live event feed for the current session. Bottom: totals + elapsed.
- Headless mode renders the same internal events as plain log lines to stdout.
- TUI and orchestrator communicate only via the `OrbitEvent` mpsc channel — no business logic in the TUI.

---

## 3. Architecture

### 3.1 Crates

`agent-client-protocol` (~0.14) · `tokio` (full) · `clap` (derive) · `ratatui` + `crossterm` · `serde`/`serde_json`/`toml` · `anyhow` + `thiserror` · `tracing` + `tracing-subscriber` + `tracing-appender` · `sha2` (spec hash) · `chrono` (timestamps). Dev: `tempfile`, `insta` (optional, snapshot tests for prompts/parser).

### 3.2 Module map

```
src/
  main.rs            # entry: parse CLI, init tracing(file), dispatch command, pick TUI/headless
  cli.rs             # clap definitions (§2.1)
  config.rs          # load/merge orbit.toml + CLI overrides; validated Config struct
  types.rs           # TestCase, TestStatus, Attempt, AnalyzerVerdict, RunPhase, OrbitEvent
  error.rs           # thiserror OrbitError; map to exit codes
  events.rs          # tokio::mpsc<OrbitEvent>; emit helpers
  harness/
    mod.rs           # trait Harness (async): run_turn(SessionConfig, prompt, EventSink) -> TurnOutcome
    acp.rs           # AcpHarness: child process mgmt, ACP client wiring, permission policy
    fake.rs          # (cfg(test) + bin for integration tests) scripted ACP agent
  prompts.rs         # all templates from §4 as consts + render(context) helpers
  explorer.rs        # explore(harness) -> writes .orbit/project-map.md
  test_plan.rs       # generate(harness, spec, map) -> tests.md; parse_tests_md() -> Vec<TestCase>
  verify.rs          # run_verify(cmd, cwd, timeout) -> VerifyResult{exit, stdout, stderr, duration}
  analyzer.rs        # analyze(harness, ctx) -> AnalyzerVerdict (JSON contract §2.5)
  test_loop.rs       # process_test(): attempt loop per §2.5/§5 pseudocode
  state.rs           # RunState <-> .orbit/state.json (atomic writes); lessons.md append/dedupe
  summary.rs         # render summary.md from final RunState
  orchestrator.rs    # run/resume top-level: phases, wiring harness+state+events
  tui/mod.rs         # ratatui App consuming OrbitEvent; headless.rs sibling renderer
tests/
  parser_tests.rs    # tests.md parser fixtures
  state_tests.rs     # state round-trip, resume logic
  loop_integration.rs# end-to-end against fake ACP agent (pass-first, pass-3rd, exhaust)
```

### 3.3 Key trait

```rust
#[async_trait]
pub trait Harness {
    /// One fresh ACP session: send `prompt`, stream updates into `events`,
    /// return when the turn ends (stop reason) or times out.
    async fn run_turn(&self, role: Role, prompt: String, events: EventSink)
        -> Result<TurnOutcome>;
}
// Role = Explorer | TestGenerator | Implementer | Analyzer  (used for logging/permission nuance)
// TurnOutcome { stop_reason, full_text: String /* concatenated agent message chunks */ }
```

Everything above the harness (explorer, test_plan, analyzer, test_loop) depends only on this trait → fully testable with `fake.rs`.

### 3.4 Test loop pseudocode (normative)

```
fn run():                                                   # orchestrator outer loop
  explore(); generate_initial_plan(spec, base_test_cmd)
  loop:
    while let Some(test) = next_pending(parse(tests.md)):   # re-parsed each iteration (living plan)
      process_test(test)                                    # may itself revise the plan
    if coverage_review && revisions_left():
      verdict = coverage_reviewer(spec, tests.md, lessons)
      if verdict adds tests: apply; continue                # loop back into testing
    break
  summarize()

fn process_test(test):
  for n in 1..=max_attempts:
    prompt = render(IMPLEMENTER, spec_excerpt, project_map, lessons, test, prev_feedback)
    outcome = harness.run_turn(Implementer, prompt)          # timeout -> failed attempt
    verify  = run_verify(test.verify_cmd)                    # exit 0 wins, always
    log attempt artifacts; persist state
    if verify.exit == 0: mark passed; update tests.md; return Passed
    verdict = analyzer.analyze(test, outcome, verify, git_diff(target))
    append lessons; prev_feedback = verdict.feedback
    if verdict.plan_revision: apply_to_tests_md(within caps) # living plan (§2.4)
    if verdict.category == "bad-test-command": re-read verify cmd from tests.md
  mark exhausted; update tests.md '[!]'; return Exhausted   # on_exhausted: continue|abort
```

---

## 4. Prompt Templates (verbatim, `{{placeholders}}` rendered by `prompts.rs`)

### 4.1 Explorer

```
You are the EXPLORER stage of Orbit, an automated test-driven development loop.

Your only job is to study this project and write a concise project map. Do NOT
modify any source file.

Investigate:
1. Directory layout and the purpose of each top-level directory.
2. Language(s), frameworks, package manager, build tool.
3. How tests are written and executed here (framework, command, naming
   conventions, where test files live). If no tests exist yet, state the
   idiomatic choice for this stack.
4. Existing modules/functions relevant to the spec below, with file paths.
5. Conventions to respect (formatting, error handling, module organization).

The spec the next stages will implement:
---SPEC---
{{spec}}
---END SPEC---

Write your findings to the file `.orbit/project-map.md` using this structure:
# Project Map
## Stack
## Layout
## Testing (framework, exact command pattern to run a single test)
## Relevant existing code
## Conventions

Keep it under 150 lines. Finish by confirming the file was written.
```

### 4.2 TestGenerator

```
You are the TEST PLANNER stage of Orbit, an automated test-driven development loop.

Read the spec and the project map below, then produce an ordered test plan.
Each test must be small, independently verifiable, and ordered so that earlier
tests never depend on later ones. Prefer 5-15 tests; merge trivia, split epics.

---SPEC---
{{spec}}
---END SPEC---

---PROJECT MAP---
{{project_map}}
---END PROJECT MAP---

The project's base test command is: {{base_test_cmd}}
Every per-test `verify` command must be derived from it (add the runner's filter
flag to select exactly one test).

Write the plan to `.orbit/tests.md` in EXACTLY this format (the runner parses it
mechanically; deviations break the run):

# Test Plan
<!-- orbit:format=v1 -->

## [ ] T01: short-kebab-name
- desc: One sentence describing the behavior this test proves.
- verify: `<exact shell command that runs ONLY this test and exits 0 on pass>`
- depends: (none)

Rules:
- IDs are T01, T02, ... in execution order.
- `verify` must use this project's test runner (see Testing section of the map)
  and select a single test by name/filter. It runs from the project root.
- `depends` lists prior test IDs or `(none)`.
- Do NOT implement anything yet. Only write `.orbit/tests.md`.
{{parse_errors_block}}
```

(`{{parse_errors_block}}` is empty on the first call; on re-prompt it contains
"Your previous plan failed to parse: …fix and rewrite the full file.")

### 4.3 Implementer (focused per-test, per-attempt prompt)

```
You are the IMPLEMENTER stage of Orbit, an automated test-driven development loop.
You work on exactly ONE test this turn. Make it pass; touch nothing unrelated.

---SPEC (relevant context)---
{{spec}}
---END SPEC---

---PROJECT MAP---
{{project_map}}
---END PROJECT MAP---

{{#if lessons}}
---LESSONS FROM EARLIER TESTS (respect these)---
{{lessons}}
---END LESSONS---
{{/if}}

YOUR TEST:
  id: {{test_id}}  ({{test_name}})
  goal: {{test_desc}}
  verification command (run from project root): {{verify_cmd}}

Already passing tests you must NOT break: {{passed_ids_csv}}

{{#if prev_feedback}}
PREVIOUS ATTEMPT FAILED. Analyzer feedback you must act on:
{{prev_feedback}}
{{/if}}

Procedure:
1. If the test itself does not exist yet, write it first (file/name must match
   the verification command above).
2. Implement the minimal production code to make it pass.
3. Run the verification command yourself and iterate until it exits 0.
4. Run the broader test suite if cheap, to avoid regressions.

Constraints:
- Stay inside this project directory.
- Follow the conventions in the project map.
- Do not edit `.orbit/` files except as explicitly allowed.
{{#if allow_fix_verify}}
- The verification command itself was diagnosed as broken. You MAY correct the
  `verify:` line for {{test_id}} inside `.orbit/tests.md` to the right command.
{{/if}}
```

### 4.4 Analyzer

```
You are the ANALYZER stage of Orbit, an automated test-driven development loop.
An implementation attempt failed verification. Diagnose it. Do NOT modify any file.

TEST: {{test_id}} ({{test_name}}) — {{test_desc}}
VERIFICATION COMMAND: {{verify_cmd}}
ATTEMPT: {{attempt_n}} of {{max_attempts}}

---VERIFICATION OUTPUT (exit code {{verify_exit}})---
{{verify_output_tail}}
---END OUTPUT---

---WHAT THE IMPLEMENTER DID (its final report)---
{{implementer_text_tail}}
---END REPORT---

---GIT DIFF OF THE ATTEMPT---
{{git_diff_tail}}
---END DIFF---

{{#if lessons}}
Lessons already recorded (do not repeat them): 
{{lessons}}
{{/if}}

You may read project files to confirm your diagnosis.

The current test plan (you may propose changes to PENDING tests only):
---TESTS.MD---
{{tests_md}}
---END TESTS.MD---

Reply with exactly one fenced JSON block:
```json
{
  "diagnosis": "...",
  "feedback": "...",
  "lessons": ["..."],
  "category": "wrong-implementation | missing-dependency | bad-test-command | environment | flaky | other",
  "plan_revision": { "reason": "...", "add": [...], "modify": [...], "remove": [...] }
}
```
- "feedback": the single most useful instruction for the NEXT attempt.
- "lessons": only durable, project-wide insights (empty array if none).
- "bad-test-command" means the verify command itself is wrong, not the code.
- "plan_revision" is OPTIONAL — include it only when this failure reveals that the
  plan itself is wrong or incomplete (missing test, wrong order, test that should
  be split). Passed tests cannot be touched. Omit the field entirely otherwise.
```

### 4.5 CoverageReviewer (runs once after all tests pass, §2.5b)

```
You are the COVERAGE REVIEWER stage of Orbit, an automated test-driven
development loop. Every planned test now passes. Your job is to decide whether
the test plan actually covers the spec, or whether passing it was too easy.
Do NOT modify any file.

---SPEC---
{{spec}}
---END SPEC---

---CURRENT TEST PLAN (all passed)---
{{tests_md}}
---END PLAN---

{{#if lessons}}
---LESSONS RECORDED DURING THE RUN---
{{lessons}}
---END LESSONS---
{{/if}}

You may read project files (source and tests) to judge real coverage.

Look for: spec requirements with no corresponding test, edge cases and error
paths the spec implies, integration gaps between features tested only in
isolation.

Reply with exactly one fenced JSON block:
```json
{
  "diagnosis": "Coverage assessment in one or two sentences.",
  "lessons": ["..."],
  "plan_revision": {
    "reason": "...",
    "add": [ { "after": "T07", "name": "...", "desc": "...", "verify": "..." } ]
  }
}
```
- If coverage is adequate, omit "plan_revision" entirely — the run will finish.
- Only "add" is honored here; you cannot modify or remove existing tests.
- Each added test must include a `verify` command derived from: {{base_test_cmd}}
```

All `*_tail` placeholders are truncated to the last N bytes (default 8 KiB) with a `[... truncated]` marker, to bound prompt size.

---

## 5. Workblocks

> Each workblock is independently implementable and ends with `cargo build && cargo clippy -- -D warnings && cargo test` green plus its own acceptance checks. Order respects dependencies.

### WB-00 — Repository bootstrap
**Goal:** empty dir → buildable skeleton with this document committed.
- `git init`; `cargo init --name orbit`; add all crates from §3.1.
- Create `agents/foundation.md` with this document's content.
- Create module skeleton from §3.2 (empty mods compiling), `rustfmt.toml`, `.gitignore` (`/target`, `.orbit/`).
- `main.rs` prints version; wire `tracing` to `.orbit/logs/orbit.log` (lazy-created).
**Accept:** `cargo run -- --help` works; first commit done.

### WB-01 — CLI + config + core types
**Goal:** §2.1 CLI and §2.2 config fully parsed and validated.
- `cli.rs`: clap derive for `run`, `resume`, `harness check`, `status` with all flags (including `--test-cmd`).
- `config.rs`: load `orbit.toml` (path resolution order from §2.2), defaults, CLI overrides (`--max-attempts`, `--test-cmd`), validation (harness command non-empty, base test command present for `run`, timeouts > 0, caps ≥ 1).
- `types.rs` + `error.rs`: all structs/enums from §2.6/§3.2; `OrbitError -> exit code` mapping.
**Accept:** unit tests — config merge precedence, bad TOML → exit 2 message; `orbit status` on a dir without `.orbit/` says "no run found".

### WB-02 — ACP harness client + `orbit harness check`  *(highest risk — do early)*
**Goal:** working `AcpHarness` implementing the `Harness` trait (§3.3) against a real agent.
- Spawn child (`command` + `args` + `env`, cwd = target), connect `agent-client-protocol` client over its stdio; `initialize` (V1), capability exchange.
- `run_turn`: `session/new` (cwd=target) → `session/prompt` → forward each `session/update` (message chunks, tool-call notifications, terminal output) as `OrbitEvent`s → return `TurnOutcome` on stop reason; enforce `prompt_timeout_secs` (cancel + error).
- Permission responder per `[permissions]` policy (§2.2, §2.7); client-side fs handlers with same path policy.
- Child lifecycle: lazy start, reuse across turns, restart-once on death, kill + `session/cancel` on Ctrl-C.
- `orbit harness check`: trivial prompt ("Reply with the single word ORBIT-OK"), print streamed updates, exit 0 if reply contains the token.
**Accept:** `orbit harness check` passes against `claude-code-acp` (`npm i -g @zed-industries/claude-code-acp`); timeout path covered by a unit test with a stub that never replies.

### WB-03 — Fake ACP agent + harness integration tests
**Goal:** deterministic test double speaking real ACP, so all later workblocks have CI-grade tests.
- `src/harness/fake.rs` + `[[bin]] orbit-fake-agent`: reads a scenario JSON (env var `ORBIT_FAKE_SCRIPT`) listing, per prompt-turn: update chunks to emit, files to write into cwd, permission requests to issue, stop reason.
- Scenarios needed later: `explorer-writes-map`, `planner-writes-tests`, `implementer-pass`, `implementer-fail-then-pass`, `analyzer-json`, `analyzer-plan-revision` (verdict adds/modifies pending tests), `coverage-adds-tests` then `coverage-clean`, `never-replies` (timeout).
- Integration test: `AcpHarness` against the fake binary round-trips a turn and surfaces events in order.
**Accept:** `cargo test` runs the fake-agent integration suite hermetically (no network, no real LLM).

### WB-04 — Prompt templates module
**Goal:** §4 templates as code.
- `prompts.rs`: consts for the four templates; tiny renderer for `{{var}}` + `{{#if}}` blocks (hand-rolled, no template-engine dep); `tail(bytes)` truncation helper.
- Snapshot/unit tests: each template renders with full and minimal context; truncation marker appears.
**Accept:** rendered prompts byte-match snapshots.

### WB-05 — Explorer stage
**Goal:** `explore()` produces `.orbit/project-map.md`.
- Render Explorer prompt (spec injected), `run_turn(Role::Explorer, ...)`; verify the file exists and is non-empty afterward; if the agent only replied inline (file missing), write `TurnOutcome.full_text` to the file as fallback; emit phase events.
**Accept:** integration test with `explorer-writes-map` scenario; fallback path unit-tested.

### WB-06 — Test plan generation + living-plan parser
**Goal:** `tests.md` produced, parsed into `Vec<TestCase>`, and mutable per the living-plan rules (§2.4).
- `generate()`: render TestGenerator prompt (with `{{base_test_cmd}}`) → turn → parse `.orbit/tests.md`.
- `parse_tests_md()` per §2.4 (regex/line-based, tolerant of surrounding prose, strict on required fields); checkbox rewrite helpers (`mark(id, Passed|Exhausted)`) preserving the rest of the file byte-for-byte.
- `apply_revision(PlanRevision)`: add (next free ID, optional `after` anchor) / modify / remove **pending-only**; reject touches to `[x]`/`[!]`; enforce `max_tests` and `max_plan_revisions` (over-cap → log + ignore).
- Re-prompt-once-on-parse-error flow (append `{{parse_errors_block}}`), then exit 2.
**Accept:** parser fixture tests (valid, missing verify, duplicate ID, prose noise, CRLF); revision tests (add/modify/remove pending, rejected passed-test edit, cap enforcement, ID allocation); integration test with `planner-writes-tests` scenario; checkbox rewrite round-trip test.

### WB-07 — Verification runner
**Goal:** deterministic pass/fail oracle.
- `run_verify(cmd, cwd, timeout)`: `sh -c` via `tokio::process`, capture stdout+stderr (merged, capped at 1 MiB), kill process group on timeout, return `VerifyResult`.
**Accept:** unit tests — exit 0, exit 1, timeout kill, large-output cap.

### WB-08 — Analyzer + CoverageReviewer stages
**Goal:** failure → structured verdict; passed-plan → coverage verdict.
- Build context (verify output tail, implementer text tail, current `tests.md`, `git diff` of target via `tokio::process` — if target isn't a git repo, diff section says "(target is not a git repository)"); render Analyzer prompt; parse last fenced JSON block (including optional `plan_revision`); retry-once / raw-text-fallback per §2.5; lessons dedupe+provenance append to `lessons.md`.
- `coverage_review()`: render §4.5 prompt, same JSON parsing; only `plan_revision.add` + `lessons` honored.
**Accept:** unit tests for JSON extraction (clean, surrounded by prose, malformed→retry→fallback, with/without `plan_revision`); integration with `analyzer-json` and `analyzer-plan-revision` scenarios; lessons file idempotence test.

### WB-09 — State, persistence, resume
**Goal:** §2.6 implemented.
- `state.rs`: `RunState` serde round-trip; atomic save (tmp+rename) after every phase change and attempt; spec-hash mismatch on `resume` → refuse with clear message (`--force` future work, not MVP).
- `orbit resume` + `orbit status` wired to real state; `--from-test T0N` skips earlier tests (marks them `skipped` is NOT in scope — it requires them already `passed`, else exit 2).
**Accept:** kill-mid-run simulation test: write state, reload, resume points at correct test/attempt; `status` renders table.

### WB-10 — Test loop + autonomous orchestrator
**Goal:** the heart — §3.4 pseudocode end to end, fully autonomous (no human gate anywhere).
- `test_loop.rs::process_test()` exactly per §3.4 (attempt logs to `logs/T<ID>-attempt<N>.log`, state persisted each step, `plan_revision` applied via WB-06's `apply_revision`, `bad-test-command` re-read, `on_exhausted` policy).
- `orchestrator.rs`: outer loop per §3.4 — explore → plan → drain pending tests (re-parsing the living `tests.md` each iteration) → coverage review (§2.5b, may add tests and loop back) → summarize; honors `resume` entry points; Ctrl-C handler (cancel, persist, exit 130).
**Accept:** integration tests against fake agent: (a) all-pass run produces `[x]` everywhere + exit 0; (b) fail-then-pass consumes 2 attempts and records analyzer feedback in the second prompt (assert via fake-agent received-prompt capture); (c) exhaustion with `on_exhausted=continue` → exit 1 and `[!]`; (d) timeout attempt counts as failure; (e) analyzer `plan_revision` mid-run inserts a pending test that then gets executed; (f) `coverage-adds-tests` scenario loops back into testing, then `coverage-clean` finishes the run; (g) caps respected (`max_tests`, `max_plan_revisions`).

### WB-11 — Summary
**Goal:** `summary.md`.
- Table: test, status, attempts, duration; sections: lessons learned (from `lessons.md`), failures with last diagnosis, total wall time, harness used.
**Accept:** snapshot test from a synthetic `RunState`.

### WB-12 — Events + TUI + headless renderer
**Goal:** §2.8 UI.
- `events.rs`: finalize `OrbitEvent` enum (PhaseChanged, TestStarted, AttemptStarted, AgentChunk, ToolCall, VerifyFinished, AnalyzerVerdict, TestFinished, RunFinished).
- `tui/`: ratatui app per §2.8 wireframe; `q`/Ctrl-C quit (graceful cancel); auto-scroll output pane with manual scroll (arrows/PgUp).
- Headless: same events → timestamped lines; selected by `--no-tui` or non-tty stdout.
**Accept:** headless integration test asserts event line sequence for the all-pass scenario; TUI smoke-tested manually (no automated TUI test in MVP).

### WB-13 — Real-world E2E + docs
**Goal:** prove it on a real harness.
- Create `examples/toy-target/` (tiny Python or Rust lib + `spec.md` with 3-4 small features).
- Run `orbit run --spec spec.md --test-cmd "<runner>"` with `claude-code-acp`; fix integration gaps found.
- `README.md`: install, quickstart, config reference, supported harnesses table, architecture diagram (the §1 sequence).
**Accept:** documented successful E2E transcript (checked-in `summary.md` from the toy run); README complete.

**Dependency order:** WB-00 → WB-01 → WB-02 → WB-03 → (WB-04..WB-08 parallelizable) → WB-09 → WB-10 → WB-11 → WB-12 → WB-13.

---

## 6. Verification strategy (whole project)

1. Per-workblock acceptance tests (above) — all hermetic except WB-02's real-agent check and WB-13.
2. `cargo clippy -- -D warnings` + `cargo fmt --check` gate every workblock.
3. The fake ACP agent (WB-03) is the backbone: every orchestration behavior must have a scripted scenario.
4. Final: WB-13 E2E with `claude-code-acp` on the toy target; verify artifacts (`tests.md` checkboxes, `lessons.md`, `summary.md`), exit code, and an interrupted-then-`orbit resume` run.

## 7. References

- ACP protocol: https://agentclientprotocol.com · Rust crate: https://docs.rs/agent-client-protocol (~0.14)
- Agents: claude-code-acp (`npm i -g @zed-industries/claude-code-acp`), `codex acp`, OpenCode ACP (https://opencode.ai/docs/acp/), Gemini CLI `--experimental-acp`
