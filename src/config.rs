use crate::types::Role;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub harness: HarnessConfig,
    pub steps: StepsConfig,
    pub r#loop: LoopConfig,
}

#[derive(Debug, Clone)]
pub struct HarnessConfig {
    pub command: String,
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

/// Per-step ACP overrides. A step with `None` falls back to [`Config::harness`].
#[derive(Debug, Clone, Default)]
pub struct StepsConfig {
    pub plan: Option<HarnessConfig>,
    pub code: Option<HarnessConfig>,
    pub evaluation: Option<HarnessConfig>,
}

#[derive(Debug, Clone)]
pub struct LoopConfig {
    pub max_attempts: u32,
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

impl Config {
    /// Resolve the ACP harness for a given step/role, falling back to the
    /// global `harness` when no per-step override is configured.
    pub fn harness_for(&self, role: Role) -> HarnessConfig {
        match role {
            Role::Prompter => self.steps.plan.clone(),
            Role::Coder => self.steps.code.clone(),
            Role::Evaluator => self.steps.evaluation.clone(),
        }
        .unwrap_or_else(|| self.harness.clone())
    }
}

/// Parse a command line ("cmd arg1 arg2") into a [`HarnessConfig`].
/// Returns `None` when the line is empty.
fn parse_cmd(rest: &str) -> Option<HarnessConfig> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    let (cmd, args) = parts.split_first()?;
    Some(HarnessConfig {
        command: (*cmd).to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
    })
}

/// Apply directives from a `.orbit` config file onto `cfg`. Later layers
/// override earlier ones, so callers apply global, then project, then explicit.
///
/// Format (line-based):
/// ```text
/// # comment
/// harness claude-code-acp
/// step plan = claude --acp
/// step code = opencode --acp
/// step eval = pi.dev --acp
/// max_attempts 5
/// timeout 1200
/// ```
pub fn apply_orbit_config(cfg: &mut Config, contents: &str) {
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (kw, rest) = line
            .split_once(char::is_whitespace)
            .map(|(k, r)| (k, r.trim()))
            .unwrap_or((line, ""));
        match kw {
            "harness" => {
                if let Some(h) = parse_cmd(rest) {
                    cfg.harness = h;
                }
            }
            "step" => {
                // rest = "<name> = <cmd...>"
                if let Some((name, val)) = rest.split_once('=') {
                    if let Some(h) = parse_cmd(val.trim()) {
                        match name.trim() {
                            "plan" => cfg.steps.plan = Some(h),
                            "code" => cfg.steps.code = Some(h),
                            "eval" | "evaluation" => cfg.steps.evaluation = Some(h),
                            other => tracing::warn!(step = other, "unknown step name in config"),
                        }
                    }
                } else {
                    tracing::warn!(line = rest, "malformed step directive (expected 'step <name> = <cmd>')");
                }
            }
            "max_attempts" => {
                if let Ok(n) = rest.parse() {
                    cfg.r#loop.max_attempts = n;
                }
            }
            "timeout" => {
                if let Ok(n) = rest.parse() {
                    cfg.r#loop.prompt_timeout_secs = n;
                }
            }
            other => tracing::warn!(directive = other, "unknown config directive"),
        }
    }
}

/// Path to the global config file: `~/.orbit/config.orbit`.
pub fn home_config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".orbit").join("config.orbit"))
}

/// Path to the project config file: `<target>/.orbit/config.orbit`.
fn project_config_path(target: &Path) -> PathBuf {
    target.join(".orbit").join("config.orbit")
}

/// Load configuration with precedence (lowest to highest):
/// defaults < global (`~/.orbit/config.orbit`) < project (`<target>/.orbit/config.orbit`) < explicit `--config`.
pub fn load(explicit: Option<&str>, target: &Path) -> Config {
    let mut cfg = Config::default();

    if let Some(global) = home_config_path()
        && let Ok(contents) = std::fs::read_to_string(&global)
    {
        apply_orbit_config(&mut cfg, &contents);
    }

    let project = project_config_path(target);
    if let Ok(contents) = std::fs::read_to_string(&project) {
        apply_orbit_config(&mut cfg, &contents);
    }

    if let Some(explicit_path) = explicit
        && let Ok(contents) = std::fs::read_to_string(explicit_path)
    {
        apply_orbit_config(&mut cfg, &contents);
    }

    cfg
}

#[derive(Debug, Clone)]
pub struct RunConfig {
    pub target: PathBuf,
    pub config: Config,
    pub spec_path: PathBuf,
}

/// Persist the default harness command into `~/.orbit/config.orbit`,
/// preserving any existing `step`/`max_attempts`/`timeout` lines.
pub fn save_acp_default(config_path: &Path, command_str: &str) -> anyhow::Result<()> {
    let command_str = command_str.trim();
    if command_str.is_empty() {
        return Err(anyhow::anyhow!("Empty ACP command"));
    }

    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Preserve all non-`harness` lines from any existing config.
    let mut lines: Vec<String> = Vec::new();
    if let Ok(existing) = std::fs::read_to_string(config_path) {
        for line in existing.lines() {
            if line.trim_start().starts_with("harness") {
                continue;
            }
            lines.push(line.to_string());
        }
    }

    let mut out = format!("harness {command_str}\n");
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(config_path, out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_harness_directive() {
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, "harness opencode acp");
        assert_eq!(cfg.harness.command, "opencode");
        assert_eq!(cfg.harness.args, vec!["acp".to_string()]);
    }

    #[test]
    fn test_apply_steps_and_loop() {
        let input = r#"
# global config
harness claude-code-acp

step plan = claude --acp
step code = opencode --acp
step eval = pi.dev --acp

max_attempts 7
timeout 900
"#;
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, input);

        assert_eq!(cfg.steps.plan.as_ref().unwrap().command, "claude");
        assert_eq!(cfg.steps.plan.as_ref().unwrap().args, vec!["--acp".to_string()]);
        assert_eq!(cfg.steps.code.as_ref().unwrap().command, "opencode");
        assert_eq!(cfg.steps.evaluation.as_ref().unwrap().command, "pi.dev");
        assert_eq!(cfg.r#loop.max_attempts, 7);
        assert_eq!(cfg.r#loop.prompt_timeout_secs, 900);
    }

    #[test]
    fn test_evaluation_alias() {
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, "step evaluation = pi.dev --acp");
        assert_eq!(cfg.steps.evaluation.as_ref().unwrap().command, "pi.dev");
    }

    #[test]
    fn test_ignores_comments_blank_and_unknown() {
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, "# just a comment\n\n   \nbogus directive here\n");
        // Nothing changed from defaults.
        assert_eq!(cfg.harness.command, "claude-code-acp");
        assert!(cfg.steps.plan.is_none());
    }

    #[test]
    fn test_harness_for_uses_step_then_fallback() {
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, "harness fallback-acp\nstep code = opencode --acp");

        // Coder -> code step override.
        let coder = cfg.harness_for(Role::Coder);
        assert_eq!(coder.command, "opencode");
        // Prompter/Evaluator -> fallback harness.
        assert_eq!(cfg.harness_for(Role::Prompter).command, "fallback-acp");
        assert_eq!(cfg.harness_for(Role::Evaluator).command, "fallback-acp");
    }

    #[test]
    fn test_layering_overrides() {
        let mut cfg = Config::default();
        // Earlier layer.
        apply_orbit_config(&mut cfg, "harness first\nmax_attempts 3");
        // Later layer overrides harness, keeps max_attempts unless restated.
        apply_orbit_config(&mut cfg, "harness second");
        assert_eq!(cfg.harness.command, "second");
        assert_eq!(cfg.r#loop.max_attempts, 3);
    }

    #[test]
    fn test_save_acp_default_preserves_steps() {
        let dir = std::env::temp_dir().join(format!("orbit-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.orbit");
        std::fs::write(&path, "harness old-acp\nstep code = opencode --acp\nmax_attempts 4\n").unwrap();

        save_acp_default(&path, "claude-code-acp --debug").unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("harness claude-code-acp --debug"));
        assert!(contents.contains("step code = opencode --acp"));
        assert!(contents.contains("max_attempts 4"));
        // Only one harness line.
        assert_eq!(contents.matches("harness ").count(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}
