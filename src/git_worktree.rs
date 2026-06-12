use crate::cli::WorktreeAction;
use crate::error::OrbitError;
use crate::render::{self, BLD, DIM, GRN, RED, YLW};
use std::io::Write;

pub fn dispatch(action: &WorktreeAction) -> Result<(), OrbitError> {
    match action {
        WorktreeAction::List => list_worktrees(),
        WorktreeAction::Add { path, branch } => add_worktree(path, branch.as_deref()),
        WorktreeAction::Remove { path } => remove_worktree(path),
    }
}

fn is_git_repo() -> Result<(), OrbitError> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .output()
        .map_err(OrbitError::Io)?;
    if !output.status.success() {
        return Err(OrbitError::Other("Not a git repository".to_string()));
    }
    Ok(())
}

fn list_worktrees() -> Result<(), OrbitError> {
    is_git_repo()?;

    let output = std::process::Command::new("git")
        .args(["worktree", "list"])
        .output()
        .map_err(OrbitError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OrbitError::Other(format!("git worktree list failed: {}", stderr.trim())));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.trim().is_empty()).collect();

    let _ = writeln!(
        std::io::stdout(),
        "  {} {}",
        render::c("───", DIM),
        render::c("WORKTREES", BLD)
    );

    if lines.is_empty() {
        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("●", YLW),
            render::c("No worktrees found", DIM)
        );
        return Ok(());
    }

    for line in &lines {
        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("▸", GRN),
            render::c(line.trim(), BLD)
        );
    }

    let _ = writeln!(
        std::io::stdout(),
        "  {} {}",
        render::c("●", GRN),
        render::c(&format!("{} worktree(s)", lines.len()), DIM)
    );

    Ok(())
}

fn add_worktree(path: &str, branch: Option<&str>) -> Result<(), OrbitError> {
    is_git_repo()?;

    let _ = writeln!(
        std::io::stdout(),
        "  {} {} {}",
        render::c("───", DIM),
        render::c("ADD WORKTREE", BLD),
        render::c(path, DIM)
    );

    let abs_path = std::path::Path::new(path);
    if abs_path.exists() {
        return Err(OrbitError::Other(format!(
            "Path already exists: {}",
            path
        )));
    }

    let mut cmd = std::process::Command::new("git");
    cmd.args(["worktree", "add"]);

    if let Some(b) = branch {
        cmd.arg("-b").arg(b);
    }

    cmd.arg(path);

    let output = cmd.output().map_err(OrbitError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OrbitError::Other(format!("git worktree add failed: {}", stderr.trim())));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let t = line.trim();
        if !t.is_empty() {
            let _ = writeln!(std::io::stdout(), "  {} {}", render::c("▸", GRN), render::c(t, BLD));
        }
    }

    let _ = writeln!(
        std::io::stdout(),
        "  {} {}",
        render::c("✓", GRN),
        render::c("Worktree created successfully.", BLD)
    );

    Ok(())
}

fn remove_worktree(path: &str) -> Result<(), OrbitError> {
    is_git_repo()?;

    let prompt = format!("Remove worktree at '{}'?", path);
    if !confirm(&prompt, false) {
        let _ = writeln!(
            std::io::stdout(),
            "  {} {}",
            render::c("●", RED),
            render::c("Aborted by user.", BLD)
        );
        return Ok(());
    }

    let _ = writeln!(
        std::io::stdout(),
        "  {} {} {}",
        render::c("───", DIM),
        render::c("REMOVE WORKTREE", BLD),
        render::c(path, DIM)
    );

    let output = std::process::Command::new("git")
        .args(["worktree", "remove", path])
        .output()
        .map_err(OrbitError::Io)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(OrbitError::Other(format!("git worktree remove failed: {}", stderr.trim())));
    }

    let _ = writeln!(
        std::io::stdout(),
        "  {} {}",
        render::c("✓", GRN),
        render::c("Worktree removed successfully.", BLD)
    );

    Ok(())
}

fn confirm(message: &str, default: bool) -> bool {
    let prompt = if default { "[Y/n]" } else { "[y/N]" };
    eprint!(
        "  {} {} {} ",
        render::c("?", YLW),
        render::c(message, BLD),
        render::c(prompt, DIM)
    );
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).ok();
    let input = input.trim().to_lowercase();
    match input.as_str() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}
