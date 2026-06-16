pub mod acp;
pub mod claude;
#[cfg(test)]
pub mod fake;

use crate::config::{Config, HarnessConfig};
use crate::events::EventSink;
use crate::harness::acp::AcpHarness;
use crate::harness::claude::{is_native_claude, ClaudeHarness};
use crate::types::{OrbitEvent, Role, TurnOutcome};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Build the right harness for `hc`: the native [`ClaudeHarness`] when the
/// command is the `claude` CLI (which doesn't speak ACP), otherwise the generic
/// [`AcpHarness`].
pub fn make_harness(
    hc: &HarnessConfig,
    target: &Path,
    prompt_timeout_secs: u64,
    acp_retry_max_attempts: u32,
    acp_retry_base_delay_ms: u64,
) -> Box<dyn Harness> {
    if is_native_claude(&hc.command) {
        Box::new(ClaudeHarness::new(
            hc.command.clone(),
            hc.args.clone(),
            target.to_path_buf(),
            prompt_timeout_secs,
        ))
    } else {
        Box::new(AcpHarness::new(
            hc.command.clone(),
            hc.args.clone(),
            target.to_path_buf(),
            prompt_timeout_secs,
            acp_retry_max_attempts,
            acp_retry_base_delay_ms,
        ))
    }
}

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run_turn(
        &self,
        role: Role,
        prompt: String,
        events: EventSink,
    ) -> Result<TurnOutcome, crate::error::OrbitError>;

    async fn start_session(&self, events: EventSink) -> Result<Box<dyn HarnessSession>, crate::error::OrbitError>;
}

#[async_trait]
pub trait HarnessSession: Send {
    async fn run_turn(&mut self, role: Role, prompt: String) -> Result<TurnOutcome, crate::error::OrbitError>;
}

#[async_trait]
impl<T: HarnessSession + Send + ?Sized> HarnessSession for Box<T> {
    async fn run_turn(&mut self, role: Role, prompt: String) -> Result<TurnOutcome, crate::error::OrbitError> {
        self.as_mut().run_turn(role, prompt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HarnessConfig;
    use crate::events;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_fallback_for_overrides_role_config() {
        let mut cfg = crate::config::Config::default();
        cfg.harness = HarnessConfig {
            command: "primary".to_string(),
            args: vec![],
        };
        cfg.fallback = Some(HarnessConfig {
            command: "fallback-cmd".to_string(),
            args: vec!["--acp".to_string()],
        });

        let (tx, _rx) = events::channel();
        let mut router = SessionRouter::new(cfg, PathBuf::from("/tmp"), tx);

        // Before fallback, coder uses the global harness
        let key_before = router.key_for_role(Role::Coder);
        assert_eq!(key_before, "primary", "coder should start with global harness");

        // Apply fallback
        router.fallback_for(Role::Coder).unwrap();

        // After fallback, coder should use the fallback command
        let key_after = router.key_for_role(Role::Coder);
        assert_eq!(key_after, "fallback-cmd --acp", "coder should now use fallback");
    }

    #[tokio::test]
    async fn test_fallback_for_uses_global_harness_when_no_fallback_configured() {
        let mut cfg = crate::config::Config::default();
        cfg.harness = HarnessConfig {
            command: "global-harness".to_string(),
            args: vec![],
        };
        // No explicit fallback — should use self.config.harness as fallback
        cfg.fallback = None;

        let (tx, _rx) = events::channel();
        let mut router = SessionRouter::new(cfg, PathBuf::from("/tmp"), tx);

        // Override coder with per-step config
        router.config.steps.code = Some(HarnessConfig {
            command: "step-specific".to_string(),
            args: vec![],
        });
        assert_eq!(router.key_for_role(Role::Coder), "step-specific");

        // Fallback should fall back to global harness
        router.fallback_for(Role::Coder).unwrap();
        assert_eq!(router.key_for_role(Role::Coder), "global-harness");
    }
}

/// Routes each step/role to its configured ACP session, lazily starting one
/// session per distinct resolved command. Roles that resolve to the same
/// command share a session (so the default config keeps a single session).
pub struct SessionRouter {
    config: Config,
    target: PathBuf,
    events: EventSink,
    sessions: HashMap<String, Box<dyn HarnessSession>>,
}

impl SessionRouter {
    pub fn new(config: Config, target: PathBuf, events: EventSink) -> Self {
        Self {
            config,
            target,
            events,
            sessions: HashMap::new(),
        }
    }

    fn key_for_role(&self, role: Role) -> String {
        let hc = self.config.harness_for(role);
        if hc.args.is_empty() {
            hc.command.clone()
        } else {
            format!("{} {}", hc.command, hc.args.join(" "))
        }
    }

    /// Get (lazily starting if needed) the session for the given role.
    pub async fn session_for(
        &mut self,
        role: Role,
    ) -> Result<&mut dyn HarnessSession, crate::error::OrbitError> {
        let hc = self.config.harness_for(role);
        let key = self.key_for_role(role);

        if !self.sessions.contains_key(&key) {
            // Show which harness is about to start. The harness itself may
            // emit ModelInfo with a model name later (e.g. ACP config_options).
            let _ = self.events.send(OrbitEvent::ModelInfo {
                tool: hc.to_command_line(),
                model: None,
            });

            let harness = make_harness(
                &hc,
                &self.target,
                self.config.r#loop.prompt_timeout_secs,
                self.config.r#loop.acp_retry_max_attempts,
                self.config.r#loop.acp_retry_base_delay_ms,
            );
            let sess = harness.start_session(self.events.clone()).await?;
            self.sessions.insert(key.clone(), sess);
        }

        Ok(self
            .sessions
            .get_mut(&key)
            .expect("session just inserted")
            .as_mut())
    }

    /// Switch the given role to the fallback harness, invalidating the current
    /// session so the next call to [`session_for`](Self::session_for) creates a
    /// new session with the fallback command.
    ///
    /// Returns `Err` when no fallback is configured.
    pub fn fallback_for(&mut self, role: Role) -> Result<(), crate::error::OrbitError> {
        let fallback = self
            .config
            .fallback
            .clone()
            .or_else(|| Some(self.config.harness.clone()))
            .ok_or_else(|| {
                crate::error::OrbitError::Config(
                    "no fallback harness configured".to_string(),
                )
            })?;

        let old_key = self.key_for_role(role);
        self.sessions.remove(&old_key);

        match role {
            Role::Prompter => self.config.steps.plan = Some(fallback),
            Role::Coder => self.config.steps.code = Some(fallback),
            Role::Evaluator => self.config.steps.evaluation = Some(fallback),
        }

        Ok(())
    }
}
