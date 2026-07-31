//! Per-worktree durable memory compatible with the standalone Local Agent layout
//! (``.local_agent/JOURNAL.md`` and ``.local_agent/project_summary.md``).

use gpui::{App, Entity};
use project::Project;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const STATE_DIR_NAME: &str = ".local_agent";
const JOURNAL_FILENAME: &str = "JOURNAL.md";
const PROJECT_SUMMARY_FILENAME: &str = "project_summary.md";

pub fn state_dir_for_worktree(worktree_root: &Path) -> PathBuf {
    worktree_root.join(STATE_DIR_NAME)
}

pub fn journal_path(worktree_root: &Path) -> PathBuf {
    state_dir_for_worktree(worktree_root).join(JOURNAL_FILENAME)
}

pub fn project_summary_path(worktree_root: &Path) -> PathBuf {
    state_dir_for_worktree(worktree_root).join(PROJECT_SUMMARY_FILENAME)
}

pub fn primary_worktree_root(project: &Entity<Project>, cx: &App) -> Option<PathBuf> {
    worktree_roots(project, cx).into_iter().next()
}

pub fn worktree_roots(project: &Entity<Project>, cx: &App) -> Vec<PathBuf> {
    project
        .read(cx)
        .worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect()
}

pub fn read_journal(worktree_root: &Path) -> std::io::Result<String> {
    let path = journal_path(worktree_root);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

pub fn append_journal(worktree_root: &Path, note: &str) -> std::io::Result<()> {
    let note = note.trim();
    if note.is_empty() {
        return Ok(());
    }

    let state_dir = state_dir_for_worktree(worktree_root);
    std::fs::create_dir_all(&state_dir)?;
    let path = journal_path(worktree_root);
    let header = if path.exists() {
        ""
    } else {
        "# Project Journal\n"
    };
    let stamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let entry = format!("\n## {stamp}\n{note}\n");

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{header}{entry}").as_bytes())
}

pub fn read_project_summary(worktree_root: &Path) -> std::io::Result<Option<String>> {
    let path = project_summary_path(worktree_root);
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let trimmed = contents.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn write_project_summary(worktree_root: &Path, summary: &str) -> std::io::Result<()> {
    let summary = summary.trim();
    if summary.is_empty() {
        return Ok(());
    }
    let state_dir = state_dir_for_worktree(worktree_root);
    std::fs::create_dir_all(&state_dir)?;
    std::fs::write(project_summary_path(worktree_root), format!("{summary}\n"))
}

/// Builds the memory block injected into new thread system prompts.
pub fn format_project_memory_context(worktree_roots: &[PathBuf]) -> Option<String> {
    let mut parts = Vec::new();

    for root in worktree_roots {
        if let Ok(Some(summary)) = read_project_summary(root) {
            parts.push(format!(
                "Project summary for `{}` (from a previous session):\n{summary}",
                root.display()
            ));
        }
        if let Ok(journal) = read_journal(root)
            && !journal.trim().is_empty()
        {
            parts.push(format!(
                "Project journal for `{}` (durable notes):\n{}",
                root.display(),
                journal.trim()
            ));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

pub fn persist_summary_to_worktrees(project: &Entity<Project>, summary: &str, cx: &App) {
    for root in worktree_roots(project, cx) {
        if let Err(error) = write_project_summary(&root, summary) {
            log::warn!(
                "Failed to write project summary for {}: {error:#}",
                root.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_journal_append_and_read() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        assert_eq!(read_journal(root).unwrap(), "");
        append_journal(root, "First decision.").unwrap();
        append_journal(root, "Second decision.").unwrap();

        let text = read_journal(root).unwrap();
        assert!(text.contains("# Project Journal"));
        assert!(text.contains("First decision."));
        assert!(text.contains("Second decision."));
    }

    #[test]
    fn test_project_summary_round_trip() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        assert_eq!(read_project_summary(root).unwrap(), None);
        write_project_summary(root, "Shipped feature X.").unwrap();
        assert_eq!(
            read_project_summary(root).unwrap().as_deref(),
            Some("Shipped feature X.")
        );
    }

    #[test]
    fn test_format_project_memory_context() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        write_project_summary(&root, "Summary text.").unwrap();
        append_journal(&root, "Journal note.").unwrap();

        let context = format_project_memory_context(std::slice::from_ref(&root)).unwrap();
        assert!(context.contains("Summary text."));
        assert!(context.contains("Journal note."));
    }
}
