use super::{Harness, HarnessSession};
use crate::error::OrbitError;
use crate::events::EventSink;
use crate::types::{OrbitEvent, Role, TurnOutcome};
use crate::tool_format;
use agent_client_protocol::role::acp::Client;
use agent_client_protocol::schema::{
    ContentBlock, InitializeRequest, ProtocolVersion, RequestPermissionOutcome, RequestPermissionRequest,
    RequestPermissionResponse, SelectedPermissionOutcome, SessionNotification, SessionUpdate, ToolCallStatus,
};
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::AcpAgent;
use agent_client_protocol::ActiveSession;
use agent_client_protocol::{on_receive_request, Agent, ConnectionTo, Responder, SessionMessage};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

#[derive(Clone)]
struct SubagentConfig {
    cmd_str: String,
    cwd: PathBuf,
    task_timeout_secs: u64,
}

struct PendingTask {
    prompt: String,
}

async fn spawn_subagent_task(config: &SubagentConfig, task_prompt: &str) -> Result<String, OrbitError> {
    tracing::debug!(
        command = %config.cmd_str,
        cwd = %config.cwd.display(),
        prompt_len = task_prompt.len(),
        "spawning subagent task"
    );
    let agent: AcpAgent = config
        .cmd_str
        .parse()
        .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(format!("Failed to create subagent: {e}")))?;

    let cwd = config.cwd.clone();
    let timeout = Duration::from_secs(config.task_timeout_secs);
    tokio::time::timeout(timeout, async {
        Client
            .builder()
            .on_receive_request(
                async move |request: RequestPermissionRequest,
                            responder: Responder<RequestPermissionResponse>,
                            _cx: ConnectionTo<Agent>| {
                    let outcome = auto_approve(&request);
                    responder.respond(RequestPermissionResponse::new(outcome))
                },
                on_receive_request!(),
            )
            .connect_with(agent, async move |cx: ConnectionTo<Agent>| {
                cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                    .block_task()
                    .await?;

                let text = cx
                    .build_session(&cwd)
                    .block_task()
                    .run_until({
                        let task_prompt = task_prompt.to_string();
                        async move |mut session| {
                            session.send_prompt(&task_prompt)?;
                            let mut output = String::new();
                            loop {
                                let update = session.read_update().await?;
                                match update {
                                    SessionMessage::SessionMessage(dispatch) => {
                                        MatchDispatch::new(dispatch)
                                            .if_notification(async |notif: SessionNotification| {
                                                if let SessionUpdate::AgentMessageChunk(chunk) = &notif.update
                                                    && let ContentBlock::Text(text) = &chunk.content {
                                                        output.push_str(&text.text);
                                                    }
                                                Ok(())
                                            })
                                            .await
                                            .otherwise_ignore()?;
                                    }
                                    SessionMessage::StopReason(_) => break,
                                    _ => {}
                                }
                            }
                            Ok(output)
                        }
                    })
                    .await?;

                Ok(text)
            })
            .await
    })
    .await
    .map_err(|_| {
        OrbitError::Acp(format!(
            "Subagent task did not respond within {} seconds",
            config.task_timeout_secs
        ))
    })?
    .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(e.to_string()))
}

fn auto_approve(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    match request.options.first() {
        Some(opt) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(opt.option_id.clone())),
        None => RequestPermissionOutcome::Cancelled,
    }
}

fn send_event(tx: &EventSink, event: OrbitEvent) {
    let _ = tx.send(event);
}

async fn read_until_stop(
    session: &mut ActiveSession<'_, Agent>,
    events: &EventSink,
    cwd: &std::path::Path,
) -> Result<ReadResult, agent_client_protocol::Error> {
    let mut output = String::new();
    let mut line_buf = String::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tool_starts: HashMap<String, Instant> = HashMap::new();
    let mut last_tool: Option<(String, Option<String>)> = None;
    let mut pending_tasks: Vec<PendingTask> = Vec::new();

    loop {
        let update = session.read_update().await?;
        match update {
            SessionMessage::SessionMessage(dispatch) => {
                MatchDispatch::new(dispatch)
                    .if_notification(async |notif: SessionNotification| {
                        match &notif.update {
                            SessionUpdate::AgentMessageChunk(chunk) => {
                                if let ContentBlock::Text(text) = &chunk.content {
                                    output.push_str(&text.text);
                                    line_buf.push_str(&text.text);
                                    while let Some(pos) = line_buf.find('\n') {
                                        let complete = line_buf[..pos].to_string();
                                        if !complete.is_empty() {
                                            send_event(events, OrbitEvent::AgentChunk(complete));
                                        }
                                        line_buf.drain(..=pos);
                                    }
                                }
                            }
                            SessionUpdate::AgentThoughtChunk(chunk) => {
                                if let ContentBlock::Text(text) = &chunk.content
                                    && !text.text.trim().is_empty()
                                {
                                    send_event(events, OrbitEvent::AgentChunk("[thought] ".to_string()));
                                }
                            }
                            SessionUpdate::ToolCall(tool) => {
                                let id = tool.tool_call_id.to_string();
                                if !tool.title.is_empty() {
                                    tool_names.insert(id.clone(), tool.title.clone());
                                }
                                tool_starts.insert(id.clone(), Instant::now());
                                let name = tool.title.clone();
                                let params = tool_format::fmt_tool_call(&name, &tool.raw_input, cwd);
                                let emit = (name.clone(), params.clone());
                                if params.is_some() && !name.is_empty() && last_tool.as_ref() != Some(&emit) {
                                    last_tool = Some(emit);
                                    send_event(events, OrbitEvent::ToolCall {
                                        name: name.clone(),
                                        params,
                                        raw_input: tool.raw_input.clone(),
                                    });
                                }
                                tracing::info!(tool = %tool.title, id = %id, "tool started");

                                if tool.title.to_lowercase().contains("task")
                                    && let Some(raw) = &tool.raw_input
                                {
                                    let task_prompt = raw
                                        .get("prompt")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("")
                                        .to_string();
                                    tracing::info!(
                                        id = %id,
                                        input_len = task_prompt.len(),
                                        "task captured for subagent execution"
                                    );
                                    pending_tasks.push(PendingTask { prompt: task_prompt });
                                }
                            }
                            SessionUpdate::ToolCallUpdate(up) => {
                                let id = up.tool_call_id.to_string();
                                let name = tool_names
                                    .get(&id)
                                    .cloned()
                                    .or_else(|| up.fields.title.clone().filter(|t| !t.is_empty()))
                                    .unwrap_or_default();
                                if let Some(status) = &up.fields.status
                                    && matches!(status, ToolCallStatus::Completed | ToolCallStatus::Failed)
                                    && let Some(start) = tool_starts.remove(&id)
                                {
                                    tracing::info!(
                                        tool = %name,
                                        id = %id,
                                        status = ?status,
                                        duration_secs = start.elapsed().as_secs_f64(),
                                        "tool finished"
                                    );
                                }
                                let params = tool_format::fmt_tool_call(&name, &up.fields.raw_input, cwd);
                                let emit = (name.clone(), params.clone());
                                if params.is_some() && !name.is_empty() && last_tool.as_ref() != Some(&emit) {
                                    last_tool = Some(emit);
                                    send_event(events, OrbitEvent::ToolCall {
                                        name,
                                        params,
                                        raw_input: up.fields.raw_input.clone(),
                                    });
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    })
                    .await
                    .otherwise_ignore()?;
            }
            SessionMessage::StopReason(_) => {
                let remaining = line_buf.trim().to_string();
                if !remaining.is_empty() {
                    send_event(events, OrbitEvent::AgentChunk(remaining));
                }
                break;
            }
            _ => {}
        }
    }

    Ok(ReadResult { output, pending_tasks })
}

struct ReadResult {
    output: String,
    pending_tasks: Vec<PendingTask>,
}

async fn read_until_tasks_complete(
    session: &mut ActiveSession<'_, Agent>,
    prompt: &str,
    events: &EventSink,
    cwd: &std::path::Path,
    subagent: Option<&SubagentConfig>,
) -> Result<String, agent_client_protocol::Error> {
    session.send_prompt(prompt)?;
    let mut full_output = String::new();

    loop {
        let read_result = read_until_stop(session, events, cwd).await?;
        full_output.push_str(&read_result.output);

        if read_result.pending_tasks.is_empty() || subagent.is_none() {
            if full_output.is_empty() {
                full_output = read_result.output;
            }
            return Ok(full_output);
        }

        tracing::info!(
            task_count = read_result.pending_tasks.len(),
            "processing subagent tasks"
        );

        for task in &read_result.pending_tasks {
            tracing::info!(prompt_len = task.prompt.len(), "executing subagent task");
            let result = match spawn_subagent_task(subagent.unwrap(), &task.prompt).await {
                Ok(text) => text,
                Err(e) => format!("Subagent task failed: {e}"),
            };
            tracing::info!(result_len = result.len(), "subagent task completed, feeding back");
            session.send_prompt(result)?;
        }

        let follow_up = read_until_stop(session, events, cwd).await?;
        full_output.push_str(&follow_up.output);
    }
}

enum SessionCommand {
    RunTurn {
        prompt: String,
        result: oneshot::Sender<Result<String, OrbitError>>,
    },
}

pub struct AcpSessionHandle {
    cmd_tx: Option<mpsc::UnboundedSender<SessionCommand>>,
    cmd_str: String,
    cwd: PathBuf,
    timeout_secs: u64,
    events: EventSink,
    subagent: SubagentConfig,
}

fn spawn_session_task(
    cmd_str: String,
    cwd: PathBuf,
    timeout_secs: u64,
    events: EventSink,
    subagent: SubagentConfig,
) -> mpsc::UnboundedSender<SessionCommand> {
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();

    tokio::spawn(async move {
        let agent: AcpAgent = match cmd_str.parse() {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to create ACP agent: {e}");
                return;
            }
        };

        let timeout = Duration::from_secs(timeout_secs);
        let _ = tokio::time::timeout(timeout, async {
            let _ = Client
                .builder()
                .on_receive_request(
                    async move |request: RequestPermissionRequest,
                                responder: Responder<RequestPermissionResponse>,
                                _cx: ConnectionTo<Agent>| {
                        let outcome = auto_approve(&request);
                        responder.respond(RequestPermissionResponse::new(outcome))
                    },
                    on_receive_request!(),
                )
                .connect_with(agent, async move |cx: ConnectionTo<Agent>| {
                    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await
                        .map_err(|e| {
                            tracing::error!("Session init failed: {e}");
                            e
                        })?;

                    let _ = cx
                        .build_session(&cwd)
                        .block_task()
                        .run_until(async move |mut session| {
                            while let Some(cmd) = cmd_rx.recv().await {
                                match cmd {
                                    SessionCommand::RunTurn { prompt, result } => {
                                        let text = match AcpHarness::run_turn_in_session(
                                            &mut session,
                                            &prompt,
                                            &events,
                                            &cwd,
                                            Some(&subagent),
                                        )
                                        .await
                                        {
                                            Ok(t) => t,
                                            Err(e) => {
                                                let _ =
                                                    result.send(Err(OrbitError::Acp(e.to_string())));
                                                continue;
                                            }
                                        };
                                        let _ = result.send(Ok(text));
                                    }
                                }
                            }
                            Ok::<_, agent_client_protocol::Error>(())
                        })
                        .await;

                    Ok(())
                })
                .await;
        })
        .await;
    });

    cmd_tx
}

#[async_trait]
impl HarnessSession for AcpSessionHandle {
    async fn run_turn(&mut self, _role: Role, prompt: String) -> Result<TurnOutcome, OrbitError> {
        if self.cmd_tx.is_none() || self.cmd_tx.as_ref().unwrap().is_closed() {
            tracing::warn!("ACP session task died, re-spawning...");
            send_event(&self.events, OrbitEvent::Notice {
                message: "ACP session task died, re-spawning...".to_string(),
            });
            self.cmd_tx = Some(spawn_session_task(
                self.cmd_str.clone(),
                self.cwd.clone(),
                self.timeout_secs,
                self.events.clone(),
                self.subagent.clone(),
            ));
        }

        let cmd_tx = self.cmd_tx.as_ref().unwrap();
        let (tx, rx) = oneshot::channel();
        cmd_tx
            .send(SessionCommand::RunTurn { prompt, result: tx })
            .map_err(|_| OrbitError::Acp("session task has terminated".to_string()))?;
        let full_text = rx
            .await
            .map_err(|_| OrbitError::Acp("session task has terminated".to_string()))?
            .map_err(|e| e)?;
        Ok(TurnOutcome {
            stop_reason: "end_turn".to_string(),
            full_text,
        })
    }
}

pub struct AcpHarness {
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    prompt_timeout_secs: u64,
    task_timeout_secs: u64,
    retry_max_attempts: u32,
    retry_base_delay: Duration,
}

impl AcpHarness {
    pub fn new(
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        prompt_timeout_secs: u64,
        retry_max_attempts: u32,
        retry_base_delay_ms: u64,
    ) -> Self {
        Self {
            command,
            args,
            cwd,
            prompt_timeout_secs,
            task_timeout_secs: prompt_timeout_secs,
            retry_max_attempts,
            retry_base_delay: Duration::from_millis(retry_base_delay_ms),
        }
    }

    fn cmd_str(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }

    fn subagent_config(&self) -> SubagentConfig {
        SubagentConfig {
            cmd_str: self.cmd_str(),
            cwd: self.cwd.clone(),
            task_timeout_secs: self.task_timeout_secs,
        }
    }

    fn is_transient_error(err: &OrbitError) -> bool {
        match err {
            OrbitError::Acp(msg) => {
                // Command parse failures are fatal — retrying won't change the command string.
                if msg.starts_with("Failed to create") {
                    return false;
                }
                // Everything else (connection drops, init failures, read errors, timeouts) is transient.
                true
            }
            _ => false,
        }
    }

    async fn run_turn_with_retry(
        &self,
        prompt: String,
        events: EventSink,
    ) -> Result<String, OrbitError> {
        let mut last_error: Option<OrbitError> = None;
        for attempt in 1..=self.retry_max_attempts {
            match self.run_turn_once(prompt.clone(), events.clone()).await {
                Ok(text) => return Ok(text),
                Err(e) if Self::is_transient_error(&e) && attempt < self.retry_max_attempts => {
                    tracing::warn!(
                        attempt,
                        max_attempts = self.retry_max_attempts,
                        error = %e,
                        "ACP transient error, retrying with backoff"
                    );
                    send_event(&events, OrbitEvent::Notice {
                        message: format!(
                            "ACP transient error (attempt {}/{}): {}. Retrying...",
                            attempt, self.retry_max_attempts, e,
                        ),
                    });
                    let delay = self.retry_base_delay * attempt as u32;
                    tokio::time::sleep(delay).await;
                    last_error = Some(e);
                }
                Err(e) => return Err(e),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            OrbitError::Acp("retry exhausted with no preceding error".to_string())
        }))
    }

    async fn run_turn_in_session(
        session: &mut ActiveSession<'_, Agent>,
        prompt: &str,
        events: &EventSink,
        cwd: &std::path::Path,
        subagent: Option<&SubagentConfig>,
    ) -> Result<String, agent_client_protocol::Error> {
        read_until_tasks_complete(session, prompt, events, cwd, subagent).await
    }

    async fn run_turn_once(
        &self,
        prompt: String,
        events: EventSink,
    ) -> Result<String, OrbitError> {
        let cmd_str = self.cmd_str();
        let cwd = self.cwd.clone();
        let timeout = Duration::from_secs(self.prompt_timeout_secs);
        let subagent = self.subagent_config();

        tracing::debug!(
            command = %cmd_str,
            cwd = %cwd.display(),
            timeout_secs = self.prompt_timeout_secs,
            "spawning ACP agent (one-shot turn)"
        );

        let agent: AcpAgent = match cmd_str.parse() {
            Ok(a) => a,
            Err(e) => return Err(OrbitError::Acp(format!("Failed to create ACP agent: {e}"))),
        };

        tokio::time::timeout(timeout, async {
            Client
                .builder()
                .on_receive_request(
                    async move |request: RequestPermissionRequest,
                                responder: Responder<RequestPermissionResponse>,
                                _cx: ConnectionTo<Agent>| {
                        let outcome = auto_approve(&request);
                        responder.respond(RequestPermissionResponse::new(outcome))
                    },
                    on_receive_request!(),
                )
                .connect_with(agent, async move |cx: ConnectionTo<Agent>| {
                    cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                        .block_task()
                        .await?;

                    let text = cx
                        .build_session(&cwd)
                        .block_task()
                        .run_until({
                            let prompt = prompt.clone();
                            let events = events.clone();
                            let cwd = cwd.clone();
                            async move |mut session| {
                                Self::run_turn_in_session(
                                    &mut session, &prompt, &events, &cwd, Some(&subagent),
                                )
                                .await
                            }
                        })
                        .await?;

                    Ok(text)
                })
                .await
        })
        .await
        .map_err(|_| OrbitError::Acp(format!("Agent did not respond within {} seconds", self.prompt_timeout_secs)))?
        .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(e.to_string()))
    }
}

#[async_trait]
impl Harness for AcpHarness {
    async fn run_turn(&self, role: Role, prompt: String, events: EventSink) -> Result<TurnOutcome, OrbitError> {
        let started = Instant::now();
        let prompt_bytes = prompt.len();
        tracing::info!(role = ?role, prompt_bytes, "turn started");

        let full_text = self.run_turn_with_retry(prompt, events).await?;

        tracing::info!(
            role = ?role,
            prompt_bytes,
            duration_secs = started.elapsed().as_secs_f64(),
            "turn finished"
        );

        Ok(TurnOutcome {
            stop_reason: "end_turn".to_string(),
            full_text,
        })
    }

    async fn start_session(&self, events: EventSink) -> Result<Box<dyn HarnessSession>, OrbitError> {
        let cmd_str = self.cmd_str();
        let cwd = self.cwd.clone();
        let timeout_secs = self.prompt_timeout_secs;
        let subagent = self.subagent_config();

        tracing::debug!(
            command = %cmd_str,
            cwd = %cwd.display(),
            timeout_secs,
            "starting persistent ACP session"
        );

        let cmd_tx = spawn_session_task(
            cmd_str.clone(),
            cwd.clone(),
            timeout_secs,
            events.clone(),
            subagent.clone(),
        );

        Ok(Box::new(AcpSessionHandle {
            cmd_tx: Some(cmd_tx),
            cmd_str,
            cwd,
            timeout_secs,
            events,
            subagent,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_harness(timeout_secs: u64) -> AcpHarness {
        AcpHarness::new(
            "sleep".to_string(),
            vec!["999999".to_string()],
            PathBuf::from("/tmp"),
            timeout_secs,
            3,
            50,
        )
    }

    #[tokio::test]
    async fn test_run_turn_applies_timeout() {
        let harness = test_harness(1);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = harness.run_turn(Role::Coder, "test prompt".to_string(), tx).await;

        assert!(result.is_err(), "Expected timeout error from run_turn");
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("timeout") || err_msg.contains("did not respond"),
            "Expected timeout-related error message, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_retry_succeeds_after_transient_failures() {
        let harness = AcpHarness::new(
            "sleep".to_string(),
            vec!["999999".to_string()],
            PathBuf::from("/tmp"),
            1,  // short timeout — each attempt will time out
            3,  // retry up to 3 times
            10, // 10ms base delay (fast for tests)
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = harness.run_turn(Role::Coder, "test prompt".to_string(), tx).await;

        // After 3 transient failures, should still get a timeout error
        assert!(result.is_err(), "Expected error after exhausting retries");
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("timeout") || err_msg.contains("did not respond"),
            "Expected timeout-related error message, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_fatal_error_not_retried() {
        // A command that fails to parse should fail immediately without retry
        let harness = AcpHarness::new(
            "".to_string(),
            vec![],
            PathBuf::from("/tmp"),
            10,
            3,
            50,
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = harness.run_turn(Role::Coder, "test".to_string(), tx).await;

        assert!(result.is_err(), "Expected error from empty command");
        let err = result.unwrap_err();
        let err_msg = err.to_string();
        assert!(
            err_msg.contains("Failed to create"),
            "Expected fatal parse error, got: {err_msg}"
        );
    }

    #[tokio::test]
    async fn test_session_recovers_after_death() {
        let harness = test_harness(5);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = harness.start_session(tx).await.unwrap();

        // First turn — the session task is alive but the agent won't respond.
        let first = session.run_turn(Role::Coder, "turn 1".to_string()).await;
        assert!(first.is_err(), "Expected error from unresponsive agent");

        // Second turn — even if the spawned task died from the timeout,
        // the handle should re-spawn and attempt a new connection.
        // The re-spawn will also time out (the agent is still unresponsive),
        // but crucially the call does not hang forever or panic.
        let second = session.run_turn(Role::Coder, "turn 2".to_string()).await;
        assert!(second.is_err(), "Expected error on second turn after re-spawn");
    }

    #[tokio::test]
    async fn test_retry_count_respected() {
        // With retry_max_attempts = 1, only one attempt is made (no retry).
        let harness = AcpHarness::new(
            "sleep".to_string(),
            vec!["999999".to_string()],
            PathBuf::from("/tmp"),
            1, // quick timeout
            1, // only 1 attempt = no retry
            50,
        );

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let result = harness.run_turn(Role::Coder, "test".to_string(), tx).await;

        assert!(result.is_err(), "Expected error with retry_max_attempts=1");
    }

    #[test]
    fn test_is_transient_error() {
        let fatal = OrbitError::Acp("Failed to create ACP agent: parse error".to_string());
        assert!(!AcpHarness::is_transient_error(&fatal), "parse error should be fatal");

        let fatal2 = OrbitError::Acp("Failed to create subagent: connection refused".to_string());
        assert!(!AcpHarness::is_transient_error(&fatal2), "subagent create error should be fatal");

        let transient = OrbitError::Acp("Agent did not respond within 30 seconds".to_string());
        assert!(AcpHarness::is_transient_error(&transient), "timeout should be transient");

        let transient2 = OrbitError::Acp("connection reset".to_string());
        assert!(AcpHarness::is_transient_error(&transient2), "connection error should be transient");

        let transient3 = OrbitError::Acp("read_update failed: broken pipe".to_string());
        assert!(AcpHarness::is_transient_error(&transient3), "read error should be transient");

        let non_acp = OrbitError::Other("logic error".to_string());
        assert!(!AcpHarness::is_transient_error(&non_acp), "non-ACP error should not be retried");
    }

    #[test]
    fn test_is_transient_error_empty_cmd() {
        // Empty command string produces "Failed to create ACP agent: ..."
        let err = OrbitError::Acp("Failed to create ACP agent: empty command".to_string());
        assert!(!AcpHarness::is_transient_error(&err));
    }
}
