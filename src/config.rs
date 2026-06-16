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
    // The native `claude` CLI: orbit talks to it directly over stream-json
    // (see harness::claude), so no ACP adapter needs to be installed.
    "claude".to_string()
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
    pub acp_retry_max_attempts: u32,
    pub acp_retry_base_delay_ms: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            prompt_timeout_secs: default_prompt_timeout_secs(),
            acp_retry_max_attempts: default_acp_retry_max_attempts(),
            acp_retry_base_delay_ms: default_acp_retry_base_delay_ms(),
        }
    }
}

fn default_max_attempts() -> u32 {
    5
}
fn default_prompt_timeout_secs() -> u64 {
    1200
}
fn default_acp_retry_max_attempts() -> u32 {
    3
}
fn default_acp_retry_base_delay_ms() -> u64 {
    1000
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
impl HarnessConfig {
    /// Parse a command line ("cmd arg1 arg2") into a [`HarnessConfig`].
    /// Returns `None` when the line has no command token.
    pub fn parse(line: &str) -> Option<HarnessConfig> {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (cmd, args) = parts.split_first()?;
        Some(HarnessConfig {
            command: (*cmd).to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
        })
    }

    /// Render back to a command line ("cmd arg1 arg2").
    pub fn to_command_line(&self) -> String {
        if self.args.is_empty() {
            self.command.clone()
        } else {
            format!("{} {}", self.command, self.args.join(" "))
        }
    }
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
                if let Some(h) = HarnessConfig::parse(rest) {
                    cfg.harness = h;
                }
            }
            "step" => {
                // rest = "<name> = <cmd...>"
                if let Some((name, val)) = rest.split_once('=') {
                    if let Some(h) = HarnessConfig::parse(val.trim()) {
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
            "acp_retry_max_attempts" => {
                if let Ok(n) = rest.parse() {
                    cfg.r#loop.acp_retry_max_attempts = n;
                }
            }
            "acp_retry_base_delay_ms" => {
                if let Ok(n) = rest.parse() {
                    cfg.r#loop.acp_retry_base_delay_ms = n;
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
pub fn project_config_path(target: &Path) -> PathBuf {
    target.join(".orbit").join("config.orbit")
}

/// Load configuration with precedence (lowest to highest):
/// defaults < global (`~/.orbit/config.orbit`) < project (`<target>/.orbit/config.orbit`) < explicit `--config`.
pub fn load(explicit: Option<&str>, target: &Path) -> Config {
    let mut cfg = Config::default();

    if let Some(global) = home_config_path() {
        match std::fs::read_to_string(&global) {
            Ok(contents) => {
                tracing::debug!(path = %global.display(), "loading global config");
                apply_orbit_config(&mut cfg, &contents);
            }
            Err(_) => tracing::debug!(path = %global.display(), "no global config"),
        }
    }

    let project = project_config_path(target);
    match std::fs::read_to_string(&project) {
        Ok(contents) => {
            tracing::debug!(path = %project.display(), "loading project config");
            apply_orbit_config(&mut cfg, &contents);
        }
        Err(_) => tracing::debug!(path = %project.display(), "no project config"),
    }

    if let Some(explicit_path) = explicit {
        match std::fs::read_to_string(explicit_path) {
            Ok(contents) => {
                tracing::debug!(path = explicit_path, "loading explicit config");
                apply_orbit_config(&mut cfg, &contents);
            }
            Err(e) => tracing::debug!(path = explicit_path, error = %e, "explicit config unreadable"),
        }
    }

    tracing::debug!(
        harness = %cfg.harness.to_command_line(),
        plan = ?cfg.steps.plan.as_ref().map(|h| h.to_command_line()),
        code = ?cfg.steps.code.as_ref().map(|h| h.to_command_line()),
        eval = ?cfg.steps.evaluation.as_ref().map(|h| h.to_command_line()),
        max_attempts = cfg.r#loop.max_attempts,
        timeout_secs = cfg.r#loop.prompt_timeout_secs,
        acp_retry_max_attempts = cfg.r#loop.acp_retry_max_attempts,
        acp_retry_base_delay_ms = cfg.r#loop.acp_retry_base_delay_ms,
        "config resolved"
    );

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

/// Write a full `.orbit` config: the `harness` line followed by `step` lines for
/// each configured step. Any existing `max_attempts`/`timeout`/comment lines are
/// preserved (only `harness`/`step` lines are replaced).
pub fn write_orbit_config(
    path: &Path,
    harness: &HarnessConfig,
    steps: &StepsConfig,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Preserve everything except existing harness/step lines.
    let mut preserved: Vec<String> = Vec::new();
    if let Ok(existing) = std::fs::read_to_string(path) {
        for line in existing.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("harness") || trimmed.starts_with("step") {
                continue;
            }
            preserved.push(line.to_string());
        }
    }

    let mut out = format!("harness {}\n", harness.to_command_line());
    if let Some(h) = &steps.plan {
        out.push_str(&format!("step plan = {}\n", h.to_command_line()));
    }
    if let Some(h) = &steps.code {
        out.push_str(&format!("step code = {}\n", h.to_command_line()));
    }
    if let Some(h) = &steps.evaluation {
        out.push_str(&format!("step eval = {}\n", h.to_command_line()));
    }
    for line in preserved {
        out.push_str(&line);
        out.push('\n');
    }
    std::fs::write(path, out)?;
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
    fn test_apply_acp_retry_directives() {
        let mut cfg = Config::default();
        apply_orbit_config(
            &mut cfg,
            "acp_retry_max_attempts 5\nacp_retry_base_delay_ms 2000\n",
        );
        assert_eq!(cfg.r#loop.acp_retry_max_attempts, 5);
        assert_eq!(cfg.r#loop.acp_retry_base_delay_ms, 2000);
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
        assert_eq!(cfg.harness.command, "claude");
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

    #[test]
    fn test_harness_config_parse() {
        let hc = HarnessConfig::parse("claude --acp --flag").unwrap();
        assert_eq!(hc.command, "claude");
        assert_eq!(hc.args, vec!["--acp".to_string(), "--flag".to_string()]);
        assert_eq!(hc.to_command_line(), "claude --acp --flag");

        assert!(HarnessConfig::parse("").is_none());
        assert!(HarnessConfig::parse("   ").is_none());
    }

    #[test]
    fn test_write_orbit_config_with_steps_preserves_other_lines() {
        let dir = std::env::temp_dir().join(format!("orbit-write-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.orbit");
        // Existing config with steps to be replaced and loop settings to keep.
        std::fs::write(
            &path,
            "# my config\nharness old\nstep code = old-code\nmax_attempts 7\ntimeout 900\n",
        )
        .unwrap();

        let harness = HarnessConfig::parse("claude-code-acp").unwrap();
        let steps = StepsConfig {
            plan: HarnessConfig::parse("claude --acp"),
            code: None,
            evaluation: HarnessConfig::parse("pi.dev --acp"),
        };
        write_orbit_config(&path, &harness, &steps).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("harness claude-code-acp"));
        assert!(contents.contains("step plan = claude --acp"));
        assert!(contents.contains("step eval = pi.dev --acp"));
        // code step omitted (None) and old step replaced.
        assert!(!contents.contains("step code"));
        // Preserved lines.
        assert!(contents.contains("# my config"));
        assert!(contents.contains("max_attempts 7"));
        assert!(contents.contains("timeout 900"));
        assert_eq!(contents.matches("harness ").count(), 1);

        // Re-parsing yields the configured values.
        let mut cfg = Config::default();
        apply_orbit_config(&mut cfg, &contents);
        assert_eq!(cfg.harness.command, "claude-code-acp");
        assert_eq!(cfg.steps.plan.as_ref().unwrap().command, "claude");
        assert!(cfg.steps.code.is_none());
        assert_eq!(cfg.steps.evaluation.as_ref().unwrap().command, "pi.dev");
        assert_eq!(cfg.r#loop.max_attempts, 7);

        std::fs::remove_dir_all(&dir).ok();
    }
}
