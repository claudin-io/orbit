use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Config {
    #[serde(default)]
    pub harness: HarnessConfig,
    #[serde(default)]
    pub r#loop: LoopConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessConfig {
    #[serde(default = "default_harness_command")]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            command: default_harness_command(),
            args: Vec::new(),
        }
    }
}

fn default_harness_command() -> String {
    "claude-code-acp".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct LoopConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_prompt_timeout_secs")]
    pub prompt_timeout_secs: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            prompt_timeout_secs: default_prompt_timeout_secs(),
        }
    }
}

fn default_max_attempts() -> u32 {
    5
}
fn default_prompt_timeout_secs() -> u64 {
    1200
}

pub fn load(path: Option<&str>, target: &Path) -> Config {
    let mut config = Config::default();

    if let Some(config_path) = path {
        let path = PathBuf::from(config_path);
        if path.exists()
            && let Ok(contents) = std::fs::read_to_string(&path)
                && let Ok(file_config) = toml::from_str::<Config>(&contents) {
                    merge(&mut config, file_config);
                }
    }

    let target_config = target.join("orbit.toml");
    if target_config.exists()
        && let Ok(contents) = std::fs::read_to_string(&target_config)
            && let Ok(file_config) = toml::from_str::<Config>(&contents) {
                merge(&mut config, file_config);
            }

    config
}

fn merge(base: &mut Config, overlay: Config) {
    if !overlay.harness.command.is_empty() {
        base.harness.command = overlay.harness.command;
    }
    if !overlay.harness.args.is_empty() {
        base.harness.args = overlay.harness.args;
    }
    if overlay.r#loop.max_attempts != default_max_attempts() {
        base.r#loop.max_attempts = overlay.r#loop.max_attempts;
    }
    if overlay.r#loop.prompt_timeout_secs != default_prompt_timeout_secs() {
        base.r#loop.prompt_timeout_secs = overlay.r#loop.prompt_timeout_secs;
    }
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub target: PathBuf,
    pub config: Config,
    pub spec_path: PathBuf,
}

#[derive(Serialize, Deserialize)]
struct AcpConfigFile {
    harness: AcpHarnessSection,
}

#[derive(Serialize, Deserialize)]
struct AcpHarnessSection {
    command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    args: Vec<String>,
}

pub fn save_acp_default(config_path: &Path, command_str: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = command_str.split_whitespace().collect();
    if parts.is_empty() {
        return Err(anyhow::anyhow!("Empty ACP command"));
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let acp_config = AcpConfigFile {
        harness: AcpHarnessSection {
            command: parts[0].to_string(),
            args: parts[1..].iter().map(|s| s.to_string()).collect(),
        },
    };

    let toml_str = toml::to_string(&acp_config)?;
    std::fs::write(config_path, toml_str)?;
    Ok(())
}

pub fn load_acp_default(config_path: &Path) -> anyhow::Result<String> {
    if !config_path.exists() {
        return Err(anyhow::anyhow!("ACP config file not found: {}", config_path.display()));
    }
    let contents = std::fs::read_to_string(config_path)?;
    let acp_config: AcpConfigFile = toml::from_str(&contents)?;
    let mut parts = vec![acp_config.harness.command];
    parts.extend(acp_config.harness.args);
    Ok(parts.join(" "))
}

pub fn load_acp_default_from_home() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = PathBuf::from(home).join(".orbit").join("config.toml");
    load_acp_default(&path).ok()
}
