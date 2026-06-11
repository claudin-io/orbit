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
) -> Result<String, agent_client_protocol::Error> {
    let mut output = String::new();
    let mut line_buf = String::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut tool_starts: HashMap<String, Instant> = HashMap::new();
    let mut last_tool: Option<(String, Option<String>)> = None;

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

    Ok(output)
}

enum SessionCommand {
    RunTurn {
        prompt: String,
        result: oneshot::Sender<Result<String, OrbitError>>,
    },
}

pub struct AcpSessionHandle {
    cmd_tx: mpsc::UnboundedSender<SessionCommand>,
}

#[async_trait]
impl HarnessSession for AcpSessionHandle {
    async fn run_turn(&mut self, _role: Role, prompt: String) -> Result<TurnOutcome, OrbitError> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
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

    fn cmd_str(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }

    async fn run_turn_in_session(
        session: &mut ActiveSession<'_, Agent>,
        prompt: &str,
        events: &EventSink,
        cwd: &std::path::Path,
    ) -> Result<String, agent_client_protocol::Error> {
        session.send_prompt(prompt)?;
        read_until_stop(session, events, cwd).await
    }
}

#[async_trait]
impl Harness for AcpHarness {
    async fn run_turn(&self, role: Role, prompt: String, events: EventSink) -> Result<TurnOutcome, OrbitError> {
        let started = Instant::now();
        let prompt_bytes = prompt.len();
        tracing::info!(role = ?role, prompt_bytes, "turn started");

        let cmd_str = self.cmd_str();
        let cwd = self.cwd.clone();
        let timeout = Duration::from_secs(self.prompt_timeout_secs);

        let agent: AcpAgent = cmd_str
            .parse()
            .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(format!("Failed to create ACP agent: {e}")))?;

        let full_text = tokio::time::timeout(timeout, async {
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
                                let events = events.clone();
                                let cwd = cwd.clone();
                                async move |mut session| {
                                    Self::run_turn_in_session(&mut session, &prompt, &events, &cwd).await
                                }
                            })
                            .await?;

                        Ok(text)
                    }
                })
                .await
        })
        .await
        .map_err(|_| OrbitError::Acp(format!("Agent did not respond within {} seconds", self.prompt_timeout_secs)))?
        .map_err(|e: agent_client_protocol::Error| OrbitError::Acp(e.to_string()))?;

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
                                            let text = match Self::run_turn_in_session(
                                                &mut session, &prompt, &events, &cwd,
                                            )
                                            .await
                                            {
                                                Ok(t) => t,
                                                Err(e) => {
                                                    let _ = result.send(Err(OrbitError::Acp(e.to_string())));
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

        Ok(Box::new(AcpSessionHandle { cmd_tx }))
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
