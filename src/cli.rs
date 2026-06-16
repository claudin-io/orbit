use clap::{Parser, Subcommand};
use std::path::PathBuf;

pub const ORBIT_VERSION: &str = env!("ORBIT_VERSION");

#[derive(Parser, Debug)]
#[command(name = "orbit", version = ORBIT_VERSION, about = "Autonomous coding loop")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(about = "Run autonomous coding loop")]
    Run {
        #[arg(long, short)]
        spec: Option<String>,

        #[arg(long)]
        goal: Option<String>,

        #[arg(long)]
        target: Option<String>,

        #[arg(long)]
        config: Option<String>,

        #[arg(long)]
        max_attempts: Option<u32>,

        #[arg(long)]
        acp: Option<String>,

        #[arg(long, short)]
        verbose: bool,
    },

    #[command(about = "Manage ACP agent configuration")]
    Acp {
        #[command(subcommand)]
        action: AcpAction,
    },

    #[command(about = "AI-assisted git operations")]
    Git {
        #[command(subcommand)]
        action: GitCliAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum AcpAction {
    #[command(name = "set-default")]
    SetDefault {
        command: String,
    },

    #[command(name = "handshake")]
    Handshake,
}

#[derive(Subcommand, Debug)]
pub enum GitCliAction {
    #[command(about = "Analyze changes and create a commit (3-agent loop: plan → review → execute)")]
    Commit {
        #[arg(long, short, help = "Stage all changes before committing")]
        all: bool,

        #[arg(long, short = 'y', help = "Skip confirmation prompt")]
        yes: bool,
    },

    #[command(about = "Manage git worktrees")]
    Worktree {
        #[command(subcommand)]
        action: WorktreeAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum WorktreeAction {
    #[command(about = "List all worktrees")]
    List,

    #[command(about = "Add a new worktree")]
    Add {
        #[arg(help = "Path for the new worktree")]
        path: String,

        #[arg(long, short, help = "Branch to checkout in the new worktree")]
        branch: Option<String>,
    },

    #[command(about = "Remove a worktree")]
    Remove {
        #[arg(help = "Path of the worktree to remove")]
        path: String,
    },
}

pub fn resolve_config(cli: &Cli) -> anyhow::Result<RunConfig> {
    let Command::Run {
        spec,
        goal,
        target,
        config,
        max_attempts,
        acp,
        verbose: _,
    } = &cli.command
    else {
        anyhow::bail!("resolve_config called on non-Run command");
    };

    let target_path = target
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let mut cfg = crate::config::load(config.as_deref(), &target_path);

    if let Some(acp_cmd) = acp {
        let parts: Vec<&str> = acp_cmd.split_whitespace().collect();
        if !parts.is_empty() {
            cfg.harness.command = parts[0].to_string();
            cfg.harness.args = parts[1..].iter().map(|s| s.to_string()).collect();
        }
    }

    if let Some(ma) = max_attempts {
        cfg.r#loop.max_attempts = *ma;
    }

    let spec_path = match (spec, goal) {
        (Some(s), _) => {
            let path = PathBuf::from(s);
            if !path.exists() {
                open_editor_create(&path, &format!("Spec file {s} not found."))?;
            }
            path
        }
        (None, Some(g)) => {
            let tmp_dir = PathBuf::from("/tmp/orbit");
            std::fs::create_dir_all(&tmp_dir)?;
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S.%f");
            let goal_path = tmp_dir.join(format!("spec-{timestamp}.md"));
            std::fs::write(&goal_path, format!("# Goal\n\n{}", g))?;
            goal_path
        }
        (None, None) => {
            let tmp_dir = PathBuf::from("/tmp/orbit");
            std::fs::create_dir_all(&tmp_dir)?;
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S.%f");
            let goal_path = tmp_dir.join(format!("spec-{timestamp}.md"));
            open_editor_create(&goal_path, "No --spec or --goal provided.")?;
            goal_path
        }
    };

    Ok(RunConfig {
        target: target_path,
        config: cfg,
        spec_path,
    })
}

fn open_editor_create(path: &Path, reason: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let editor = std::env::var("ORBIT_EDITOR")
        .or_else(|_| std::env::var("EDITOR"))
        .or_else(|_| std::env::var("VISUAL"))
        .unwrap_or_else(|_| "nano".to_string());

    eprintln!("{reason} Opening {editor} to write your spec...");
    eprintln!("Save and exit the editor to continue.\n");

    std::fs::write(path, "")?;

    let status = std::process::Command::new(&editor)
        .arg(path)
        .status()
        .map_err(|e| anyhow::anyhow!("Failed to open editor '{editor}': {e}"))?;

    if !status.success() {
        return Err(anyhow::anyhow!("Editor '{editor}' exited with error"));
    }

    Ok(())
}

use crate::config::RunConfig;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_run_command_parses() {
        let cli = Cli::parse_from(["orbit", "run", "--spec", "test.md"]);
        match &cli.command {
            Command::Run { spec, .. } => assert_eq!(spec.as_deref(), Some("test.md")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_goal_flag_parses() {
        let cli = Cli::parse_from(["orbit", "run", "--goal", "implement feature"]);
        match &cli.command {
            Command::Run { goal, .. } => assert_eq!(goal.as_deref(), Some("implement feature")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_acp_flag_parses() {
        let cli = Cli::parse_from(["orbit", "run", "--spec", "s.md", "--acp", "gemini acp"]);
        match &cli.command {
            Command::Run { acp, .. } => assert_eq!(acp.as_deref(), Some("gemini acp")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_max_attempts_parses() {
        let cli = Cli::parse_from(["orbit", "run", "--spec", "s.md", "--max-attempts", "3"]);
        match &cli.command {
            Command::Run { max_attempts, .. } => assert_eq!(*max_attempts, Some(3)),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn test_acp_set_default_parses() {
        let cli = Cli::parse_from(["orbit", "acp", "set-default", "opencode acp"]);
        match &cli.command {
            Command::Acp { action } => match action {
                AcpAction::SetDefault { command } => assert_eq!(command, "opencode acp"),
                _ => panic!("expected SetDefault"),
            },
            _ => panic!("expected Acp"),
        }
    }

    #[test]
    fn test_acp_handshake_parses() {
        let cli = Cli::parse_from(["orbit", "acp", "handshake"]);
        match &cli.command {
            Command::Acp { action } => assert!(matches!(action, AcpAction::Handshake)),
            _ => panic!("expected Acp"),
        }
    }

    #[test]
    fn test_git_commit_parses() {
        let cli = Cli::parse_from(["orbit", "git", "commit"]);
        match &cli.command {
            Command::Git { action } => assert!(matches!(action, GitCliAction::Commit { all: false, .. })),
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_commit_all_parses() {
        let cli = Cli::parse_from(["orbit", "git", "commit", "--all"]);
        match &cli.command {
            Command::Git { action } => assert!(matches!(action, GitCliAction::Commit { all: true, .. })),
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_commit_yes_parses() {
        let cli = Cli::parse_from(["orbit", "git", "commit", "-y"]);
        match &cli.command {
            Command::Git { action } => assert!(matches!(action, GitCliAction::Commit { yes: true, .. })),
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_worktree_list_parses() {
        let cli = Cli::parse_from(["orbit", "git", "worktree", "list"]);
        match &cli.command {
            Command::Git { action } => assert!(matches!(action, GitCliAction::Worktree { action: WorktreeAction::List })),
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_worktree_add_parses() {
        let cli = Cli::parse_from(["orbit", "git", "worktree", "add", "../hotfix"]);
        match &cli.command {
            Command::Git { action } => {
                assert!(matches!(action, GitCliAction::Worktree { action: WorktreeAction::Add { path, branch: None } } if path == "../hotfix"));
            }
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_worktree_add_with_branch_parses() {
        let cli = Cli::parse_from(["orbit", "git", "worktree", "add", "-b", "fix/api", "../fix"]);
        match &cli.command {
            Command::Git { action } => {
                assert!(matches!(action, GitCliAction::Worktree { action: WorktreeAction::Add { path, branch: Some(b) } } if path == "../fix" && b == "fix/api"));
            }
            _ => panic!("expected Git"),
        }
    }

    #[test]
    fn test_git_worktree_remove_parses() {
        let cli = Cli::parse_from(["orbit", "git", "worktree", "remove", "../hotfix"]);
        match &cli.command {
            Command::Git { action } => {
                assert!(matches!(action, GitCliAction::Worktree { action: WorktreeAction::Remove { path } } if path == "../hotfix"));
            }
            _ => panic!("expected Git"),
        }
    }
}
