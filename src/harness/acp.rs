use super::Harness;
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

struct SubagentConfig {
    cmd_str: String,
    cwd: PathBuf,
    task_timeout_secs: u64,
}

struct PendingTask {
    prompt: String,
}

async fn spawn_subagent(config: &SubagentConfig, task_prompt: &str) -> Result<String, OrbitError> {
    let agent: AcpAgent = config
        .cmd_str
        .parse()
        .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(format!("Failed to create subagent: {}", e)))?;

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
                            read_to_string_simple(&mut session).await
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

async fn read_to_string_simple(session: &mut ActiveSession<'_, Agent>) -> Result<String, agent_client_protocol::Error> {
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

fn auto_approve(request: &RequestPermissionRequest) -> RequestPermissionOutcome {
    match request.options.first() {
        Some(opt) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(opt.option_id.clone())),
        None => RequestPermissionOutcome::Cancelled,
    }
}

fn send_event(tx: &EventSink, event: OrbitEvent) {
    let _ = tx.send(event);
}

async fn stream_session_output(
    session: &mut ActiveSession<'_, Agent>,
    events: &EventSink,
    cwd: &std::path::Path,
    subagent: Option<&SubagentConfig>,
) -> Result<String, agent_client_protocol::Error> {
    let mut output = String::new();
    let mut line_buf = String::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tool_starts: HashMap<String, Instant> = HashMap::new();
    let mut last_tool: Option<(String, Option<String>)> = None;
    let mut pending_tasks: Vec<PendingTask> = Vec::new();

    loop {
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
                                    && !text.text.trim().is_empty() {
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
                                    && let Some(raw) = &tool.raw_input {
                                        let task_prompt = raw
                                            .get("prompt")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        pending_tasks.push(PendingTask { prompt: task_prompt.clone() });
                                        tracing::info!(
                                            id = %id,
                                            input = %crate::prompts::tail(&task_prompt, 500),
                                            "task captured for subagent execution"
                                        );
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
                                        && let Some(start) = tool_starts.remove(&id) {
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

        if pending_tasks.is_empty() || subagent.is_none() {
            return Ok(output);
        }

        let tasks = std::mem::take(&mut pending_tasks);
        let config = subagent.unwrap();
        for task in &tasks {
            tracing::info!(prompt_len = task.prompt.len(), "executing subagent task");
            match spawn_subagent(config, &task.prompt).await {
                Ok(result) => {
                    tracing::info!(result_len = result.len(), "subagent task completed");
                    if let Err(e) = session.send_prompt(result) {
                        tracing::error!(error = %e, "failed to send task result back to agent");
                        return Err(e);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "subagent task failed");
                    let err_msg = format!("Subagent task failed: {}", e);
                    if let Err(e) = session.send_prompt(err_msg) {
                        tracing::error!(error = %e, "failed to send task error back to agent");
                        return Err(e);
                    }
                }
            }
        }
    }
}

pub struct AcpHarness {
    command: String,
    args: Vec<String>,
    cwd: PathBuf,
    prompt_timeout_secs: u64,
}

impl AcpHarness {
    pub fn new(command: String, args: Vec<String>, cwd: PathBuf, prompt_timeout_secs: u64) -> Self {
        Self {
            command,
            args,
            cwd,
            prompt_timeout_secs,
        }
    }
}

#[async_trait]
impl Harness for AcpHarness {
    async fn run_turn(&self, role: Role, prompt: String, events: EventSink) -> Result<TurnOutcome, OrbitError> {
        let started = Instant::now();
        let prompt_bytes = prompt.len();
        tracing::info!(role = ?role, prompt_bytes, "turn started");
        let cmd_str = if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        };

        let agent: AcpAgent = cmd_str
            .parse()
            .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(format!("Failed to create ACP agent: {}", e)))?;

        let cwd = self.cwd.clone();
        let subagent_cfg = SubagentConfig {
            cmd_str: cmd_str.clone(),
            cwd: cwd.clone(),
            task_timeout_secs: self.prompt_timeout_secs,
        };

        let timeout_duration = Duration::from_secs(self.prompt_timeout_secs);
        let full_text = tokio::time::timeout(timeout_duration, async {
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
                .connect_with(agent, {
                    let events = events.clone();
                    async move |cx: ConnectionTo<Agent>| {
                        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
                            .block_task()
                            .await?;

                        let text = cx
                            .build_session(&cwd)
                            .block_task()
                            .run_until({
                                let cwd = cwd.clone();
                                async move |mut session| {
                                    session.send_prompt(&prompt)?;
                                    stream_session_output(&mut session, &events, &cwd, Some(&subagent_cfg)).await
                                }
                            })
                            .await?;

                        Ok(text)
                    }
                })
                .await
        })
        .await
        .map_err(|_| {
            tracing::warn!(
                role = ?role,
                timeout_secs = self.prompt_timeout_secs,
                "turn timed out"
            );
            OrbitError::Acp(format!(
                "Agent did not respond within {} seconds", self.prompt_timeout_secs
            ))
        })?
        .map_err(|e| OrbitError::Acp(e.to_string()))?;

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_run_turn_applies_timeout() {
        let harness = AcpHarness::new(
            "sleep".to_string(),
            vec!["999999".to_string()],
            PathBuf::from("/tmp"),
            1,
        );

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
}
