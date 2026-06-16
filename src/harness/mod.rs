pub mod acp;
#[cfg(test)]
pub mod fake;

use crate::config::Config;
use crate::events::EventSink;
use crate::harness::acp::AcpHarness;
use crate::types::{Role, TurnOutcome};
use async_trait::async_trait;
use std::collections::HashMap;
use std::path::PathBuf;

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
            let harness = AcpHarness::new(
                hc.command,
                hc.args,
                self.target.clone(),
                self.config.r#loop.prompt_timeout_secs,
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
}
