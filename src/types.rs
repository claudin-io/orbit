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
}

#[derive(Debug, Clone)]
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


