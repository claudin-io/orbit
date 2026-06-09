use super::{Harness, HarnessSession};
use crate::error::OrbitError;
use crate::events::EventSink;
use crate::types::{Role, TurnOutcome};
use async_trait::async_trait;

pub struct FakeHarness;

#[async_trait]
impl Harness for FakeHarness {
    async fn run_turn(&self, _role: Role, _prompt: String, _events: EventSink) -> Result<TurnOutcome, OrbitError> {
        Ok(TurnOutcome {
            stop_reason: "end_turn".to_string(),
            full_text: String::new(),
        })
    }
}

pub struct FakeHarnessSession;

#[async_trait]
impl HarnessSession for FakeHarnessSession {
    async fn run_turn(&mut self, _role: Role, _prompt: String) -> Result<TurnOutcome, OrbitError> {
        Ok(TurnOutcome {
            stop_reason: "end_turn".to_string(),
            full_text: String::new(),
        })
    }
}
