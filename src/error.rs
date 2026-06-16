use thiserror::Error;

#[derive(Error, Debug)]
pub enum OrbitError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("ACP protocol error: {0}")]
    Acp(String),

    #[error("Parse error: {0}")]
    Parse(String),

    #[error("All attempts exhausted: {0}")]
    Exhausted(String),

    #[error("Session limit reached: {0}")]
    SessionLimit(String),

    #[error("{0}")]
    Other(String),
}

impl OrbitError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Config(_) | Self::Parse(_) => 2,
            Self::Io(_) | Self::Acp(_) => 2,
            Self::Exhausted(_) => 1,
            Self::SessionLimit(_) => 2,
            Self::Other(_) => 2,
        }
    }
}

impl From<anyhow::Error> for OrbitError {
    fn from(e: anyhow::Error) -> Self {
        Self::Other(e.to_string())
    }
}
