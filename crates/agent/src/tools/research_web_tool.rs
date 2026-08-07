use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use agent_client_protocol::schema as acp;
use anyhow::Result;
use futures::FutureExt as _;
use gpui::{App, Entity, Task};
use http_client::HttpClientWithUrl;
use language_model::LanguageModelToolResultContent;
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use ui::SharedString;
use util::markdown::MarkdownInlineCode;
use web_research::{
    PageFetcher, ResearchBudgets, ResearchDigest, ResearchPageBody, ResearchSeed, ResearchSession,
    StoredResearchSession, list_sessions, load_session, new_session_id, render_session_list,
    save_session, web_rag_source,
};
use web_search::WebSearchRegistry;

use crate::project_memory::primary_worktree_root;
use crate::{AgentTool, ToolCallEventStream, ToolInput, ToolPermissionContext, full_access_enabled};
use super::browse_page_tool::web_research_config_from_settings;
use super::deserialize_optional_stringly_u64;
use super::deserialize_optional_stringly_usize;
use super::rag_tool::{RAG_INDEX_CACHE, embedding_settings_for_rag};

/// Run a bounded multi-page web research session and return a structured digest.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ResearchWebToolInput {
    /// Research question or search query. Ignored when `list_sessions` is true.
    #[serde(default)]
    pub query: String,
    /// Optional host allowlist (e.g. `docs.rs`, `example.com`). Defaults to hosts from search hits.
    /// Same-host/subdomain follows stay inside this list without re-confirm; cross-domain expansion is not automatic.
    #[serde(default)]
    pub domains: Vec<String>,
    /// Maximum pages to fetch (default from settings, typically 6).
    #[serde(default, deserialize_with = "deserialize_optional_stringly_usize")]
    pub max_pages: Option<usize>,
    /// Maximum link-follow depth from each SERP seed (default 1).
    #[serde(default, deserialize_with = "deserialize_optional_stringly_usize")]
    pub max_depth: Option<usize>,
    /// Wall-clock budget in seconds (default from settings, typically 90).
    #[serde(default, deserialize_with = "deserialize_optional_stringly_u64")]
    pub time_budget_secs: Option<u64>,
    /// Resume a previously persisted session id (returns stored digest; no network).
    pub session_id: Option<String>,
    /// List recent persisted research sessions instead of running discovery.
    #[serde(default)]
    pub list_sessions: bool,
    /// Persist this run to app-data for later resume (default true).
    pub persist: Option<bool>,
    /// Explicitly ingest fetched page text into the project RAG index as `source=web:…`.
    /// Never automatic — must be set true by the model/user.
    #[serde(default)]
    pub ingest_to_rag: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResearchWebToolOutput {
    Success(ResearchDigest),
    List { content: String },
    Error { error: String },
}

impl From<ResearchWebToolOutput> for LanguageModelToolResultContent {
    fn from(value: ResearchWebToolOutput) -> Self {
        match value {
            ResearchWebToolOutput::Success(digest) => digest.render_for_model().into(),
            ResearchWebToolOutput::List { content } => content.into(),
            ResearchWebToolOutput::Error { error } => error.into(),
        }
    }
}

pub struct ResearchWebTool {
    http_client: Arc<HttpClientWithUrl>,
    project: Entity<Project>,
}

impl ResearchWebTool {
    pub fn new(http_client: Arc<HttpClientWithUrl>, project: Entity<Project>) -> Self {
        Self {
            http_client,
            project,
        }
    }
}

impl AgentTool for ResearchWebTool {
    type Input = ResearchWebToolInput;
    type Output = ResearchWebToolOutput;

    const NAME: &'static str = "research_web";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) if input.list_sessions => "List research sessions".into(),
            Ok(input) if input.session_id.is_some() => {
                format!(
                    "Resume research {}",
                    MarkdownInlineCode(input.session_id.as_deref().unwrap_or(""))
                )
                .into()
            }
            Ok(input) if !input.query.is_empty() => {
                format!("Research: {}", MarkdownInlineCode(&input.query)).into()
            }
            _ => "Research the web".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let http_client = self.http_client.clone();
        let project = self.project.clone();
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| ResearchWebToolOutput::Error {
                    error: error.to_string(),
                })?;

            let settings = cx.update(|cx| agent_settings::AgentSettings::get_global(cx).clone());
            if !settings.web_research.enabled {
                return Err(ResearchWebToolOutput::Error {
                    error: "Web research is disabled (agent.web_research.enabled=false).".into(),
                });
            }

            let data_dir = paths::data_dir().clone();

            if input.list_sessions {
                let summaries = list_sessions(&data_dir, 20).map_err(|error| {
                    ResearchWebToolOutput::Error {
                        error: error.to_string(),
                    }
                })?;
                return Ok(ResearchWebToolOutput::List {
                    content: render_session_list(&summaries),
                });
            }

            if let Some(session_id) = input.session_id.clone() {
                let mut stored = load_session(&data_dir, &session_id).map_err(|error| {
                    ResearchWebToolOutput::Error {
                        error: error.to_string(),
                    }
                })?;
                stored.digest.session_id = Some(stored.id.clone());

                let mut note = String::new();
                if input.ingest_to_rag {
                    let mut auth_values = research_auth_values(
                        &stored.digest.query,
                        &stored.digest.domain_allowlist,
                        true,
                    );
                    auth_values.push(format!("session_id={}", stored.id));
                    let authorize = cx.update(|cx| {
                        event_stream.authorize(
                            format!(
                                "Ingest research session {} into RAG ({} page(s))",
                                MarkdownInlineCode(&stored.id),
                                stored.pages.len()
                            ),
                            ToolPermissionContext::new(Self::NAME, auth_values),
                            cx,
                        )
                    });
                    authorize
                        .await
                        .map_err(|error| ResearchWebToolOutput::Error {
                            error: error.to_string(),
                        })?;
                    note = ingest_pages_to_rag(&project, &stored.pages, cx).await?;
                }

                event_stream.update_fields(
                    acp::ToolCallUpdateFields::new().title(format!(
                        "Resumed research session {}",
                        MarkdownInlineCode(&stored.id)
                    )),
                );
                emit_digest_update(&stored.digest, &event_stream);
                let mut digest = stored.digest;
                if !note.is_empty() {
                    digest.stopped_reason =
                        format!("{} | {note}", digest.stopped_reason);
                }
                return Ok(ResearchWebToolOutput::Success(digest));
            }

            if input.query.trim().is_empty() {
                return Err(ResearchWebToolOutput::Error {
                    error: "query is required unless session_id or list_sessions is set".into(),
                });
            }

            if settings.web_research.browser_fallback_enabled
                && !cx.update(|cx| full_access_enabled(cx))
            {
                return Err(ResearchWebToolOutput::Error {
                    error: "Browser fallback is enabled but Unleashed is disabled. Enable Unleashed or set agent.web_research.browser_fallback_enabled=false.".into(),
                });
            }

            let max_pages = input
                .max_pages
                .unwrap_or(settings.web_research.max_pages)
                .clamp(1, 12);
            let max_depth = input
                .max_depth
                .unwrap_or(settings.web_research.max_depth)
                .clamp(0, 3);
            let time_budget_secs = input
                .time_budget_secs
                .unwrap_or(settings.web_research.research_wall_clock_secs)
                .clamp(15, 300);
            let persist = input.persist.unwrap_or(true);

            event_stream.update_fields(
                acp::ToolCallUpdateFields::new().title(format!(
                    "Discovering: {}…",
                    truncate_for_title(&input.query, 48)
                )),
            );

            let search_task = cx.update(|cx| {
                let Some(registry) = WebSearchRegistry::try_read_global(cx) else {
                    return Err(ResearchWebToolOutput::Error {
                        error: "Web search is not initialized.".into(),
                    });
                };
                let Some(provider) = registry.active_provider() else {
                    return Err(ResearchWebToolOutput::Error {
                        error: "Web search is not available.".into(),
                    });
                };
                Ok(provider.search(input.query.clone(), cx))
            })?;

            let search_response = futures::select! {
                result = search_task.fuse() => {
                    result.map_err(|error| ResearchWebToolOutput::Error {
                        error: format!("discovery_failed: {error}"),
                    })?
                }
                _ = event_stream.cancelled_by_user().fuse() => {
                    return Err(ResearchWebToolOutput::Error {
                        error: "Research cancelled by user".into(),
                    });
                }
            };

            let seeds: Vec<ResearchSeed> = search_response
                .results
                .into_iter()
                .take(settings.web_research.max_search_results)
                .map(|result| ResearchSeed {
                    title: result.title,
                    url: result.url,
                    snippet: result.text,
                })
                .collect();

            if seeds.is_empty() {
                return Err(ResearchWebToolOutput::Error {
                    error: "discovery_failed: no search hits".into(),
                });
            }

            let mut domains = input.domains.clone();
            if domains.is_empty() {
                domains = ResearchSession::allowlist_from_seeds(&seeds);
            }
            domains.retain(|domain| {
                !domain.parse::<std::net::IpAddr>().is_ok()
                    && !domain.contains(':')
                    && domain.contains('.')
            });
            if domains.is_empty() {
                return Err(ResearchWebToolOutput::Error {
                    error: "No usable domain allowlist (SERP hosts empty or all blocked).".into(),
                });
            }

            // Authorize after SERP so the prompt shows concrete domains.
            // Budgets/flags live in the title only so Always-allow domain patterns work.
            let auth_values =
                research_auth_values(&input.query, &domains, input.ingest_to_rag);
            let authorize = cx.update(|cx| {
                let browser_note = if settings.web_research.browser_fallback_enabled {
                    "; may use isolated browser"
                } else {
                    ""
                };
                let ingest_note = if input.ingest_to_rag {
                    "; will ingest to RAG"
                } else {
                    ""
                };
                let domain_preview = domains
                    .iter()
                    .take(6)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                event_stream.authorize(
                    format!(
                        "Research web for {} on [{domain_preview}] (≤{max_pages} pages, depth {max_depth}, {time_budget_secs}s{browser_note}{ingest_note})",
                        MarkdownInlineCode(&input.query)
                    ),
                    ToolPermissionContext::new(Self::NAME, auth_values),
                    cx,
                )
            });
            authorize
                .await
                .map_err(|error| ResearchWebToolOutput::Error {
                    error: format!("research_denied_after_discovery: {error}"),
                })?;

            event_stream.update_fields(
                acp::ToolCallUpdateFields::new()
                    .title(format!(
                        "Researching {} host(s), ≤{max_pages} pages…",
                        domains.len()
                    ))
                    .content(
                        seeds
                            .iter()
                            .map(|seed| {
                                acp::ToolCallContent::Content(acp::Content::new(
                                    acp::ContentBlock::ResourceLink(
                                        acp::ResourceLink::new(seed.title.clone(), seed.url.clone())
                                            .title(seed.title.clone())
                                            .description(seed.snippet.clone()),
                                    ),
                                ))
                            })
                            .collect::<Vec<_>>(),
                    ),
            );

            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_flag = cancel.clone();

            let mut config = web_research_config_from_settings(&settings.web_research);
            config.max_pages = max_pages;
            config.max_depth = max_depth;
            config.research_wall_clock = std::time::Duration::from_secs(time_budget_secs);

            let budgets = ResearchBudgets {
                max_pages,
                max_depth,
                max_download_bytes: settings.web_research.max_response_bytes.saturating_mul(4),
                wall_clock: std::time::Duration::from_secs(time_budget_secs),
            };

            let query = input.query.clone();
            let event_stream_for_progress = event_stream.clone();
            let research = async move {
                let fetcher = PageFetcher::new(http_client, config.clone());
                let session =
                    ResearchSession::new(query, domains, budgets, config, cancel.clone());
                session
                    .run(seeds, &fetcher, |progress| {
                        event_stream_for_progress.update_fields(
                            acp::ToolCallUpdateFields::new().title(progress),
                        );
                    })
                    .await
            };

            let outcome = futures::select! {
                outcome = research.fuse() => outcome,
                _ = event_stream.cancelled_by_user().fuse() => {
                    cancel_flag.store(true, Ordering::SeqCst);
                    return Err(ResearchWebToolOutput::Error {
                        error: "Research cancelled by user".into(),
                    });
                }
            };

            let mut digest = outcome.digest;
            let pages = outcome.pages;
            let mut ingest_note = String::new();

            if persist {
                let id = new_session_id(&digest.query);
                let now = now_ms();
                digest.session_id = Some(id.clone());
                let stored = StoredResearchSession {
                    id: id.clone(),
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    digest: digest.clone(),
                    pages: pages.clone(),
                };
                if let Err(error) = save_session(&data_dir, &stored) {
                    ingest_note = format!("persist_failed: {error}");
                }
            }

            if input.ingest_to_rag {
                match ingest_pages_to_rag(&project, &pages, cx).await {
                    Ok(note) => {
                        if ingest_note.is_empty() {
                            ingest_note = note;
                        } else {
                            ingest_note = format!("{ingest_note}; {note}");
                        }
                    }
                    Err(error) => return Err(error),
                }
            }

            if !ingest_note.is_empty() {
                digest.stopped_reason = format!("{} | {ingest_note}", digest.stopped_reason);
            }

            emit_digest_update(&digest, &event_stream);
            Ok(ResearchWebToolOutput::Success(digest))
        })
    }

    fn replay(
        &self,
        _input: Self::Input,
        output: Self::Output,
        event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        if let ResearchWebToolOutput::Success(digest) = &output {
            emit_digest_update(digest, &event_stream);
        }
        Ok(())
    }
}

async fn ingest_pages_to_rag(
    project: &Entity<Project>,
    pages: &[ResearchPageBody],
    cx: &mut gpui::AsyncApp,
) -> Result<String, ResearchWebToolOutput> {
    if pages.is_empty() {
        return Ok("rag_ingest: no fetched pages".into());
    }

    let (worktree_root, api_base, embedding_model, http_client) = cx
        .update(|cx| {
            let worktree_root = primary_worktree_root(project, cx).ok_or_else(|| {
                anyhow::anyhow!("No project worktree is open for RAG ingest.")
            })?;
            let (api_base, embedding_model) = embedding_settings_for_rag(cx)?;
            let http_client = project.read(cx).client().http_client();
            Ok::<_, anyhow::Error>((worktree_root, api_base, embedding_model, http_client))
        })
        .map_err(|error| ResearchWebToolOutput::Error {
            error: error.to_string(),
        })?;

    let index = RAG_INDEX_CACHE
        .for_worktree(&worktree_root)
        .map_err(|error| ResearchWebToolOutput::Error {
            error: format!("Failed to open RAG index: {error:#}"),
        })?;

    let mut total = 0usize;
    let mut sources = 0usize;
    for page in pages {
        if page.markdown.trim().is_empty() {
            continue;
        }
        let source = web_rag_source(&page.url, &page.source_id);
        let header = format!(
            "# {}\nSource: {}\nMode: {}\n\n",
            page.title, page.url, page.fetch_mode
        );
        let text = format!("{header}{}", page.markdown);
        let chunks = index
            .ingest_text(&source, &text, http_client.clone(), &api_base, &embedding_model)
            .await
            .map_err(|error| ResearchWebToolOutput::Error {
                error: format!("Failed to ingest {source}: {error:#}"),
            })?;
        if chunks > 0 {
            sources += 1;
            total += chunks;
        }
    }

    Ok(format!(
        "rag_ingest: {total} chunk(s) from {sources} page(s) as source=web:…"
    ))
}

fn emit_digest_update(digest: &ResearchDigest, event_stream: &ToolCallEventStream) {
    event_stream.update_fields(
        acp::ToolCallUpdateFields::new()
            .title(format!(
                "Researched: {} page(s), {}",
                digest.pages_fetched, digest.stopped_reason
            ))
            .content(
                digest
                    .sources
                    .iter()
                    .map(|source| {
                        acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::ResourceLink(
                                acp::ResourceLink::new(source.title.clone(), source.url.clone())
                                    .title(format!("[{}] {}", source.index, source.title))
                                    .description(
                                        source
                                            .quotes
                                            .first()
                                            .cloned()
                                            .unwrap_or_else(|| source.source_id.clone()),
                                    ),
                            ),
                        ))
                    })
                    .collect::<Vec<_>>(),
            ),
    );
}

fn truncate_for_title(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Auth values for Always-allow matching: query + concrete domains (+ ingest flag).
/// Budgets stay in the authorize title only.
pub(crate) fn research_auth_values(
    query: &str,
    domains: &[String],
    ingest_to_rag: bool,
) -> Vec<String> {
    let mut values = vec![query.to_string()];
    values.extend(domains.iter().cloned());
    if ingest_to_rag {
        values.push("ingest_to_rag=1".into());
    }
    values
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::research_auth_values;

    #[test]
    fn research_web_is_registered() {
        assert!(
            crate::ALL_TOOL_NAMES.contains(&"research_web"),
            "research_web must be registered in tools!"
        );
    }

    #[test]
    fn auth_values_are_query_domains_and_optional_ingest() {
        let values = research_auth_values(
            "gpui docs",
            &["docs.rs".into(), "zed.dev".into()],
            true,
        );
        assert_eq!(
            values,
            vec![
                "gpui docs".to_string(),
                "docs.rs".into(),
                "zed.dev".into(),
                "ingest_to_rag=1".into(),
            ]
        );
        let without_ingest = research_auth_values("q", &["a.com".into()], false);
        assert!(!without_ingest.iter().any(|v| v.starts_with("max_pages=")));
        assert!(!without_ingest.iter().any(|v| v.contains("ingest_to_rag")));
    }
}
