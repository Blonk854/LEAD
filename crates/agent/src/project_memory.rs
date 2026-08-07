//! Per-worktree durable memory compatible with the standalone Local Agent layout
//! (``.local_agent/JOURNAL.md`` and ``.local_agent/project_summary.md``).

use gpui::{App, Entity};
use project::Project;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const STATE_DIR_NAME: &str = ".local_agent";
const JOURNAL_FILENAME: &str = "JOURNAL.md";
const PROJECT_SUMMARY_FILENAME: &str = "project_summary.md";

/// Max fact / content lines from the journal injected into new-thread prompts.
/// Keeps WARM memory from consuming the small-window HOT budget as the log grows.
const JOURNAL_INJECTION_MAX_LINES: usize = 25;

/// Max `TAG | claim` lines accepted in a single journal append (one timestamp section).
const JOURNAL_FACT_BATCH_MAX: usize = 8;

/// Skip identical fact lines already present in the most recent journal facts.
const JOURNAL_FACT_DEDUP_WINDOW: usize = 20;

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
        .filter_map(|worktree| {
            let worktree = worktree.read(cx);
            // Journal/summary use local std::fs. Skip single-file tabs (thread
            // exports) and remote roots that are not writable this way.
            if worktree.is_single_file() || !worktree.is_local() {
                return None;
            }
            let path = worktree.abs_path().to_path_buf();
            path.is_dir().then_some(path)
        })
        .collect()
}

fn ensure_directory_worktree(worktree_root: &Path) -> std::io::Result<()> {
    if worktree_root.is_dir() {
        return Ok(());
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotADirectory,
        format!(
            "project memory requires a directory worktree, got {}",
            worktree_root.display()
        ),
    ))
}

pub fn read_journal(worktree_root: &Path) -> std::io::Result<String> {
    if !worktree_root.is_dir() {
        return Ok(String::new());
    }
    let path = journal_path(worktree_root);
    match std::fs::read_to_string(&path) {
        Ok(contents) => Ok(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error),
    }
}

/// Result of appending to the project journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalAppendOutcome {
    /// Lines actually written into the new timestamped section.
    pub lines_written: usize,
    /// Fact lines skipped because they matched recent journal entries.
    pub duplicates_skipped: usize,
    /// Whether the written body was a structured `TAG | claim` batch.
    pub is_facts: bool,
}

/// Options for [`append_journal_with_options`].
#[derive(Debug, Clone, Copy)]
pub struct JournalAppendOptions {
    /// When true and facts were written, upsert managed headings in
    /// `project_summary.md`. Compaction callers pass false so a fresh handoff
    /// summary is not immediately rewritten from journal one-liners.
    pub sync_summary: bool,
}

impl Default for JournalAppendOptions {
    fn default() -> Self {
        Self {
            sync_summary: true,
        }
    }
}

/// Appends a note (or a batch of fact lines) as a single timestamped journal section.
///
/// When the note contains `TAG | claim` lines, they are normalized into one batch
/// of at most [`JOURNAL_FACT_BATCH_MAX`] lines, de-duplicated within the batch and
/// against the most recent journal facts. Freeform notes (no fact lines) are
/// stored as-is. Successful fact writes sync managed spine headings by default.
pub fn append_journal(
    worktree_root: &Path,
    note: &str,
) -> std::io::Result<JournalAppendOutcome> {
    append_journal_with_options(worktree_root, note, JournalAppendOptions::default())
}

/// Like [`append_journal`], with explicit control over spine sync.
pub fn append_journal_with_options(
    worktree_root: &Path,
    note: &str,
    options: JournalAppendOptions,
) -> std::io::Result<JournalAppendOutcome> {
    let Some(prepared) = prepare_journal_note(worktree_root, note)? else {
        return Ok(JournalAppendOutcome {
            lines_written: 0,
            duplicates_skipped: 0,
            is_facts: false,
        });
    };
    if prepared.lines_written == 0 {
        return Ok(JournalAppendOutcome {
            lines_written: 0,
            duplicates_skipped: prepared.duplicates_skipped,
            is_facts: prepared.is_facts,
        });
    }

    ensure_directory_worktree(worktree_root)?;

    let state_dir = state_dir_for_worktree(worktree_root);
    std::fs::create_dir_all(&state_dir)?;
    let path = journal_path(worktree_root);
    let header = if path.exists() {
        ""
    } else {
        "# Project Journal\n"
    };
    let stamp = chrono::Utc::now().format("%Y-%m-%d %H:%M UTC");
    let entry = format!("\n## {stamp}\n{}\n", prepared.body);

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(format!("{header}{entry}").as_bytes())?;

    if options.sync_summary && prepared.is_facts && prepared.lines_written > 0 {
        if let Err(error) = sync_project_summary_from_journal(worktree_root) {
            log::warn!(
                "Failed to sync project summary from journal for {}: {error:#}",
                worktree_root.display()
            );
        }
    }

    Ok(JournalAppendOutcome {
        lines_written: prepared.lines_written,
        duplicates_skipped: prepared.duplicates_skipped,
        is_facts: prepared.is_facts,
    })
}

struct PreparedJournalNote {
    body: String,
    lines_written: usize,
    duplicates_skipped: usize,
    is_facts: bool,
}

fn prepare_journal_note(
    worktree_root: &Path,
    note: &str,
) -> std::io::Result<Option<PreparedJournalNote>> {
    let Some(normalized) = normalize_journal_batch(note) else {
        return Ok(None);
    };

    if !normalized.is_facts {
        return Ok(Some(PreparedJournalNote {
            lines_written: normalized.lines.len().max(1),
            duplicates_skipped: 0,
            body: normalized.lines.join("\n"),
            is_facts: false,
        }));
    }

    let existing = if worktree_root.is_dir() {
        read_journal(worktree_root)?
    } else {
        String::new()
    };
    let recent = extract_journal_fact_lines(&existing);
    let recent_start = recent.len().saturating_sub(JOURNAL_FACT_DEDUP_WINDOW);
    let recent_keys: std::collections::HashSet<String> = recent[recent_start..]
        .iter()
        .map(|line| fact_dedupe_key(line))
        .collect();

    let mut kept = Vec::new();
    let mut duplicates_skipped = 0usize;
    for line in normalized.lines {
        let key = fact_dedupe_key(&line);
        if recent_keys.contains(&key) || kept.iter().any(|k: &String| fact_dedupe_key(k) == key)
        {
            duplicates_skipped += 1;
            continue;
        }
        kept.push(line);
    }

    if kept.is_empty() {
        return Ok(Some(PreparedJournalNote {
            body: String::new(),
            lines_written: 0,
            duplicates_skipped,
            is_facts: true,
        }));
    }

    Ok(Some(PreparedJournalNote {
        lines_written: kept.len(),
        duplicates_skipped,
        body: kept.join("\n"),
        is_facts: true,
    }))
}

#[derive(Debug, PartialEq, Eq)]
pub struct NormalizedJournalBatch {
    pub lines: Vec<String>,
    pub is_facts: bool,
}

/// Normalizes a journal note into either a capped fact batch or a freeform body.
pub fn normalize_journal_batch(note: &str) -> Option<NormalizedJournalBatch> {
    let trimmed = note.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut facts = Vec::new();
    for line in trimmed.lines() {
        let line = line.trim();
        if !is_fact_line(line) {
            continue;
        }
        let key = fact_dedupe_key(line);
        if facts
            .iter()
            .any(|existing: &String| fact_dedupe_key(existing) == key)
        {
            continue;
        }
        facts.push(line.to_string());
        if facts.len() >= JOURNAL_FACT_BATCH_MAX {
            break;
        }
    }

    if !facts.is_empty() {
        return Some(NormalizedJournalBatch {
            lines: facts,
            is_facts: true,
        });
    }

    Some(NormalizedJournalBatch {
        lines: vec![trimmed.to_string()],
        is_facts: false,
    })
}

/// Dedupe key is normalized `tag|claim` only (optional evidence after the second
/// `|` is ignored so evidence drift does not create near-duplicate facts).
fn fact_dedupe_key(line: &str) -> String {
    let Some((tag, rest)) = line.split_once('|') else {
        return normalize_ws_lower(line);
    };
    let claim = rest.split('|').next().unwrap_or(rest);
    format!("{}|{}", normalize_ws_lower(tag), normalize_ws_lower(claim))
}

fn normalize_ws_lower(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

/// Managed `##` headings upserted from journal facts into `project_summary.md`.
const MANAGED_SPINE_HEADINGS: &[&str] = &["Goal", "State", "Decisions", "Next", "Pitfalls"];

/// Upserts managed spine headings in `project_summary.md` from journal facts.
///
/// Only rewrites the five managed `##` headings. Replaces a heading only when
/// the journal supplies a non-empty value for it — never clears an existing
/// heading. Preserves all non-managed content as a trailing freeform body.
/// No-ops when the journal has no usable facts for those fields.
pub fn sync_project_summary_from_journal(worktree_root: &Path) -> std::io::Result<bool> {
    if !worktree_root.is_dir() {
        return Ok(false);
    }
    let journal = read_journal(worktree_root)?;
    let facts = extract_journal_fact_lines(&journal);
    if facts.is_empty() {
        return Ok(false);
    }

    let derived = derive_spine_fields(&facts);
    if derived.values().all(|v| v.is_empty()) {
        return Ok(false);
    }

    let existing = read_project_summary(worktree_root)?.unwrap_or_default();
    let (mut managed, freeform) = parse_managed_and_freeform(&existing);

    for heading in MANAGED_SPINE_HEADINGS {
        if let Some(value) = derived.get(*heading)
            && !value.is_empty()
        {
            managed.insert((*heading).to_string(), value.clone());
        }
    }

    if managed.is_empty() && freeform.trim().is_empty() {
        return Ok(false);
    }

    let mut out = String::new();
    for heading in MANAGED_SPINE_HEADINGS {
        if let Some(body) = managed.get(*heading) {
            let body = body.trim();
            if body.is_empty() {
                continue;
            }
            out.push_str("## ");
            out.push_str(heading);
            out.push('\n');
            out.push_str(body);
            out.push_str("\n\n");
        }
    }
    // Keep freeform under a non-managed `## Context` heading so the next sync
    // does not absorb trailing prose into the last managed section body.
    let freeform = freeform.trim();
    if !freeform.is_empty() {
        let already_headed = freeform.lines().next().is_some_and(|line| {
            line.strip_prefix("## ")
                .is_some_and(|h| !MANAGED_SPINE_HEADINGS.iter().any(|m| *m == h.trim()))
        });
        if !already_headed {
            out.push_str("## Context\n");
        }
        out.push_str(freeform);
        out.push('\n');
    }

    let trimmed = out.trim();
    if trimmed.is_empty() {
        return Ok(false);
    }
    if existing.trim() == trimmed {
        return Ok(false);
    }

    atomic_write_project_summary(worktree_root, trimmed)?;
    Ok(true)
}

fn derive_spine_fields(facts: &[String]) -> std::collections::BTreeMap<&'static str, String> {
    let mut fields = std::collections::BTreeMap::new();

    if let Some(goal) = facts.iter().rev().find(|line| fact_has_tag(line, "GOAL")) {
        fields.insert("Goal", fact_display_body(goal));
    }

    let state_line = facts
        .iter()
        .rev()
        .find(|line| {
            fact_has_tag(line, "FIX") || fact_has_tag(line, "FAIL") || fact_has_tag(line, "BLOCK")
        })
        .cloned();
    if let Some(ref state) = state_line {
        fields.insert("State", fact_display_body(state));
    }

    let decisions: Vec<String> = facts
        .iter()
        .rev()
        .filter(|line| fact_has_tag(line, "DEC"))
        .take(5)
        .map(|line| fact_display_body(line))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !decisions.is_empty() {
        fields.insert("Decisions", decisions.join("\n"));
    }

    if let Some(next) = facts.iter().rev().find(|line| fact_has_tag(line, "NEXT")) {
        fields.insert("Next", fact_display_body(next));
    }

    let state_key = state_line.as_ref().map(|line| fact_dedupe_key(line));
    let pitfalls: Vec<String> = facts
        .iter()
        .rev()
        .filter(|line| fact_has_tag(line, "FAIL") || fact_has_tag(line, "BLOCK"))
        .filter(|line| state_key.as_ref().is_none_or(|key| &fact_dedupe_key(line) != key))
        .take(3)
        .map(|line| fact_display_body(line))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if !pitfalls.is_empty() {
        fields.insert("Pitfalls", pitfalls.join("\n"));
    }

    fields
}

fn fact_has_tag(line: &str, tag: &str) -> bool {
    line.split_once('|')
        .is_some_and(|(left, _)| left.trim().eq_ignore_ascii_case(tag))
}

fn fact_display_body(line: &str) -> String {
    line.split_once('|')
        .map(|(_, rest)| rest.trim().to_string())
        .unwrap_or_else(|| line.trim().to_string())
}

fn parse_managed_and_freeform(summary: &str) -> (std::collections::BTreeMap<String, String>, String) {
    let mut managed = std::collections::BTreeMap::new();
    let mut freeform = String::new();
    let mut current_managed: Option<String> = None;
    let mut current_body = String::new();

    let flush_managed = |heading: &str,
                         body: &str,
                         managed: &mut std::collections::BTreeMap<String, String>| {
        let body = body.trim();
        if !body.is_empty() {
            managed.insert(heading.to_string(), body.to_string());
        }
    };

    for line in summary.lines() {
        if let Some(heading) = line.strip_prefix("## ").map(str::trim) {
            if let Some(prev) = current_managed.take() {
                flush_managed(&prev, &current_body, &mut managed);
                current_body.clear();
            }
            if MANAGED_SPINE_HEADINGS.iter().any(|h| *h == heading) {
                current_managed = Some(heading.to_string());
            } else {
                if !freeform.is_empty() {
                    freeform.push('\n');
                }
                freeform.push_str(line);
            }
            continue;
        }

        if current_managed.is_some() {
            if !current_body.is_empty() {
                current_body.push('\n');
            }
            current_body.push_str(line);
        } else {
            if !freeform.is_empty() {
                freeform.push('\n');
            }
            freeform.push_str(line);
        }
    }

    if let Some(prev) = current_managed.take() {
        flush_managed(&prev, &current_body, &mut managed);
    }

    (managed, freeform)
}

fn atomic_write_project_summary(worktree_root: &Path, summary: &str) -> std::io::Result<()> {
    ensure_directory_worktree(worktree_root)?;
    let state_dir = state_dir_for_worktree(worktree_root);
    std::fs::create_dir_all(&state_dir)?;
    let path = project_summary_path(worktree_root);
    let tmp = state_dir.join(format!(
        ".{}.tmp",
        PROJECT_SUMMARY_FILENAME.trim_end_matches(".md")
    ));
    std::fs::write(&tmp, format!("{}\n", summary.trim()))?;
    // On Windows, rename fails if the destination exists.
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Returns the most recent journal fact lines across a worktree (newest last).
pub fn recent_journal_facts(worktree_root: &Path, limit: usize) -> Vec<String> {
    let Ok(journal) = read_journal(worktree_root) else {
        return Vec::new();
    };
    let facts = extract_journal_fact_lines(&journal);
    let start = facts.len().saturating_sub(limit);
    facts[start..].to_vec()
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
    ensure_directory_worktree(worktree_root)?;
    let state_dir = state_dir_for_worktree(worktree_root);
    std::fs::create_dir_all(&state_dir)?;
    std::fs::write(project_summary_path(worktree_root), format!("{summary}\n"))
}

/// Builds the memory block injected into new thread system prompts.
///
/// Includes the project spine (`project_summary`) plus only the **most recent**
/// journal facts — not the full append-only log — so small context windows keep
/// headroom for the active working set. Agents can still call `read_journal`
/// for the complete history.
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
            && let Some(tail) = journal_injection_tail(&journal)
        {
            parts.push(format!(
                "Recent project journal facts for `{}` (use `read_journal` for the full log):\n{tail}",
                root.display()
            ));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

/// Selects the last [`JOURNAL_INJECTION_MAX_LINES`] durable lines from a journal.
///
/// Prefers `TAG | claim` fact lines when present; otherwise falls back to the
/// last non-heading content lines (legacy freeform notes).
pub fn journal_injection_tail(journal: &str) -> Option<String> {
    let facts = extract_journal_fact_lines(journal);
    let selected = if !facts.is_empty() {
        let start = facts.len().saturating_sub(JOURNAL_INJECTION_MAX_LINES);
        facts[start..].to_vec()
    } else {
        let content = extract_journal_content_lines(journal);
        if content.is_empty() {
            return None;
        }
        let start = content.len().saturating_sub(JOURNAL_INJECTION_MAX_LINES);
        content[start..].to_vec()
    };

    if selected.is_empty() {
        None
    } else {
        Some(selected.join("\n"))
    }
}

fn extract_journal_fact_lines(journal: &str) -> Vec<String> {
    journal
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if is_fact_line(trimmed) {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .collect()
}

fn extract_journal_content_lines(journal: &str) -> Vec<String> {
    journal
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            Some(trimmed.to_string())
        })
        .collect()
}

/// `TAG | claim` (optional `| evidence`) with an uppercase tag token.
fn is_fact_line(line: &str) -> bool {
    let Some((tag, rest)) = line.split_once('|') else {
        return false;
    };
    let tag = tag.trim();
    let rest = rest.trim();
    !tag.is_empty()
        && !rest.is_empty()
        && tag.len() <= 16
        && tag
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch == '_')
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

/// Splits a compaction/handoff response into summary prose and an optional
/// trailing `FACTS` journal batch.
///
/// Only the **last** whole-line `FACTS` / `FACTS:` header is treated as the
/// journal boundary, and only when at least one `TAG | claim` line follows.
/// Mid-summary headings like "Facts:" therefore cannot truncate the handoff.
/// If splitting would leave an empty summary, the original text is kept intact.
pub fn split_summary_and_journal_facts(text: &str) -> (String, Option<String>) {
    let lines: Vec<&str> = text.lines().collect();
    let mut facts_at = None;
    for (ix, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("facts") || trimmed.eq_ignore_ascii_case("facts:") {
            facts_at = Some(ix);
        }
    }

    let Some(facts_at) = facts_at else {
        return (text.trim().to_string(), None);
    };

    let facts_body = lines[facts_at + 1..].join("\n");
    let Some(facts) = parse_journal_fact_extract_response(&facts_body) else {
        // Header without usable facts — keep the full text as summary.
        return (text.trim().to_string(), None);
    };

    let summary = lines[..facts_at].join("\n").trim().to_string();
    if summary.is_empty() {
        return (text.trim().to_string(), None);
    }

    (summary, Some(facts))
}

/// Parses a model response from journal fact extraction into a normalized batch body.
pub fn parse_journal_fact_extract_response(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return None;
    }

    let body = if let Some(rest) = trimmed.strip_prefix("FACTS") {
        let rest = rest.trim_start_matches(':').trim();
        if rest.is_empty() {
            return None;
        }
        rest
    } else {
        trimmed
    };

    let batch = normalize_journal_batch(body)?;
    if !batch.is_facts {
        return None;
    }
    Some(batch.lines.join("\n"))
}

/// Appends a normalized fact batch to every local directory worktree journal.
///
/// When `sync_summary` is false (compaction FACTS), the journal is updated but
/// `project_summary.md` is left alone — compaction already wrote a fresh summary.
pub fn persist_journal_facts_to_worktrees(
    project: &Entity<Project>,
    facts: &str,
    sync_summary: bool,
    cx: &App,
) {
    let facts = facts.trim();
    if facts.is_empty() {
        return;
    }
    let options = JournalAppendOptions { sync_summary };
    for root in worktree_roots(project, cx) {
        match append_journal_with_options(&root, facts, options) {
            Ok(outcome) if outcome.lines_written > 0 => {
                log::debug!(
                    "Auto-persisted {} journal fact(s) for {}",
                    outcome.lines_written,
                    root.display()
                );
            }
            Ok(_) => {}
            Err(error) => {
                log::warn!(
                    "Failed to append journal facts for {}: {error:#}",
                    root.display()
                );
            }
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
        assert!(context.contains("read_journal"));
    }

    #[test]
    fn test_journal_injection_prefers_fact_lines_and_tails() {
        let mut journal = String::from("# Project Journal\n");
        for i in 1..=40 {
            journal.push_str(&format!(
                "\n## 2026-01-01 00:{i:02} UTC\nDEC | decision number {i} | notes/x.md\n"
            ));
        }
        // Freeform noise that must not displace fact selection.
        journal.push_str("\n## aside\njust some prose without a tag\n");

        let tail = journal_injection_tail(&journal).unwrap();
        let lines: Vec<_> = tail.lines().collect();
        assert_eq!(lines.len(), JOURNAL_INJECTION_MAX_LINES);
        assert!(lines[0].contains("decision number 16"), "{tail}");
        assert!(lines.last().unwrap().contains("decision number 40"));
        assert!(!tail.contains("just some prose"));
    }

    #[test]
    fn test_journal_injection_falls_back_to_content_lines() {
        let journal = r#"
# Project Journal

## 2026-01-01 00:00 UTC
First freeform note.

## 2026-01-01 00:01 UTC
Second freeform note.
"#;
        let tail = journal_injection_tail(journal).unwrap();
        assert!(tail.contains("First freeform note."));
        assert!(tail.contains("Second freeform note."));
        assert!(!tail.contains("# Project Journal"));
    }

    #[test]
    fn test_format_project_memory_context_does_not_dump_full_journal() {
        let dir = TempDir::new().unwrap();
        let root = dir.path().to_path_buf();
        write_project_summary(&root, "Spine.").unwrap();
        for i in 1..=30 {
            append_journal(&root, &format!("DEC | item {i} | n/a")).unwrap();
        }

        let context = format_project_memory_context(std::slice::from_ref(&root)).unwrap();
        assert!(context.contains("Spine."));
        assert!(context.contains("DEC | item 30 | n/a"));
        assert!(!context.contains("DEC | item 1 | n/a"));
        assert_eq!(
            context
                .lines()
                .filter(|line| line.starts_with("DEC |"))
                .count(),
            JOURNAL_INJECTION_MAX_LINES
        );
    }

    #[test]
    fn test_project_memory_rejects_file_worktree_roots() {
        let dir = TempDir::new().unwrap();
        let file_root = dir.path().join("opened-thread-export.md");
        std::fs::write(&file_root, "# thread export\n").unwrap();

        let err = append_journal(&file_root, "should not write").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);

        let err = write_project_summary(&file_root, "should not write").unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);

        // Reading a file "worktree" should be a quiet no-op, not a hard error.
        assert_eq!(read_journal(&file_root).unwrap(), "");
        assert_eq!(read_project_summary(&file_root).unwrap(), None);
        assert!(!file_root.join(STATE_DIR_NAME).exists());
    }

    #[test]
    fn test_journal_append_still_works_when_journal_already_exists() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(root, "first").unwrap();
        append_journal(root, "second").unwrap();
        let text = read_journal(root).unwrap();
        assert!(text.contains("first"));
        assert!(text.contains("second"));
        // Overwriting summary repeatedly must not hit AlreadyExists.
        write_project_summary(root, "one").unwrap();
        write_project_summary(root, "two").unwrap();
        assert_eq!(read_project_summary(root).unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn test_normalize_journal_batch_caps_and_dedupes() {
        let note = "\
GOAL | Ship memory stress
DEC | Use SQLite
DEC | Use SQLite
PATH | notes/decisions.md
FAIL | bad path
FIX | recovered
NEXT | report
URL | https://example.com
PREF | no computer_use
BLOCK | waiting on rebuild
API | append_journal
EXTRA | should be dropped because over cap
";
        let batch = normalize_journal_batch(note).unwrap();
        assert!(batch.is_facts);
        assert_eq!(batch.lines.len(), JOURNAL_FACT_BATCH_MAX);
        assert!(batch.lines.iter().any(|line| line.starts_with("GOAL |")));
        assert_eq!(
            batch
                .lines
                .iter()
                .filter(|line| line.contains("Use SQLite"))
                .count(),
            1
        );
        assert!(!batch.lines.iter().any(|line| line.starts_with("EXTRA |")));
    }

    #[test]
    fn test_append_journal_batches_facts_into_one_section() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let outcome = append_journal(
            root,
            "\
GOAL | Batch test
DEC | One section only
NEXT | Verify timestamp count
",
        )
        .unwrap();
        assert_eq!(outcome.lines_written, 3);
        assert_eq!(outcome.duplicates_skipped, 0);

        let text = read_journal(root).unwrap();
        let stamps = text.matches("## ").count();
        assert_eq!(stamps, 1, "expected a single timestamped section:\n{text}");
        assert!(text.contains("GOAL | Batch test"));
        assert!(text.contains("DEC | One section only"));
        assert!(text.contains("NEXT | Verify timestamp count"));
    }

    #[test]
    fn test_append_journal_skips_recent_duplicate_facts() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(root, "DEC | Use SQLite | notes/decisions.md").unwrap();
        let outcome = append_journal(
            root,
            "\
DEC | Use SQLite | notes/decisions.md
DEC | Brand new decision
",
        )
        .unwrap();
        assert_eq!(outcome.lines_written, 1);
        assert_eq!(outcome.duplicates_skipped, 1);

        let text = read_journal(root).unwrap();
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("Use SQLite"))
                .count(),
            1
        );
        assert!(text.contains("Brand new decision"));
    }

    #[test]
    fn test_split_summary_and_journal_facts() {
        let text = "\
Goal: Ship memory
Next: rebuild

FACTS
GOAL | Ship memory pipeline
DEC | Batched journal writes
NEXT | Rebuild LEAD
";
        let (summary, facts) = split_summary_and_journal_facts(text);
        assert!(summary.contains("Goal: Ship memory"));
        assert!(!summary.contains("FACTS"));
        let facts = facts.unwrap();
        assert!(facts.contains("GOAL | Ship memory pipeline"));
        assert!(facts.contains("DEC | Batched journal writes"));
        assert_eq!(facts.lines().count(), 3);
    }

    #[test]
    fn test_split_summary_ignores_mid_summary_facts_heading() {
        let text = "\
Goal: remember things

Facts:
the corpus has secret codes ALPHA and BRAVO that matter.
Next: quiz the agent

FACTS
GOAL | Pass context quiz
DEC | ALPHA-719 is Orion ship date
";
        let (summary, facts) = split_summary_and_journal_facts(text);
        assert!(summary.contains("Facts:"));
        assert!(summary.contains("secret codes ALPHA"));
        assert!(summary.contains("Next: quiz the agent"));
        assert!(!summary.contains("GOAL | Pass context quiz"));
        let facts = facts.unwrap();
        assert!(facts.contains("GOAL | Pass context quiz"));
        assert!(facts.contains("ALPHA-719"));
    }

    #[test]
    fn test_split_summary_keeps_full_text_when_only_facts_block() {
        let text = "\
FACTS
GOAL | Only facts no prose
";
        let (summary, facts) = split_summary_and_journal_facts(text);
        assert!(summary.contains("FACTS"));
        assert!(summary.contains("GOAL | Only facts no prose"));
        assert_eq!(facts, None);
    }

    #[test]
    fn test_split_summary_keeps_full_text_when_facts_unusable() {
        let text = "\
Goal: keep going

FACTS
none of this is tagged
";
        let (summary, facts) = split_summary_and_journal_facts(text);
        assert!(summary.contains("FACTS"));
        assert!(summary.contains("none of this is tagged"));
        assert_eq!(facts, None);
    }

    #[test]
    fn test_parse_journal_fact_extract_none() {
        assert_eq!(parse_journal_fact_extract_response("NONE"), None);
        assert_eq!(parse_journal_fact_extract_response("none\n"), None);
        assert_eq!(
            parse_journal_fact_extract_response("just some prose without tags"),
            None
        );
    }

    #[test]
    fn test_append_journal_reports_duplicates_when_nothing_written() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(root, "DEC | Use SQLite | notes/decisions.md").unwrap();
        let outcome = append_journal(root, "DEC | Use SQLite | notes/decisions.md").unwrap();
        assert_eq!(outcome.lines_written, 0);
        assert_eq!(outcome.duplicates_skipped, 1);
    }

    #[test]
    fn test_fact_dedupe_ignores_evidence_and_case_drift() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(root, "DEC | Use SQLite | notes/a.md").unwrap();
        let outcome = append_journal(root, "DEC | use   sqlite | notes/b.md").unwrap();
        assert_eq!(outcome.lines_written, 0);
        assert_eq!(outcome.duplicates_skipped, 1);
    }

    #[test]
    fn test_sync_project_summary_from_journal_upserts_headings() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(
            root,
            "\
GOAL | Ship memory pipeline
DEC | Prefer TAG|claim journal
NEXT | Rebuild LEAD
FAIL | Flaky extract on empty turns
",
        )
        .unwrap();

        let summary = read_project_summary(root).unwrap().unwrap();
        assert!(summary.contains("## Goal"));
        assert!(summary.contains("Ship memory pipeline"));
        assert!(summary.contains("## Decisions"));
        assert!(summary.contains("Prefer TAG|claim journal"));
        assert!(summary.contains("## Next"));
        assert!(summary.contains("Rebuild LEAD"));
        assert!(summary.contains("## State"));
        assert!(summary.contains("Flaky extract"));
    }

    #[test]
    fn test_sync_preserves_freeform_and_does_not_clear_missing_headings() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write_project_summary(
            root,
            "\
## Goal
Rich compaction goal narrative with lots of detail.

## Context
Keep this freeform handoff body.

## Next
Old next step
",
        )
        .unwrap();

        append_journal_with_options(
            root,
            "DEC | Only a decision\nNEXT | New next step",
            JournalAppendOptions {
                sync_summary: true,
            },
        )
        .unwrap();

        let summary = read_project_summary(root).unwrap().unwrap();
        assert!(summary.contains("Rich compaction goal narrative"));
        assert!(summary.contains("Keep this freeform handoff body"));
        assert!(summary.contains("New next step"));
        assert!(summary.contains("Only a decision"));
    }

    #[test]
    fn test_sync_skipped_when_append_disables_it() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write_project_summary(root, "## Goal\nCompaction authored\n").unwrap();
        append_journal_with_options(
            root,
            "GOAL | Should not rewrite summary\nDEC | Still journals",
            JournalAppendOptions {
                sync_summary: false,
            },
        )
        .unwrap();

        let summary = read_project_summary(root).unwrap().unwrap();
        assert_eq!(summary.trim(), "## Goal\nCompaction authored");
        assert!(read_journal(root).unwrap().contains("Should not rewrite summary"));
    }

    #[test]
    fn test_empty_journal_sync_is_noop() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        write_project_summary(root, "Leave me alone.").unwrap();
        assert!(!sync_project_summary_from_journal(root).unwrap());
        assert_eq!(
            read_project_summary(root).unwrap().as_deref(),
            Some("Leave me alone.")
        );
    }

    #[test]
    fn test_pitfalls_excludes_state_line() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        append_journal(
            root,
            "\
FAIL | First failure
BLOCK | Later blocker
",
        )
        .unwrap();
        let summary = read_project_summary(root).unwrap().unwrap();
        assert!(summary.contains("## State"));
        assert!(summary.contains("Later blocker"));
        // Only one FAIL/BLOCK besides State → Pitfalls gets First failure.
        assert!(summary.contains("## Pitfalls"));
        assert!(summary.contains("First failure"));
        let state_section = summary
            .split("## State")
            .nth(1)
            .unwrap()
            .split("## ")
            .next()
            .unwrap();
        assert!(!state_section.contains("First failure"));
    }
}
