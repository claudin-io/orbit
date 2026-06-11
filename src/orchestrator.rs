use crate::cli::{self, AcpAction, Command};
use crate::cli::Cli;
use crate::config;
use crate::error::OrbitError;
use crate::events::{EventSink, emit};
use crate::harness::Harness;
use crate::harness::acp::AcpHarness;
use crate::prompts::{EVALUATOR_TEMPLATE, PROMPTER_REVISION_TEMPLATE, PROMPTER_TEMPLATE, extract_fenced_json, render};
use crate::types::{EvalVerdict, OrbitEvent, PrompterOutput, Role, RunPhase};
use std::collections::HashMap;

pub async fn dispatch(cli: Cli, events: EventSink) -> Result<(), OrbitError> {
    match &cli.command {
        Command::Run { .. } => {
            let run_config = cli::resolve_config(&cli)?;
            run_simple_loop(run_config, events).await
        }
        Command::Git { action } => crate::git::dispatch(action, events).await,
        Command::Acp { action } => match action {
            AcpAction::SetDefault { command } => {
                let home = std::env::var("HOME").map_err(|_| OrbitError::Config("HOME not set".to_string()))?;
                let config_path = std::path::PathBuf::from(home).join(".orbit").join("config.toml");
                config::save_acp_default(&config_path, command)?;
                println!("Saved default ACP command: {}", command);
                Ok(())
            }
            AcpAction::Handshake => {
                let target = std::env::current_dir().unwrap_or_default();
                let config = config::load(None, &target);
                let harness = AcpHarness::new(
                    config.harness.command.clone(),
                    config.harness.args.clone(),
                    target,
                    config.r#loop.prompt_timeout_secs,
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

    let harness = AcpHarness::new(
        config.harness.command.clone(),
        config.harness.args.clone(),
        target.clone(),
        config.r#loop.prompt_timeout_secs,
    );

    emit!(events, OrbitEvent::PhaseChanged(RunPhase::Prompting));

    let prompter_output = run_prompter(&harness, &spec_content, &events).await?;
    let prompt_summary = extract_goal(&prompter_output.prompt);

    emit!(
        events,
        OrbitEvent::PromptCreated {
            prompt_summary: prompt_summary.clone(),
            rubric: prompter_output.rubric.clone(),
        }
    );

    let rubric_text = rubrics_to_display(&prompter_output.rubric);
    let mut prompt = prompter_output.prompt;
    let max_attempts = config.r#loop.max_attempts;

    for attempt in 1..=max_attempts {
        emit!(events, OrbitEvent::PhaseChanged(RunPhase::Coding));
        emit!(
            events,
            OrbitEvent::AttemptStarted {
                attempt,
                max_attempts,
            }
        );

        let coder_outcome = harness.run_turn(Role::Coder, prompt.clone(), events.clone()).await?;
        let coder_text = coder_outcome.full_text;

        let coder_summary = summarize_coder_output(&coder_text);
        emit!(events, OrbitEvent::CoderOutput {
            summary: coder_summary.clone(),
        });

        emit!(events, OrbitEvent::PhaseChanged(RunPhase::Evaluating));

        let eval_outcome =
            run_evaluator(&harness, &spec_content, &rubric_text, &coder_text, &events).await?;
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
            prompt = run_prompter_revision(
                &harness,
                &spec_content,
                &coder_text,
                &eval_outcome.feedback,
                &eval_outcome.diagnosis,
                &events,
            )
            .await?;
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

async fn run_prompter(
    harness: &dyn Harness,
    spec: &str,
    events: &EventSink,
) -> Result<PrompterOutput, OrbitError> {
    let mut ctx = HashMap::new();
    ctx.insert("spec", spec);
    let prompt = render(PROMPTER_TEMPLATE, &ctx);
    let outcome = harness.run_turn(Role::Prompter, prompt, events.clone()).await?;
    parse_prompter_output(&outcome.full_text)
}

async fn run_prompter_revision(
    harness: &dyn Harness,
    spec: &str,
    coder_output: &str,
    eval_feedback: &str,
    eval_diagnosis: &str,
    events: &EventSink,
) -> Result<String, OrbitError> {
    let mut ctx = HashMap::new();
    ctx.insert("spec", spec);
    ctx.insert("coder_output", coder_output);
    ctx.insert("eval_feedback", eval_feedback);
    ctx.insert("eval_diagnosis", eval_diagnosis);
    let prompt = render(PROMPTER_REVISION_TEMPLATE, &ctx);
    let outcome = harness.run_turn(Role::Prompter, prompt, events.clone()).await?;
    let parsed = parse_prompter_output(&outcome.full_text)?;
    Ok(parsed.prompt)
}

async fn run_evaluator(
    harness: &dyn Harness,
    spec: &str,
    rubric: &str,
    coder_output: &str,
    events: &EventSink,
) -> Result<EvalVerdict, OrbitError> {
    let mut ctx = HashMap::new();
    ctx.insert("spec", spec);
    ctx.insert("rubric", rubric);
    ctx.insert("coder_output", coder_output);
    let prompt = render(EVALUATOR_TEMPLATE, &ctx);
    let outcome = harness.run_turn(Role::Evaluator, prompt, events.clone()).await?;
    parse_eval_verdict(&outcome.full_text)
}

fn sanitize_llm_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;

    for ch in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(ch);
            } else if ch == '\\' {
                escaped = true;
                out.push(ch);
            } else if ch == '"' {
                in_string = false;
                out.push(ch);
            } else if ch == '\n' || ch == '\r' {
                out.push_str("\\n");
            } else if ch == '\t' {
                out.push_str("\\t");
            } else if ch.is_control() {
            } else {
                out.push(ch);
            }
        } else {
            if ch == '"' {
                in_string = true;
            }
            out.push(ch);
        }
    }
    out
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
    let json_str =
        extract_fenced_json(&raw).or_else(|| {
            let t = raw.trim();
            if t.starts_with('{') {
                Some(t)
            } else {
                None
            }
        });
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
    let json_str =
        extract_fenced_json(&raw).or_else(|| {
            let t = raw.trim();
            if t.starts_with('{') {
                Some(t)
            } else {
                None
            }
        });
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
    use crate::events;
    use crate::harness::fake::FakeHarness;

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
    fn test_extract_goal_from_prompt() {
        let prompt = "Goal: Update the README\n\nContext: ...";
        assert_eq!(extract_goal(prompt), "Update the README");
    }

    #[test]
    fn test_extract_goal_fallback() {
        let prompt = "Some text without a goal section";
        assert_eq!(extract_goal(prompt), "See prompt for details");
    }

    #[tokio::test]
    async fn test_run_prompter_with_fake_harness() {
        let harness = FakeHarness;
        let (tx, _rx) = events::channel();
        let result = run_prompter(&harness, "do something", &tx).await;
        assert!(result.is_err(), "fake harness returns empty text, should fail to parse");
    }

    #[tokio::test]
    async fn test_run_evaluator_with_fake_harness() {
        let harness = FakeHarness;
        let (tx, _rx) = events::channel();
        let result = run_evaluator(&harness, "spec", "rubric", "output", &tx).await;
        assert!(result.is_err(), "fake harness returns empty text, should fail to parse");
    }
}
