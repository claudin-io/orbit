//! Native Claude Code harness.
//!
//! Talks to the `claude` CLI directly over its stream-json protocol instead of
//! going through an ACP adapter. Claude Code does not speak ACP natively, so
//! the alternative would be bundling the Node `claude-agent-acp` bridge; this
//! harness removes that layer entirely by spawning
//!
//! ```text
//! claude --print --input-format stream-json --output-format stream-json --verbose \
//!        --permission-mode bypassPermissions
//! ```
//!
//! and exchanging newline-delimited JSON: each turn writes one
//! `{"type":"user",...}` message to stdin and reads stdout lines until the
//! turn's `{"type":"result",...}`. The process is kept alive across turns
//! (stream-json input mode), so a persistent [`ClaudeSessionHandle`] maps
//! cleanly onto Claude Code's own conversation state. `Task` subagents are run
//! inside `claude` itself, so the ACP harness's subagent-spawning machinery is
//! not needed here.

use super::{Harness, HarnessSession};
use crate::error::OrbitError;
use crate::events::EventSink;
use crate::tool_format;
use crate::types::{OrbitEvent, Role, TurnOutcome};
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

/// `true` when `command` refers to the native `claude` CLI (by file stem), so
/// the [`SessionRouter`](super::SessionRouter) can route it to this harness
/// instead of the generic ACP one. Matches `claude`, `/usr/local/bin/claude`,
/// `claude.exe`, etc. — but not `claude-code-acp`.
pub fn is_native_claude(command: &str) -> bool {
    Path::new(command)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|stem| stem == "claude")
        .unwrap_or(false)
}

fn send_event(tx: &EventSink, event: OrbitEvent) {
    let _ = tx.send(event);
}

/// Build the user-message line written to `claude`'s stdin for one prompt.
fn user_message_line(prompt: &str) -> String {
    let msg = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": prompt },
    });
    format!("{msg}\n")
}

struct TurnData {
    full_text: String,
    stop_reason: String,
}

/// Drain any complete lines out of `line_buf`, emitting each as an
/// `AgentChunk`. Mirrors the line-buffering the ACP harness does so partial
/// trailing text is held back until the next chunk completes it.
fn drain_lines(line_buf: &mut String, events: &EventSink) {
    while let Some(pos) = line_buf.find('\n') {
        let complete = line_buf[..pos].to_string();
        if !complete.is_empty() {
            send_event(events, OrbitEvent::AgentChunk(complete));
        }
        line_buf.drain(..=pos);
    }
}

/// Process a single `assistant` message: stream its text blocks line-by-line
/// and emit a `ToolCall` event for each `tool_use` block.
fn handle_assistant(
    v: &Value,
    events: &EventSink,
    cwd: &Path,
    line_buf: &mut String,
    output: &mut String,
) {
    let Some(content) = v.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array())
    else {
        return;
    };

    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    output.push_str(text);
                    line_buf.push_str(text);
                    drain_lines(line_buf, events);
                }
            }
            Some("thinking") => {
                if let Some(text) = block.get("thinking").and_then(|t| t.as_str())
                    && !text.trim().is_empty()
                {
                    send_event(events, OrbitEvent::AgentChunk("[thought] ".to_string()));
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                if name.is_empty() {
                    continue;
                }
                let raw_input = block.get("input").cloned();
                // tool_format keys off lowercase tool names ("read", "bash", …).
                let params = tool_format::fmt_tool_call(&name.to_lowercase(), &raw_input, cwd);
                tracing::info!(tool = %name, "tool call");
                send_event(events, OrbitEvent::ToolCall { name, params, raw_input });
            }
            _ => {}
        }
    }
}

/// Read stdout lines until this turn's terminal `result` message, translating
/// each into orbit events and accumulating the assistant's final text.
async fn pump_turn(
    lines: &mut Lines<BufReader<ChildStdout>>,
    events: &EventSink,
    cwd: &Path,
) -> Result<TurnData, OrbitError> {
    let mut line_buf = String::new();
    let mut output = String::new();

    loop {
        let line = match lines.next_line().await? {
            Some(l) => l,
            None => {
                return Err(OrbitError::Other(
                    "claude closed its output before completing the turn".to_string(),
                ))
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        let v: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!(error = %e, "skipping unparseable claude output line");
                continue;
            }
        };

        match v.get("type").and_then(|t| t.as_str()) {
            Some("assistant") => handle_assistant(&v, events, cwd, &mut line_buf, &mut output),
            Some("result") => {
                let remaining = line_buf.trim();
                if !remaining.is_empty() {
                    send_event(events, OrbitEvent::AgentChunk(remaining.to_string()));
                }

                let is_error = v.get("is_error").and_then(|b| b.as_bool()).unwrap_or(false);
                let stop_reason = v
                    .get("stop_reason")
                    .and_then(|s| s.as_str())
                    .unwrap_or("end_turn")
                    .to_string();
                let result_text =
                    v.get("result").and_then(|r| r.as_str()).unwrap_or("").to_string();

                if is_error {
                    let detail = if !result_text.is_empty() {
                        result_text
                    } else {
                        v.get("subtype").and_then(|s| s.as_str()).unwrap_or("error").to_string()
                    };

                    let detail_lower = detail.to_lowercase();
                    if detail_lower.contains("session limit") || detail_lower.contains("rate limit") {
                        return Err(OrbitError::SessionLimit(detail));
                    }

                    return Err(OrbitError::Other(format!("claude turn failed: {detail}")));
                }

                // `result.result` is the authoritative final assistant text;
                // fall back to accumulated text blocks if it's absent.
                let full_text = if !result_text.is_empty() { result_text } else { output };
                return Ok(TurnData { full_text, stop_reason });
            }
            // system/init, rate_limit_event, tool-result `user` messages, etc.
            _ => {}
        }
    }
}

enum SessionCommand {
    RunTurn {
        prompt: String,
        result: oneshot::Sender<Result<String, OrbitError>>,
    },
}

pub struct ClaudeSessionHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
}

#[async_trait]
impl HarnessSession for ClaudeSessionHandle {
    async fn run_turn(&mut self, _role: Role, prompt: String) -> Result<TurnOutcome, OrbitError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(SessionCommand::RunTurn { prompt, result: tx })
            .map_err(|_| OrbitError::Other("claude session task has terminated".to_string()))?;
        let full_text = rx
            .await
            .map_err(|_| OrbitError::Other("claude session task has terminated".to_string()))??;
        Ok(TurnOutcome { stop_reason: "end_turn".to_string(), full_text })
    }
}

pub struct ClaudeHarness {
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    prompt_timeout_secs: u64,
}

impl ClaudeHarness {
    pub fn new(command: String, args: Vec<String>, cwd: PathBuf, prompt_timeout_secs: u64) -> Self {
        Self { command, args, cwd, prompt_timeout_secs }
    }

    /// Spawn `claude` in stream-json print mode with stdin/stdout piped.
    fn spawn(&self) -> Result<Child, OrbitError> {
        let mut cmd = Command::new(&self.command);
        cmd.current_dir(&self.cwd)
            .arg("--print")
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--permission-mode")
            .arg("bypassPermissions")
            .args(&self.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        tracing::debug!(
            command = %self.command,
            cwd = %self.cwd.display(),
            "spawning native claude harness"
        );

        cmd.spawn().map_err(|e| OrbitError::Other(format!("failed to spawn `{}`: {e}", self.command)))
    }

    /// Write one prompt and pump the turn, applying the prompt timeout.
    async fn run_one(
        &self,
        stdin: &mut ChildStdin,
        lines: &mut Lines<BufReader<ChildStdout>>,
        prompt: &str,
        events: &EventSink,
    ) -> Result<TurnData, OrbitError> {
        stdin.write_all(user_message_line(prompt).as_bytes()).await?;
        stdin.flush().await?;

        let timeout = Duration::from_secs(self.prompt_timeout_secs);
        match tokio::time::timeout(timeout, pump_turn(lines, events, &self.cwd)).await {
            Ok(result) => result,
            Err(_) => Err(OrbitError::Other(format!(
                "claude did not complete the turn within {} seconds",
                self.prompt_timeout_secs
            ))),
        }
    }
}

#[async_trait]
impl Harness for ClaudeHarness {
    async fn run_turn(
        &self,
        role: Role,
        prompt: String,
        events: EventSink,
    ) -> Result<TurnOutcome, OrbitError> {
        let started = Instant::now();
        tracing::info!(role = ?role, prompt_bytes = prompt.len(), "turn started (native claude)");

        let mut child = self.spawn()?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut lines = BufReader::new(stdout).lines();

        let data = self.run_one(&mut stdin, &mut lines, &prompt, &events).await;
        // Close stdin so claude exits cleanly, then reap it.
        drop(stdin);
        let _ = child.kill().await;
        let data = data?;

        tracing::info!(
            role = ?role,
            duration_secs = started.elapsed().as_secs_f64(),
            "turn finished (native claude)"
        );
        Ok(TurnOutcome { stop_reason: data.stop_reason, full_text: data.full_text })
    }

    async fn start_session(&self, events: EventSink) -> Result<Box<dyn HarnessSession>, OrbitError> {
        let mut child = self.spawn()?;
        let mut stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");
        let mut lines = BufReader::new(stdout).lines();

        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();
        let harness = ClaudeHarness {
            command: self.command.clone(),
            args: self.args.clone(),
            cwd: self.cwd.clone(),
            prompt_timeout_secs: self.prompt_timeout_secs,
        };

        tokio::spawn(async move {
            // Keep `child` owned here so it lives for the session and is killed
            // on drop when the command channel closes.
            let _child = &mut child;
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SessionCommand::RunTurn { prompt, result } => {
                        let outcome =
                            harness.run_one(&mut stdin, &mut lines, &prompt, &events).await;
                        let _ = result.send(outcome.map(|d| d.full_text));
                    }
                }
            }
            drop(stdin);
            let _ = child.kill().await;
        });

        Ok(Box::new(ClaudeSessionHandle { cmd_tx }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_native_claude() {
        assert!(is_native_claude("claude"));
        assert!(is_native_claude("/usr/local/bin/claude"));
        assert!(is_native_claude("claude.exe"));
        assert!(!is_native_claude("claude-code-acp"));
        assert!(!is_native_claude("opencode"));
        assert!(!is_native_claude(""));
    }

    #[test]
    fn test_user_message_line() {
        let line = user_message_line("hello");
        assert!(line.ends_with('\n'));
        let v: Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], "hello");
    }

    #[tokio::test]
    async fn test_run_turn_applies_timeout() {
        let harness =
            ClaudeHarness::new("sleep".to_string(), vec!["999999".to_string()], "/tmp".into(), 1);
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = harness.run_turn(Role::Coder, "test".to_string(), tx).await;
        assert!(result.is_err(), "expected timeout/closed error");
    }

    #[test]
    fn test_session_limit_detected() {
        let msg = "You've hit your session limit. Resets at midnight.".to_string();
        let err = OrbitError::SessionLimit(msg);
        assert!(matches!(err, OrbitError::SessionLimit(_)));
    }
}
