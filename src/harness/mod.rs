pub mod acp;
#[cfg(test)]
pub mod fake;

use crate::events::EventSink;
use crate::types::{Role, TurnOutcome};
use async_trait::async_trait;

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run_turn(
        &self,
        role: Role,
        prompt: String,
        events: EventSink,
    ) -> Result<TurnOutcome, crate::error::OrbitError>;
}

#[async_trait]
pub trait HarnessSession: Send {
    async fn run_turn(&mut self, role: Role, prompt: String) -> Result<TurnOutcome, crate::error::OrbitError>;
}
