pub mod acp;
pub mod claude;
#[cfg(test)]
pub mod fake;

use crate::config::{Config, HarnessConfig};
use crate::events::EventSink;
use crate::harness::acp::AcpHarness;
use crate::harness::claude::{is_native_claude, ClaudeHarness};
use crate::types::{Role, TurnOutcome};
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

    /// Get (lazily starting if needed) the session for the given role.
    pub async fn session_for(
        &mut self,
        role: Role,
    ) -> Result<&mut dyn HarnessSession, crate::error::OrbitError> {
        let hc = self.config.harness_for(role);
        let key = if hc.args.is_empty() {
            hc.command.clone()
        } else {
            format!("{} {}", hc.command, hc.args.join(" "))
        };

        if !self.sessions.contains_key(&key) {
            let harness = make_harness(&hc, &self.target, self.config.r#loop.prompt_timeout_secs);
            let sess = harness.start_session(self.events.clone()).await?;
            self.sessions.insert(key.clone(), sess);
        }

        Ok(self
            .sessions
            .get_mut(&key)
            .expect("session just inserted")
            .as_mut())
    }
}
