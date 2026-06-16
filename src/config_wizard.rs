use crate::config::{self, HarnessConfig, StepsConfig};
use crate::error::OrbitError;
use crate::events::EventSink;
use crate::harness::make_harness;
use crate::types::{OrbitEvent, Role};
use std::path::{Path, PathBuf};
use tokio::sync::oneshot;

/// Result of prompting for one ACP command.
enum Outcome {
    /// A validated (or force-saved) command.
    Cmd(HarnessConfig),
    /// User left it blank — fall back to the base/harness command.
    UseBase,
    /// User cancelled the whole wizard.
    Cancel,
}

/// Send a text-input prompt to the renderer and await the typed line.
async fn ask(events: &EventSink, message: impl Into<String>) -> Result<String, OrbitError> {
    let (tx, rx) = oneshot::channel();
    events
        .send(OrbitEvent::PromptInput {
            message: message.into(),
            tx,
        })
        .map_err(|_| OrbitError::Other("renderer channel closed".to_string()))?;
    rx.await
        .map_err(|_| OrbitError::Other("input cancelled".to_string()))
}

/// Print an informational line via the renderer.
fn notice(events: &EventSink, message: impl Into<String>) {
    let _ = events.send(OrbitEvent::Notice {
        message: message.into(),
    });
}

/// Run a one-shot ACP handshake against `hc` to validate it can connect.
async fn handshake(
    hc: &HarnessConfig,
    cwd: &Path,
    timeout: u64,
    events: &EventSink,
) -> Result<(), OrbitError> {
    let harness = make_harness(hc, cwd, timeout);
    harness
        .run_turn(
            Role::Coder,
            "Hello, respond with exactly: OK".to_string(),
            events.clone(),
        )
        .await
        .map(|_| ())
}

/// Prompt for one ACP command, run a handshake, and handle failures.
async fn prompt_and_validate(
    events: &EventSink,
    label: &str,
    cwd: &Path,
    timeout: u64,
    allow_blank: bool,
) -> Result<Outcome, OrbitError> {
    loop {
        let hint = if allow_blank {
            " (Enter = use base)"
        } else {
            ""
        };
        let line = ask(events, format!("ACP command for {label}{hint}:")).await?;

        if line.is_empty() {
            if allow_blank {
                return Ok(Outcome::UseBase);
            }
            notice(events, "Command required — please type something.");
            continue;
        }

        let hc = match HarnessConfig::parse(&line) {
            Some(hc) => hc,
            None => {
                notice(events, "Invalid command.");
                continue;
            }
        };

        notice(events, format!("Testing handshake: {}", hc.to_command_line()));
        match handshake(&hc, cwd, timeout, events).await {
            Ok(()) => {
                notice(events, format!("✓ {label}: ACP OK"));
                return Ok(Outcome::Cmd(hc));
            }
            Err(e) => {
                let choice = ask(
                    events,
                    format!(
                        "{label}: ACP FAILED: {e}  [r] rewrite  [s] save anyway  [c] cancel:"
                    ),
                )
                .await?
                .to_lowercase();
                match choice.as_str() {
                    "s" | "save" => return Ok(Outcome::Cmd(hc)),
                    "c" | "cancel" => return Ok(Outcome::Cancel),
                    _ => continue, // "r" / anything else → rewrite
                }
            }
        }
    }
}

pub async fn run_wizard(events: EventSink) -> Result<(), OrbitError> {
    let cwd = std::env::current_dir().map_err(OrbitError::Io)?;
    // Inherit the prompt timeout from any existing config, else the default.
    let existing = config::load(None, &cwd);
    let timeout = existing.r#loop.prompt_timeout_secs;

    notice(&events, "orbit configuration (.orbit)");

    // 1. Where to save.
    let save_path: PathBuf = loop {
        let loc = ask(
            &events,
            "Where to save? [1] global (~/.orbit/config.orbit)  [2] project (<cwd>/.orbit/config.orbit):",
        )
        .await?;
        match loc.as_str() {
            "1" => match config::home_config_path() {
                Some(p) => break p,
                None => {
                    notice(&events, "HOME not set — choose [2] project.");
                    continue;
                }
            },
            "2" => break config::project_config_path(&cwd),
            _ => notice(&events, "Invalid option. Type 1 or 2."),
        }
    };

    // 2. Mode: same for all, or per-step.
    let segmented = loop {
        let mode = ask(
            &events,
            "ACP: [1] same for all  [2] per step (plan/code/eval):",
        )
        .await?;
        match mode.as_str() {
            "1" => break false,
            "2" => break true,
            _ => notice(&events, "Invalid option. Type 1 or 2."),
        }
    };

    // 3. Collect commands.
    let mut steps = StepsConfig::default();
    let harness: HarnessConfig;

    if !segmented {
        match prompt_and_validate(&events, "harness", &cwd, timeout, false).await? {
            Outcome::Cmd(hc) => harness = hc,
            Outcome::Cancel => return cancel(&events),
            Outcome::UseBase => unreachable!("blank not allowed for base"),
        }
    } else {
        match prompt_and_validate(&events, "base (harness)", &cwd, timeout, false).await? {
            Outcome::Cmd(hc) => harness = hc,
            Outcome::Cancel => return cancel(&events),
            Outcome::UseBase => unreachable!("blank not allowed for base"),
        }

        for (label, slot) in [
            ("plan", &mut steps.plan),
            ("code", &mut steps.code),
            ("eval", &mut steps.evaluation),
        ] {
            match prompt_and_validate(&events, label, &cwd, timeout, true).await? {
                Outcome::Cmd(hc) => *slot = Some(hc),
                Outcome::UseBase => {} // leave None → falls back to harness
                Outcome::Cancel => return cancel(&events),
            }
        }
    }

    // 4. Write.
    config::write_orbit_config(&save_path, &harness, &steps)
        .map_err(|e| OrbitError::Config(format!("failed to write config: {e}")))?;
    notice(&events, format!("Saved to {}", save_path.display()));

    Ok(())
}

fn cancel(events: &EventSink) -> Result<(), OrbitError> {
    notice(events, "Cancelled. Nothing saved.");
    Ok(())
}
