use crate::cli::GitCliAction;
use crate::config;
use crate::error::OrbitError;
use crate::events::{EventSink, emit};
use crate::harness::{Harness, HarnessSession};
use crate::harness::acp::AcpHarness;
use crate::prompts::{extract_fenced_json, render, sanitize_llm_json};
use crate::render;
use crate::types::{EvalVerdict, OrbitEvent, PrompterOutput, Role, RunPhase};
use std::collections::HashMap;
use std::io::Write;

const PROMPT_GIT_PLANNER: &str = r#"You are a Git Commit Planner. Analyze git state and produce a commit plan.

Fast rules:
- Run git status + git diff (2 bash calls max)
- No task tool
- Do not explore beyond git output

Read git status and diff, then decide: one commit or multi? What type/scope/message?

{{#if stage_all}}
The user wants to commit ALL changes (--all flag). Your plan must include `git add -A` before committing.
{{/if}}

Output ONLY this JSON:
{"prompt": "Goal: ...\n\nContext: ...\n\nPlan:\n1. git add <files>\n   git commit -m 'type(scope): subject'\n\nRequirements:\n1. ...\n\nConstraints:\n- ...", "rubric": [{"criterion": "...", "description": "...", "weight": 3}], "analysis": "Brief reasoning"}
"#;

const PROMPT_GIT_EVALUATOR: &str = r#"You are a Git Commit Plan Evaluator. Check the proposed commit plan.

---COMMIT PLAN---
{{plan}}
---END COMMIT PLAN---

---RUBRIC---
{{rubric}}
---END RUBRIC---

Fast rules: no task tool, no exploration — just read the plan and rubric above.

Is the plan correct? Cohesive files? Good message? Right type/scope?

Output ONLY this JSON:
{"approved": true, "feedback": "ok", "diagnosis": "All criteria met", "results": [{"criterion": "...", "pass": true, "evidence": "..."}]}
"#;

const PROMPT_GIT_COMMITTER: &str = r#"You are a Git Committer. Execute the approved commit plan.

---COMMIT PLAN---
{{plan}}
---END COMMIT PLAN---

Execute:
1. git add <files>
2. git commit -m 'message'

Verify with git log --oneline -1. Report what was done.

No task tool.
"#;

const PROMPT_GIT_PLANNER_REVISION: &str = r#"You are a Git Commit Planner. Previous plan was rejected. Revise.

---EVALUATION---
{{eval_feedback}}
{{eval_diagnosis}}
---END EVALUATION---

---PREVIOUS PLAN---
{{previous_plan}}
---END PREVIOUS PLAN---

Fast rules: no task tool, no re-exploration. Fix what the evaluation says.

Output ONLY this JSON:
{"prompt": "Goal: ...\n\nContext: ...\n\nPlan:\n1. git add <files>\n   git commit -m 'type(scope): subject'\n\nRequirements:\n1. ...\n\nConstraints:\n- ...", "rubric": [{"criterion": "...", "description": "...", "weight": 3}], "analysis": "Brief explanation"}
"#;

pub async fn dispatch(action: &GitCliAction, events: EventSink) -> Result<(), OrbitError> {
    match action {
        GitCliAction::Commit { all, yes } => run_git_commit_loop(*all, *yes, events).await,
        GitCliAction::Worktree { action } => {
            emit!(events, OrbitEvent::PhaseChanged(RunPhase::GitWorktree));
            let result = crate::git_worktree::dispatch(action);
            emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
            emit!(events, OrbitEvent::RunFinished { exit_code: if result.is_ok() { 0 } else { 1 } });
            result
        }
    }
}

fn confirm(message: &str, default: bool) -> bool {
    let prompt = if default { "[Y/n]" } else { "[y/N]" };
    eprint!(
        "  {} {} {} ",
        render::c("?", render::YLW),
        render::c(message, render::BLD),
        render::c(prompt, render::DIM)
    );
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();
    match input.as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

async fn run_git_commit_loop(all: bool, yes: bool, events: EventSink) -> Result<(), OrbitError> {
    let target = std::env::current_dir().map_err(OrbitError::Io)?;
    let mut config = config::load(None, &target);
    if config.harness.command == "claude-code-acp"
        && let Some(saved) = config::load_acp_default_from_home()
    {
        let parts: Vec<&str> = saved.split_whitespace().collect();
        if !parts.is_empty() {
            config.harness.command = parts[0].to_string();
            config.harness.args = parts[1..].iter().map(|s| s.to_string()).collect();
        }
    }

    emit!(
        events,
        OrbitEvent::RunStarted {
            spec_path: "git commit".to_string(),
            target: target.to_string_lossy().to_string(),
        }
    );

    let harness = AcpHarness::new(
        config.harness.command.clone(),
        config.harness.args.clone(),
        target,
        config.r#loop.prompt_timeout_secs,
    );

    let mut session = harness.start_session(events.clone()).await?;
    let max_attempts = config.r#loop.max_attempts;
    let mut plan_text = String::new();
    let mut plan_feedback = String::new();
    let mut plan_diagnosis = String::new();

    for attempt in 1..=max_attempts {
        emit!(events, OrbitEvent::PhaseChanged(RunPhase::GitPlanning));
        emit!(
            events,
            OrbitEvent::AttemptStarted {
                attempt,
                max_attempts,
            }
        );

        let planner_prompt = if attempt == 1 {
            let mut ctx = HashMap::new();
            ctx.insert("stage_all", if all { "true" } else { "" });
            render(PROMPT_GIT_PLANNER, &ctx)
        } else {
            let mut ctx: HashMap<&str, &str> = HashMap::new();
            ctx.insert("eval_feedback", &plan_feedback);
            ctx.insert("eval_diagnosis", &plan_diagnosis);
            ctx.insert("previous_plan", &plan_text);
            render(PROMPT_GIT_PLANNER_REVISION, &ctx)
        };

        let planner_outcome = session.run_turn(Role::Prompter, planner_prompt).await?;
        let output = parse_git_planner_output(&planner_outcome.full_text)?;

        plan_text = output.prompt;
        let plan_rubric = output.rubric;
        let analysis = output.analysis;
        let rubric_text = rubrics_to_display(&plan_rubric);

        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("───", render::DIM),
            render::c("PLAN", render::BLD)
        );
        for line in plan_text.lines() {
            let _ = writeln!(std::io::stdout(), "  {}", line);
        }
        if !analysis.is_empty() {
            let _ = writeln!(
                std::io::stdout(),
                "  {} {}",
                render::c("analysis:", render::DIM),
                render::c(&analysis, render::YLW)
            );
        }

        emit!(
            events,
            OrbitEvent::PromptCreated {
                prompt_summary: format!("Git commit: {}", plan_text.lines().next().unwrap_or("See plan").trim()),
                rubric: plan_rubric.clone(),
            }
        );

        emit!(events, OrbitEvent::PhaseChanged(RunPhase::GitReviewing));

        let eval_outcome = run_plan_evaluator(&mut session, &plan_text, &rubric_text).await?;

        emit!(
            events,
            OrbitEvent::EvalVerdict {
                approved: eval_outcome.approved,
                feedback: eval_outcome.feedback.clone(),
                diagnosis: eval_outcome.diagnosis.clone(),
                results: eval_outcome.results.clone(),
            }
        );

        let _ = std::io::stdout().flush();

        if !eval_outcome.approved {
            plan_feedback = eval_outcome.feedback;
            plan_diagnosis = eval_outcome.diagnosis;
            if attempt < max_attempts {
                let _ = writeln!(
                    std::io::stdout(),
                    "  {}",
                    render::c("Plan rejected. Revising...", render::YLW)
                );
            }
            continue;
        }

        if !yes {
            let confirmed = confirm("Proceed with this commit?", true);
            if !confirmed {
                let _ = writeln!(
                    std::io::stdout(),
                    "  {} {}",
                    render::c("●", render::RED),
                    render::c("Aborted by user.", render::BLD)
                );
                emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
                emit!(events, OrbitEvent::RunFinished { exit_code: 0 });
                return Ok(());
            }
        }

        emit!(events, OrbitEvent::PhaseChanged(RunPhase::GitCommitting));

        let mut ctx: HashMap<&str, &str> = HashMap::new();
        ctx.insert("plan", &plan_text);
        let committer_prompt = render(PROMPT_GIT_COMMITTER, &ctx);
        let outcome = session.run_turn(Role::Coder, committer_prompt).await?;

        emit!(
            events,
            OrbitEvent::CoderOutput {
                summary: summarize_committer_output(&outcome.full_text),
            }
        );

        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("●", render::GRN),
            render::c(outcome.full_text.trim(), render::DIM)
        );

        emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
        emit!(events, OrbitEvent::RunFinished { exit_code: 0 });
        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("✓", render::GRN),
            render::c("Commit completed successfully.", render::BLD)
        );
        return Ok(());
    }

    emit!(events, OrbitEvent::PhaseChanged(RunPhase::Done));
    let reason = format!(
        "Commit plan did not pass evaluation after {} attempts",
        max_attempts
    );
    emit!(
        events,
        OrbitEvent::RunFailed {
            reason: reason.clone(),
        }
    );
    Err(OrbitError::Exhausted(reason))
}

async fn run_plan_evaluator(
    session: &mut dyn HarnessSession,
    plan: &str,
    rubric: &str,
) -> Result<EvalVerdict, OrbitError> {
    let mut ctx = HashMap::new();
    ctx.insert("plan", plan);
    ctx.insert("rubric", rubric);
    let prompt = render(PROMPT_GIT_EVALUATOR, &ctx);
    let outcome = session.run_turn(Role::Evaluator, prompt).await?;
    parse_eval_verdict(&outcome.full_text)
}

fn parse_git_planner_output(text: &str) -> Result<PrompterOutput, OrbitError> {
    let raw = text.trim();
    let raw = sanitize_llm_json(raw);
    let json_str = extract_fenced_json(&raw).or_else(|| {
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
            dump_debug(text, "git-planner");
            return Err(OrbitError::Parse("No JSON found in git planner output".to_string()));
        }
    };
    let output: PrompterOutput = serde_json::from_str(json_str).map_err(|e| {
        dump_debug(text, "git-planner");
        OrbitError::Parse(format!("Failed to parse git planner JSON: {}", e))
    })?;
    if output.prompt.is_empty() {
        return Err(OrbitError::Parse("Git planner returned empty plan".to_string()));
    }
    Ok(output)
}

fn parse_eval_verdict(text: &str) -> Result<EvalVerdict, OrbitError> {
    let raw = text.trim();
    let raw = sanitize_llm_json(raw);
    let json_str = extract_fenced_json(&raw).or_else(|| {
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
            dump_debug(text, "git-evaluator");
            return Err(OrbitError::Parse("No JSON found in git evaluator output".to_string()));
        }
    };
    let verdict: EvalVerdict = serde_json::from_str(json_str).map_err(|e| {
        dump_debug(text, "git-evaluator");
        OrbitError::Parse(format!("Failed to parse git evaluator JSON: {}", e))
    })?;
    Ok(verdict)
}

fn rubrics_to_display(rubric: &[crate::types::RubricItem]) -> String {
    rubric
        .iter()
        .map(|r| format!("- {} (weight {}): {}", r.criterion, r.weight, r.description))
        .collect::<Vec<_>>()
        .join("\n")
}

fn summarize_committer_output(text: &str) -> String {
    let non_empty: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let total = non_empty.len();
    let commit_lines: Vec<&&str> = non_empty
        .iter()
        .filter(|l| l.contains("commit") && (l.contains("hash") || l.contains("committed")))
        .collect();
    if !commit_lines.is_empty() {
        format!("{} commits created", commit_lines.len())
    } else {
        format!("{} lines of output", total)
    }
}

fn dump_debug(text: &str, label: &str) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_git_planner_output_valid() {
        let json = "```json\n{\"prompt\": \"Goal: Commit login feature\", \"rubric\": [{\"criterion\": \"C1\", \"description\": \"d\", \"weight\": 1}]}\n```";
        let result = parse_git_planner_output(json).unwrap();
        assert_eq!(result.prompt, "Goal: Commit login feature");
        assert_eq!(result.rubric.len(), 1);
    }

    #[test]
    fn test_parse_git_planner_output_no_fence() {
        let text = "{\"prompt\": \"Goal: Fix bug\", \"rubric\": []}";
        let result = parse_git_planner_output(text).unwrap();
        assert_eq!(result.prompt, "Goal: Fix bug");
    }

    #[test]
    fn test_parse_git_planner_output_empty() {
        let text = r#"{"prompt": "", "rubric": []}"#;
        assert!(parse_git_planner_output(text).is_err());
    }

    #[test]
    fn test_parse_eval_verdict_approved() {
        let text = r#"{"approved": true, "feedback": "ok", "diagnosis": "good"}"#;
        let result = parse_eval_verdict(text).unwrap();
        assert!(result.approved);
    }

    #[test]
    fn test_parse_eval_verdict_rejected() {
        let text = r#"{"approved": false, "feedback": "bad grouping", "diagnosis": "files unrelated"}"#;
        let result = parse_eval_verdict(text).unwrap();
        assert!(!result.approved);
        assert_eq!(result.feedback, "bad grouping");
    }

    #[test]
    fn test_rubrics_to_display() {
        use crate::types::RubricItem;
        let rubric = vec![RubricItem {
            criterion: "C1".to_string(),
            description: "d1".to_string(),
            weight: 3,
        }];
        let result = rubrics_to_display(&rubric);
        assert!(result.contains("C1"));
        assert!(result.contains("weight 3"));
    }
}
