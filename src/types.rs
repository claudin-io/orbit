use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalVerdict {
    pub approved: bool,
    #[serde(default)]
    pub feedback: String,
    #[serde(default)]
    pub diagnosis: String,
    #[serde(default)]
    pub results: Vec<EvalCriterionResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalCriterionResult {
    pub criterion: String,
    pub pass: bool,
    #[serde(default)]
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricItem {
    #[serde(default)]
    pub criterion: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_weight")]
    pub weight: u8,
}

fn default_weight() -> u8 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrompterOutput {
    pub prompt: String,
    #[serde(default)]
    pub rubric: Vec<RubricItem>,
    #[serde(default)]
    pub analysis: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RunPhase {
    Prompting,
    Coding,
    Evaluating,
    Done,
    GitPlanning,
    GitReviewing,
    GitCommitting,
    GitWorktree,
}

#[derive(Debug)]
pub enum OrbitEvent {
    PhaseChanged(RunPhase),
    RunStarted { spec_path: String, target: String },
    AgentChunk(String),
    TaskMessage { task_id: String, message: String },
    ToolCall { name: String, params: Option<String>, raw_input: Option<serde_json::Value> },
    PromptCreated { prompt_summary: String, rubric: Vec<RubricItem> },
    AttemptStarted { attempt: u32, max_attempts: u32 },
    CoderOutput { summary: String },
    EvalVerdict { approved: bool, feedback: String, diagnosis: String, results: Vec<EvalCriterionResult> },
    RunFinished { exit_code: i32 },
    RunFailed { reason: String },
    ConfirmRequest { message: String, default: bool, tx: tokio::sync::oneshot::Sender<bool> },
    PromptInput { message: String, tx: tokio::sync::oneshot::Sender<String> },
    Notice { message: String },
}

impl Clone for OrbitEvent {
    fn clone(&self) -> Self {
        match self {
            Self::ConfirmRequest { .. } => panic!("ConfirmRequest cannot be cloned"),
            Self::PromptInput { .. } => panic!("PromptInput cannot be cloned"),
            Self::Notice { message } => Self::Notice { message: message.clone() },
            Self::PhaseChanged(v) => Self::PhaseChanged(v.clone()),
            Self::RunStarted { spec_path, target } => Self::RunStarted { spec_path: spec_path.clone(), target: target.clone() },
            Self::AgentChunk(v) => Self::AgentChunk(v.clone()),
            Self::TaskMessage { task_id, message } => Self::TaskMessage { task_id: task_id.clone(), message: message.clone() },
            Self::ToolCall { name, params, raw_input } => Self::ToolCall { name: name.clone(), params: params.clone(), raw_input: raw_input.clone() },
            Self::PromptCreated { prompt_summary, rubric } => Self::PromptCreated { prompt_summary: prompt_summary.clone(), rubric: rubric.clone() },
            Self::AttemptStarted { attempt, max_attempts } => Self::AttemptStarted { attempt: *attempt, max_attempts: *max_attempts },
            Self::CoderOutput { summary } => Self::CoderOutput { summary: summary.clone() },
            Self::EvalVerdict { approved, feedback, diagnosis, results } => Self::EvalVerdict { approved: *approved, feedback: feedback.clone(), diagnosis: diagnosis.clone(), results: results.clone() },
            Self::RunFinished { exit_code } => Self::RunFinished { exit_code: *exit_code },
            Self::RunFailed { reason } => Self::RunFailed { reason: reason.clone() },
        }
    }
}

#[derive(Debug, Clone)]
pub struct TurnOutcome {
    pub stop_reason: String,
    pub full_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    Prompter,
    Coder,
    Evaluator,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prompter => "prompter",
            Self::Coder => "coder",
            Self::Evaluator => "evaluator",
        }
    }
}


