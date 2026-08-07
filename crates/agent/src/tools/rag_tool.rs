use crate::project_memory::primary_worktree_root;
use crate::rag::{DEFAULT_EMBEDDING_MODEL, RagIndexCache, default_top_k, embeddings_api_base};
use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema as acp;
use gpui::{App, Entity, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use language_models::AllLanguageModelSettings;
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

/// Indexes a file, directory, or previously persisted research session into the local document store.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RagIngestToolInput {
    /// File or directory path to ingest (absolute or relative to the project).
    pub path: Option<String>,
    /// Persist research session id from `research_web` — ingests pages as `source=web:…`.
    pub research_session_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RagIngestToolOutput {
    Success { message: String },
    Error { error: String },
}

impl From<RagIngestToolOutput> for LanguageModelToolResultContent {
    fn from(output: RagIngestToolOutput) -> Self {
        match output {
            RagIngestToolOutput::Success { message } => message.into(),
            RagIngestToolOutput::Error { error } => error.into(),
        }
    }
}

/// Searches the local document index for passages relevant to a query.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct RagSearchToolInput {
    /// What to search for.
    pub query: String,
    /// Number of results to return.
    #[serde(default)]
    pub k: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RagSearchToolOutput {
    Success { content: String },
    Error { error: String },
}

impl From<RagSearchToolOutput> for LanguageModelToolResultContent {
    fn from(output: RagSearchToolOutput) -> Self {
        match output {
            RagSearchToolOutput::Success { content } => content.into(),
            RagSearchToolOutput::Error { error } => error.into(),
        }
    }
}

pub(crate) static RAG_INDEX_CACHE: LazyLock<RagIndexCache> = LazyLock::new(RagIndexCache::new);

pub(crate) fn embedding_settings_for_rag(cx: &App) -> anyhow::Result<(String, String)> {
    embedding_settings(cx)
}

pub struct RagIngestTool {
    project: Entity<Project>,
}

impl RagIngestTool {
    pub fn new(project: Entity<Project>) -> Self {
        Self { project }
    }
}

impl AgentTool for RagIngestTool {
    type Input = RagIngestToolInput;
    type Output = RagIngestToolOutput;

    const NAME: &'static str = "rag_ingest";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) if input.research_session_id.is_some() => {
                format!(
                    "Index research session {}",
                    input.research_session_id.as_deref().unwrap_or("")
                )
                .into()
            }
            Ok(input) => format!(
                "Index documents at {}",
                input.path.as_deref().unwrap_or("(unspecified)")
            )
            .into(),
            Err(_) => "Index documents".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let http_client = self.project.read(cx).client().http_client();
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| RagIngestToolOutput::Error {
                error: error.to_string(),
            })?;

            if let Some(session_id) = input.research_session_id.clone() {
                let authorize = cx.update(|cx| {
                    event_stream.authorize(
                        format!("Index research session {session_id} into RAG"),
                        crate::ToolPermissionContext::new(
                            Self::NAME,
                            vec![
                                format!("research_session_id={session_id}"),
                                "ingest_to_rag=1".into(),
                            ],
                        ),
                        cx,
                    )
                });
                authorize
                    .await
                    .map_err(|error| RagIngestToolOutput::Error {
                        error: error.to_string(),
                    })?;
                return ingest_research_session(&self.project, &session_id, cx).await;
            }

            let path = input.path.ok_or_else(|| RagIngestToolOutput::Error {
                error: "rag_ingest requires `path` or `research_session_id`.".into(),
            })?;

            let (worktree_root, resolved_path, api_base, embedding_model) =
                cx.update(|cx| rag_context(&self.project, &path, cx))
                    .map_err(|error| RagIngestToolOutput::Error {
                        error: error.to_string(),
                    })?;

            let index = RAG_INDEX_CACHE
                .for_worktree(&worktree_root)
                .map_err(|error| RagIngestToolOutput::Error {
                    error: format!("Failed to open RAG index: {error:#}"),
                })?;

            let (files, chunks) = index
                .ingest_path(
                    &resolved_path,
                    http_client,
                    &api_base,
                    &embedding_model,
                )
                .await
                .map_err(|error| RagIngestToolOutput::Error {
                    error: format!("Failed to ingest documents: {error:#}"),
                })?;

            if chunks == 0 {
                Ok(RagIngestToolOutput::Success {
                    message: format!("No indexable content found at {}.", resolved_path.display()),
                })
            } else {
                Ok(RagIngestToolOutput::Success {
                    message: format!(
                        "Indexed {chunks} chunk(s) from {files} file(s). The index now holds {} chunk(s).",
                        index.count().unwrap_or(chunks)
                    ),
                })
            }
        })
    }
}

async fn ingest_research_session(
    project: &Entity<Project>,
    session_id: &str,
    cx: &mut gpui::AsyncApp,
) -> Result<RagIngestToolOutput, RagIngestToolOutput> {
    let stored = web_research::load_session(paths::data_dir(), session_id).map_err(|error| {
        RagIngestToolOutput::Error {
            error: error.to_string(),
        }
    })?;
    if stored.pages.is_empty() {
        return Ok(RagIngestToolOutput::Success {
            message: format!(
                "Research session `{session_id}` has no fetched page bodies to ingest."
            ),
        });
    }

    let (worktree_root, api_base, embedding_model, http_client) = cx
        .update(|cx| {
            let worktree_root = primary_worktree(project, cx)?;
            let (api_base, embedding_model) = embedding_settings(cx)?;
            let http_client = project.read(cx).client().http_client();
            Ok::<_, anyhow::Error>((worktree_root, api_base, embedding_model, http_client))
        })
        .map_err(|error| RagIngestToolOutput::Error {
            error: error.to_string(),
        })?;

    let index = RAG_INDEX_CACHE
        .for_worktree(&worktree_root)
        .map_err(|error| RagIngestToolOutput::Error {
            error: format!("Failed to open RAG index: {error:#}"),
        })?;

    let mut total = 0usize;
    let mut sources = 0usize;
    for page in &stored.pages {
        if page.markdown.trim().is_empty() {
            continue;
        }
        let source = web_research::web_rag_source(&page.url, &page.source_id);
        let text = format!(
            "# {}\nSource: {}\nMode: {}\n\n{}",
            page.title, page.url, page.fetch_mode, page.markdown
        );
        let chunks = index
            .ingest_text(&source, &text, http_client.clone(), &api_base, &embedding_model)
            .await
            .map_err(|error| RagIngestToolOutput::Error {
                error: format!("Failed to ingest {source}: {error:#}"),
            })?;
        if chunks > 0 {
            sources += 1;
            total += chunks;
        }
    }

    Ok(RagIngestToolOutput::Success {
        message: format!(
            "Indexed {total} chunk(s) from {sources} web page(s) in session `{session_id}` as source=web:…. The index now holds {} chunk(s).",
            index.count().unwrap_or(total)
        ),
    })
}

pub struct RagSearchTool {
    project: Entity<Project>,
}

impl RagSearchTool {
    pub fn new(project: Entity<Project>) -> Self {
        Self { project }
    }
}

impl AgentTool for RagSearchTool {
    type Input = RagSearchToolInput;
    type Output = RagSearchToolOutput;

    const NAME: &'static str = "rag_search";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Read
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Search documents for {}", input.query).into(),
            Err(_) => "Search documents".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let http_client = self.project.read(cx).client().http_client();
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| RagSearchToolOutput::Error {
                    error: error.to_string(),
                })?;

            let (worktree_root, api_base, embedding_model) = cx
                .update(|cx| rag_search_context(&self.project, cx))
                .map_err(|error| RagSearchToolOutput::Error {
                    error: error.to_string(),
                })?;

            let index = RAG_INDEX_CACHE
                .for_worktree(&worktree_root)
                .map_err(|error| RagSearchToolOutput::Error {
                    error: format!("Failed to open RAG index: {error:#}"),
                })?;

            if index.count().unwrap_or(0) == 0 {
                return Ok(RagSearchToolOutput::Success {
                    content: "The document index is empty. Use rag_ingest to add documents first."
                        .into(),
                });
            }

            let k = input.k.unwrap_or_else(default_top_k);
            let hits = index
                .search(&input.query, k, http_client, &api_base, &embedding_model)
                .await
                .map_err(|error| RagSearchToolOutput::Error {
                    error: format!("Document search failed: {error:#}"),
                })?;

            if hits.is_empty() {
                return Ok(RagSearchToolOutput::Success {
                    content: "No relevant passages found.".into(),
                });
            }

            let content = hits
                .into_iter()
                .enumerate()
                .map(|(index, hit)| {
                    let name = PathBuf::from(&hit.source)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| hit.source.clone());
                    format!(
                        "[{}] {} (chunk {}, score {:.3})\n{}",
                        index + 1,
                        name,
                        hit.chunk_index,
                        hit.score,
                        hit.text
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n");

            Ok(RagSearchToolOutput::Success { content })
        })
    }
}

fn rag_context(
    project: &Entity<Project>,
    path: &str,
    cx: &mut App,
) -> anyhow::Result<(PathBuf, PathBuf, String, String)> {
    let worktree_root = primary_worktree(project, cx)?;
    let resolved = if PathBuf::from(path).is_absolute() {
        PathBuf::from(path)
    } else {
        worktree_root.join(path)
    };
    let (api_base, embedding_model) = embedding_settings(cx)?;
    Ok((worktree_root, resolved, api_base, embedding_model))
}

fn rag_search_context(
    project: &Entity<Project>,
    cx: &mut App,
) -> anyhow::Result<(PathBuf, String, String)> {
    let worktree_root = primary_worktree(project, cx)?;
    let (api_base, embedding_model) = embedding_settings(cx)?;
    Ok((worktree_root, api_base, embedding_model))
}

fn primary_worktree(project: &Entity<Project>, cx: &mut App) -> anyhow::Result<PathBuf> {
    primary_worktree_root(project, cx)
        .ok_or_else(|| anyhow::anyhow!("No project worktree is open."))
}

fn embedding_settings(cx: &App) -> anyhow::Result<(String, String)> {
    let settings = AllLanguageModelSettings::get_global(cx).lmstudio.clone();
    if settings.api_url.trim().is_empty() {
        anyhow::bail!(
            "RAG embeddings require LM Studio. Configure language_models.lmstudio.api_url and ensure LM Studio is running."
        );
    }
    Ok((
        embeddings_api_base(&settings.api_url),
        DEFAULT_EMBEDDING_MODEL.to_string(),
    ))
}
