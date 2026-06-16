use crate::cli::{self, AcpAction, Command};
use crate::cli::Cli;
use crate::config;
use crate::error::OrbitError;
use crate::events::{EventSink, emit};
use crate::harness::SessionRouter;
use crate::prompts::{EVALUATOR_TEMPLATE, PROMPTER_REVISION_TEMPLATE, PROMPTER_TEMPLATE, extract_fenced_json, extract_json_object, render, sanitize_llm_json};
use crate::types::{EvalVerdict, OrbitEvent, PrompterOutput, Role, RunPhase, TurnOutcome};

pub async fn dispatch(cli: Cli, events: EventSink) -> Result<(), OrbitError> {
    match &cli.command {
        Command::Run { .. } => {
            let run_config = cli::resolve_config(&cli)?;
            run_simple_loop(run_config, events).await
        }
        Command::Git { action } => crate::git::dispatch(action, events).await,
        Command::Config => crate::config_wizard::run_wizard(events).await,
        Command::Acp { action } => match action {
            AcpAction::SetDefault { command } => {
                let config_path = config::home_config_path()
                    .ok_or_else(|| OrbitError::Config("HOME not set".to_string()))?;
                config::save_acp_default(&config_path, command)?;
                println!("Saved default ACP command: {}", command);
                Ok(())
            }
            AcpAction::Handshake => {
                let target = std::env::current_dir().unwrap_or_default();
                let config = config::load(None, &target);
                let harness = crate::harness::make_harness(
                    &config.harness,
                    &target,
                    config.r#loop.prompt_timeout_secs,
                    config.r#loop.acp_retry_max_attempts,
                    config.r#loop.acp_retry_base_delay_ms,
                );
                println!(
                    "Connecting to ACP agent: {} {:?}",
                    config.harness.command, config.harness.args
                );
                match harness
                    .run_turn(Role::Coder, "Hello, respond with exactly: OK".to_string(), events)
                    .await
                {
                    Ok(outcome) => {
                        println!("ACP connection OK");
                        println!("Response: {}", outcome.full_text.trim());
                        Ok(())
                    }
                    Err(e) => {
                        println!("ACP connection FAILED: {}", e);
                        Err(e)
                    }
                }
            }
        },
    }
}

fn extract_goal(prompt_text: &str) -> String {
    for line in prompt_text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**Goal:**") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    for line in prompt_text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Goal") && trimmed.contains(':') {
            let rest = trimmed.splitn(2, ':').nth(1).unwrap_or("").trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    "See prompt for details".to_string()
}

fn rubrics_to_display(rubric: &[crate::types::RubricItem]) -> String {
    rubric
        .iter()
        .map(|r| format!("- {} (weight {}): {}", r.criterion, r.weight, r.description))
        .collect::<Vec<_>>()
        .join("\n")
}

async fn run_simple_loop(run_config: config::RunConfig, events: EventSink) -> Result<(), OrbitError> {
    let target = run_config.target;
    let config = run_config.config;
    let spec_path = run_config.spec_path;
    let spec_content = std::fs::read_to_string(&spec_path).map_err(OrbitError::Io)?;

    if spec_content.trim().is_empty() {
        eprintln!("Spec is empty. Nothing to do.");
        return Ok(());
    }

    emit!(
        events,
        OrbitEvent::RunStarted {
            spec_path: spec_path.to_string_lossy().to_string(),
            target: target.to_string_lossy().to_string(),
        }
    );

    let max_attempts = config.r#loop.max_attempts;
    let mut router = SessionRouter::new(config, target.clone(), events.clone());

    emit!(events, OrbitEvent::PhaseChanged(RunPhase::Prompting));

    let prompter_output = {
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("spec", &spec_content[..]);
        let prompt = render(PROMPTER_TEMPLATE, &ctx);
        tracing::debug!(role = ?Role::Prompter, prompt_len = prompt.len(), prompt = %prompt, "sending prompt");
        let outcome = run_turn_or_fallback(&mut router, Role::Prompter, prompt, events.clone()).await?;
        tracing::debug!(role = ?Role::Prompter, output_len = outcome.full_text.len(), output = %outcome.full_text, "agent output");
        parse_prompter_output(&outcome.full_text)?
    };
    let prompt_summary = extract_goal(&prompter_output.prompt);
    tracing::debug!(
        goal = %prompt_summary,
        rubric_items = prompter_output.rubric.len(),
        "prompter produced goal and rubric"
    );

    emit!(
        events,
        OrbitEvent::PromptCreated {
            prompt_summary: prompt_summary.clone(),
            rubric: prompter_output.rubric.clone(),
        }
    );

    let rubric_text = rubrics_to_display(&prompter_output.rubric);
    let mut prompt = prompter_output.prompt;

    for attempt in 1..=max_attempts {
        emit!(events, OrbitEvent::PhaseChanged(RunPhase::Coding));
        emit!(
            events,
            OrbitEvent::AttemptStarted {
                attempt,
                max_attempts,
            }
        );

        tracing::debug!(role = ?Role::Coder, attempt, prompt_len = prompt.len(), prompt = %prompt, "sending prompt");
        let coder_outcome =
            run_turn_or_fallback(&mut router, Role::Coder, prompt.clone(), events.clone()).await?;
        let coder_text = coder_outcome.full_text;
        tracing::debug!(role = ?Role::Coder, attempt, output_len = coder_text.len(), output = %coder_text, "agent output");

        let coder_summary = summarize_coder_output(&coder_text);
        emit!(events, OrbitEvent::CoderOutput {
            summary: coder_summary.clone(),
        });

        emit!(events, OrbitEvent::PhaseChanged(RunPhase::Evaluating));

        let eval_outcome = {
            let mut ctx = std::collections::HashMap::new();
            ctx.insert("spec", &spec_content[..]);
            ctx.insert("rubric", &rubric_text);
            ctx.insert("coder_output", &coder_text);
            let prompt = render(EVALUATOR_TEMPLATE, &ctx);
            tracing::debug!(role = ?Role::Evaluator, prompt_len = prompt.len(), prompt = %prompt, "sending prompt");
            let outcome = run_turn_or_fallback(&mut router, Role::Evaluator, prompt, events.clone()).await?;
            tracing::debug!(role = ?Role::Evaluator, output_len = outcome.full_text.len(), output = %outcome.full_text, "agent output");
            parse_eval_verdict(&outcome.full_text)?
        };
        let results = eval_outcome.results.clone();

        if eval_outcome.approved {
            emit!(
                events,
                OrbitEvent::EvalVerdict {
                    approved: true,
                    feedback: eval_outcome.feedback.clone(),
                    diagnosis: eval_outcome.diagnosis.clone(),
                    results: results.clone(),
                }
            );
            emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
            emit!(events, OrbitEvent::RunFinished { exit_code: 0 });
            println!("  ✓ Task completed successfully.");
            return Ok(());
        }

            emit!(
                events,
                OrbitEvent::EvalVerdict {
                    approved: false,
                    feedback: eval_outcome.feedback.clone(),
                    diagnosis: eval_outcome.diagnosis.clone(),
                    results: results.clone(),
                }
            );

        if attempt < max_attempts {
            emit!(events, OrbitEvent::PhaseChanged(RunPhase::Prompting));
            let mut ctx = std::collections::HashMap::new();
            ctx.insert("spec", &spec_content[..]);
            ctx.insert("coder_output", &coder_text);
            ctx.insert("eval_feedback", &eval_outcome.feedback);
            ctx.insert("eval_diagnosis", &eval_outcome.diagnosis);
            let revision_prompt = render(PROMPTER_REVISION_TEMPLATE, &ctx);
            tracing::debug!(
                role = ?Role::Prompter,
                prompt_len = revision_prompt.len(),
                eval_feedback = eval_outcome.feedback,
                eval_diagnosis = eval_outcome.diagnosis,
                "sending prompt (revision)"
            );
            let outcome = run_turn_or_fallback(&mut router, Role::Prompter, revision_prompt, events.clone()).await?;
            tracing::debug!(role = ?Role::Prompter, output_len = outcome.full_text.len(), output = %outcome.full_text, "agent output");
            let parsed = parse_prompter_output(&outcome.full_text)?;
            prompt = parsed.prompt;
        }
    }

    emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
    let reason = format!(
        "Implementation did not pass evaluation after {} attempts",
        max_attempts
    );
    emit!(events, OrbitEvent::RunFailed { reason: reason.clone() });
    Err(OrbitError::Exhausted(reason))
}

/// Run a turn, handling [`OrbitError::SessionLimit`] by warning the user and
/// offering to switch to the fallback harness. Retries once on confirmation.
async fn run_turn_or_fallback(
    router: &mut SessionRouter,
    role: Role,
    prompt: String,
    events: EventSink,
) -> Result<TurnOutcome, OrbitError> {
    let session = router.session_for(role).await?;
    match session.run_turn(role, prompt.clone()).await {
        Ok(outcome) => Ok(outcome),
        Err(OrbitError::SessionLimit(detail)) => {
            emit!(events, OrbitEvent::Notice {
                message: format!("⚠ Session limit reached: {detail}"),
            });

            let (confirm_tx, confirm_rx) = tokio::sync::oneshot::channel();
            let confirm_event = OrbitEvent::ConfirmRequest {
                message: "Session limit hit. Switch to fallback harness and retry?".to_string(),
                default: true,
                tx: confirm_tx,
            };
            let _ = events.send(confirm_event);
            let confirmed = confirm_rx.await.unwrap_or(true);

            if confirmed {
                router.fallback_for(role)?;
                emit!(events, OrbitEvent::Notice {
                    message: "Switched to fallback harness. Retrying turn...".to_string(),
                });
                let session = router.session_for(role).await?;
                session.run_turn(role, prompt).await
            } else {
                Err(OrbitError::SessionLimit(detail))
            }
        }
        Err(e) => Err(e),
    }
}

fn summarize_coder_output(text: &str) -> String {
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = non_empty.len();

    let mut summary_parts: Vec<String> = Vec::new();

    let created: Vec<&&str> = non_empty
        .iter()
        .filter(|l| l.trim().starts_with("Created") || l.trim().starts_with("Written"))
        .collect();
    if !created.is_empty() {
        summary_parts.push(format!("{} files created", created.len()));
    }

    let modified: Vec<&&str> = non_empty
        .iter()
        .filter(|l| l.trim().starts_with("Modified") || l.trim().starts_with("Updated"))
        .collect();
    if !modified.is_empty() {
        summary_parts.push(format!("{} files modified", modified.len()));
    }

    if summary_parts.is_empty() {
        format!("{} lines of output", total)
    } else {
        summary_parts.join(", ")
    }
}

fn dump_debug_agent_output(text: &str, label: &str) {
    use std::io::Write;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S");
    let dir = std::env::current_dir().ok().map(|p| p.join(".orbit").join("debug"));
    if let Some(d) = dir {
        let _ = std::fs::create_dir_all(&d);
        let path = d.join(format!("agent-dump-{}-{}.log", label, ts));
        let mut f = match std::fs::File::create(&path) {
            Ok(f) => f,
            Err(_) => return,
        };
        let _ = write!(f, "--- {} AGENT OUTPUT DUMP ---\n\n", label.to_uppercase());
        let _ = write!(f, "{}", text);
        let _ = write!(f, "\n\n--- END DUMP ---\n");
        tracing::error!(path = %path.display(), "agent output dump saved");
    }
}

fn parse_prompter_output(text: &str) -> Result<PrompterOutput, OrbitError> {
    let raw = text.trim();
    let raw = sanitize_llm_json(raw);
    let json_str = extract_fenced_json(&raw)
        .or_else(|| {
            let t = raw.trim();
            if t.starts_with('{') {
                Some(t)
            } else {
                None
            }
        })
        .or_else(|| extract_json_object(&raw));
    let json_str = match json_str {
        Some(s) => s,
        None => {
            dump_debug_agent_output(text, "prompter");
            return Err(OrbitError::Parse("No JSON found in prompter output".to_string()));
        }
    };
    let output: PrompterOutput =
        serde_json::from_str(json_str).map_err(|e| {
            dump_debug_agent_output(text, "prompter");
            OrbitError::Parse(format!("Failed to parse prompter JSON: {}", e))
        })?;
    if output.prompt.is_empty() {
        return Err(OrbitError::Parse("Prompter returned empty prompt".to_string()));
    }
    Ok(output)
}

fn parse_eval_verdict(text: &str) -> Result<EvalVerdict, OrbitError> {
    let raw = text.trim();
    let raw = sanitize_llm_json(raw);
    let json_str = extract_fenced_json(&raw)
        .or_else(|| {
            let t = raw.trim();
            if t.starts_with('{') {
                Some(t)
            } else {
                None
            }
        })
        .or_else(|| extract_json_object(&raw));
    let json_str = match json_str {
        Some(s) => s,
        None => {
            dump_debug_agent_output(text, "evaluator");
            return Err(OrbitError::Parse("No JSON found in evaluator output".to_string()));
        }
    };
    let verdict: EvalVerdict =
        serde_json::from_str(json_str).map_err(|e| {
            dump_debug_agent_output(text, "evaluator");
            OrbitError::Parse(format!("Failed to parse evaluator JSON: {}", e))
        })?;
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_prompter_output_valid() {
        let json = "```json\n{\"prompt\": \"Goal: Implement X\", \"rubric\": [{\"criterion\": \"C1\", \"description\": \"d\", \"weight\": 1}]}\n```";
        let result = parse_prompter_output(json).unwrap();
        assert_eq!(result.prompt, "Goal: Implement X");
        assert_eq!(result.rubric.len(), 1);
    }

    #[test]
    fn test_parse_prompter_output_no_fence_still_works() {
        let text = "{\"prompt\": \"Goal: Implement X\", \"rubric\": []}";
        let result = parse_prompter_output(text).unwrap();
        assert_eq!(result.prompt, "Goal: Implement X");
    }

    #[test]
    fn test_parse_prompter_output_empty_prompt() {
        let text = r#"{"prompt": "", "rubric": []}"#;
        assert!(parse_prompter_output(text).is_err());
    }

    #[test]
    fn test_parse_eval_verdict_approved() {
        let text = r#"```json
{
  "approved": true,
  "feedback": "ok",
  "diagnosis": "all criteria met"
}
```"#;
        let result = parse_eval_verdict(text).unwrap();
        assert!(result.approved);
    }

    #[test]
    fn test_parse_eval_verdict_rejected() {
        let text = r#"```json
{
  "approved": false,
  "feedback": "missing error handling",
  "diagnosis": "edge case not covered"
}
```"#;
        let result = parse_eval_verdict(text).unwrap();
        assert!(!result.approved);
        assert_eq!(result.feedback, "missing error handling");
    }

    #[test]
    fn test_parse_eval_verdict_no_fence() {
        let text = r#"{"approved": false, "feedback": "no", "diagnosis": "bad"}"#;
        let result = parse_eval_verdict(text).unwrap();
        assert!(!result.approved);
    }

    #[test]
    fn test_parse_eval_verdict_prose_then_raw_json() {
        // Real-world case: agent emits a sentence of prose before the raw,
        // un-fenced JSON object. Previously failed with "No JSON found".
        let text = "All criteria met. Documentation matches implementation.\n\n{\"approved\": true, \"feedback\": \"ok\", \"diagnosis\": \"all good\"}";
        let result = parse_eval_verdict(text).unwrap();
        assert!(result.approved);
        assert_eq!(result.feedback, "ok");
    }

    #[test]
    fn test_parse_prompter_output_prose_then_raw_json() {
        let text = "Here is the prompt and rubric.\n\n{\"prompt\": \"Goal: Implement X\", \"rubric\": []}";
        let result = parse_prompter_output(text).unwrap();
        assert_eq!(result.prompt, "Goal: Implement X");
    }

    #[test]
    fn test_extract_goal_from_prompt() {
        let prompt = "Goal: Update the README\n\nContext: ...";
        assert_eq!(extract_goal(prompt), "Update the README");
    }

    #[test]
    fn test_extract_goal_fallback() {
        let prompt = "Some text without a goal section";
        assert_eq!(extract_goal(prompt), "See prompt for details");
    }

}
