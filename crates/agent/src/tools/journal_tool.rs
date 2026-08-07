use crate::project_memory::{append_journal, read_journal, worktree_roots};
use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema as acp;
use gpui::{App, Entity, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Appends a dated note to the project journal for durable, cross-session memory.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AppendToJournalToolInput {
    /// Durable note(s) to append as one timestamped journal section.
    /// Prefer a batch of 1–8 lines in the form `TAG | claim | evidence?`
    /// (tags: GOAL, DEC, PATH, API, FAIL, FIX, NEXT, URL, PREF, BLOCK).
    /// Multiple facts in one call are preferred over many separate calls.
    pub note: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AppendToJournalToolOutput {
    Success { message: String },
    Error { error: String },
}

impl From<AppendToJournalToolOutput> for LanguageModelToolResultContent {
    fn from(output: AppendToJournalToolOutput) -> Self {
        match output {
            AppendToJournalToolOutput::Success { message } => message.into(),
            AppendToJournalToolOutput::Error { error } => error.into(),
        }
    }
}

pub struct AppendToJournalTool {
    project: Entity<Project>,
}

impl AppendToJournalTool {
    pub fn new(project: Entity<Project>) -> Self {
        Self { project }
    }
}

impl AgentTool for AppendToJournalTool {
    type Input = AppendToJournalToolInput;
    type Output = AppendToJournalToolOutput;

    const NAME: &'static str = "append_to_journal";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Think
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Append to project journal".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| AppendToJournalToolOutput::Error {
                    error: error.to_string(),
                })?;

            let note = input.note.trim();
            if note.is_empty() {
                return Err(AppendToJournalToolOutput::Error {
                    error: "Refusing to write an empty journal note.".into(),
                });
            }

            let roots = cx.update(|cx| worktree_roots(&self.project, cx));
            if roots.is_empty() {
                return Err(AppendToJournalToolOutput::Error {
                    error: "No directory worktree is open (file-only tabs cannot host the project journal).".into(),
                });
            }

            let mut total_written = 0usize;
            let mut total_dupes = 0usize;
            for root in &roots {
                let outcome =
                    append_journal(root, note).map_err(|error| AppendToJournalToolOutput::Error {
                        error: format!("Failed to append journal note: {error:#}"),
                    })?;
                total_written = total_written.saturating_add(outcome.lines_written);
                total_dupes = total_dupes.saturating_add(outcome.duplicates_skipped);
            }

            if total_written == 0 {
                if total_dupes > 0 {
                    return Ok(AppendToJournalToolOutput::Success {
                        message: format!(
                            "No new journal facts saved ({total_dupes} duplicate(s) skipped)."
                        ),
                    });
                }
                return Ok(AppendToJournalToolOutput::Success {
                    message: "Nothing new to save to the project journal.".into(),
                });
            }

            let dupe_note = if total_dupes > 0 {
                format!(" ({total_dupes} duplicate(s) skipped)")
            } else {
                String::new()
            };
            Ok(AppendToJournalToolOutput::Success {
                message: format!(
                    "Saved {total_written} journal line(s) in one section{dupe_note}."
                ),
            })
        })
    }
}

/// Reads the full project journal of durable notes from previous and current sessions.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadJournalToolInput {}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReadJournalToolOutput {
    Success { content: String },
    Error { error: String },
}

impl From<ReadJournalToolOutput> for LanguageModelToolResultContent {
    fn from(output: ReadJournalToolOutput) -> Self {
        match output {
            ReadJournalToolOutput::Success { content } => content.into(),
            ReadJournalToolOutput::Error { error } => error.into(),
        }
    }
}

pub struct ReadJournalTool {
    project: Entity<Project>,
}

impl ReadJournalTool {
    pub fn new(project: Entity<Project>) -> Self {
        Self { project }
    }
}

impl AgentTool for ReadJournalTool {
    type Input = ReadJournalToolInput;
    type Output = ReadJournalToolOutput;

    const NAME: &'static str = "read_journal";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Read project journal".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let _input = input
                .recv()
                .await
                .map_err(|error| ReadJournalToolOutput::Error {
                    error: error.to_string(),
                })?;

            let roots = cx.update(|cx| worktree_roots(&self.project, cx));
            if roots.is_empty() {
                return Err(ReadJournalToolOutput::Error {
                    error: "No directory worktree is open (file-only tabs cannot host the project journal).".into(),
                });
            }

            let mut sections = Vec::new();
            for root in roots {
                let journal =
                    read_journal(&root).map_err(|error| ReadJournalToolOutput::Error {
                        error: format!("Failed to read journal: {error:#}"),
                    })?;
                if journal.trim().is_empty() {
                    continue;
                }
                sections.push(format!("## {}\n{}", root.display(), journal.trim()));
            }

            if sections.is_empty() {
                Ok(ReadJournalToolOutput::Success {
                    content: "The project journal is empty.".into(),
                })
            } else {
                Ok(ReadJournalToolOutput::Success {
                    content: sections.join("\n\n"),
                })
            }
        })
    }
}
