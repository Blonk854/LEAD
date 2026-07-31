use std::sync::Arc;

use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema as acp;
use anyhow::Result;
use cloud_llm_client::WebSearchResponse;
use futures::FutureExt as _;
use gpui::{App, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::prelude::*;
use util::markdown::MarkdownInlineCode;
use web_research::render_search_results_for_model;
use web_search::WebSearchRegistry;

/// Search the web for information using your query.
/// Use this when you need real-time information, facts, or data that might not be in your training.
/// Results include short snippets and links — fetch a page before asserting facts from snippets alone.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WebSearchToolInput {
    /// The search term or question to query on the web.
    query: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebSearchToolSuccess {
    pub query: String,
    pub provider: String,
    pub response: WebSearchResponse,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WebSearchToolOutput {
    Success(WebSearchToolSuccess),
    /// Legacy shape from older transcripts.
    LegacySuccess(WebSearchResponse),
    Error { error: String },
}

impl From<WebSearchToolOutput> for LanguageModelToolResultContent {
    fn from(value: WebSearchToolOutput) -> Self {
        match value {
            WebSearchToolOutput::Success(success) => {
                let results: Vec<(String, String, String)> = success
                    .response
                    .results
                    .iter()
                    .map(|result| {
                        (
                            result.title.clone(),
                            result.url.clone(),
                            result.text.clone(),
                        )
                    })
                    .collect();
                render_search_results_for_model(&success.provider, &success.query, &results).into()
            }
            WebSearchToolOutput::LegacySuccess(response) => {
                let results: Vec<(String, String, String)> = response
                    .results
                    .iter()
                    .map(|result| {
                        (
                            result.title.clone(),
                            result.url.clone(),
                            result.text.clone(),
                        )
                    })
                    .collect();
                render_search_results_for_model("web", "", &results).into()
            }
            WebSearchToolOutput::Error { error } => error.into(),
        }
    }
}

pub struct WebSearchTool;

impl AgentTool for WebSearchTool {
    type Input = WebSearchToolInput;
    type Output = WebSearchToolOutput;

    const NAME: &'static str = "search_web";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Searching the Web".into()
    }

    /// Available whenever a web search provider is registered (local HTML or cloud).
    fn supports_provider(_provider: &language_model::LanguageModelProviderId) -> bool {
        true
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|e| WebSearchToolOutput::Error {
                    error: e.to_string(),
                })?;

            let authorize = cx.update(|cx| {
                let context =
                    crate::ToolPermissionContext::new(Self::NAME, vec![input.query.clone()]);
                event_stream.authorize(
                    format!("Search the web for {}", MarkdownInlineCode(&input.query)),
                    context,
                    cx,
                )
            });
            authorize
                .await
                .map_err(|e| WebSearchToolOutput::Error { error: e.to_string() })?;

            let prepared = cx.update(|cx| {
                let Some(provider) = WebSearchRegistry::read_global(cx).active_provider() else {
                    return Err(WebSearchToolOutput::Error {
                        error: "Web search is not available.".to_string(),
                    });
                };
                Ok((
                    provider.id().0.to_string(),
                    provider.search(input.query.clone(), cx),
                ))
            })?;
            let (provider_id, search_task) = prepared;

            let response = futures::select! {
                result = search_task.fuse() => {
                    match result {
                        Ok(response) => response,
                        Err(err) => {
                            event_stream
                                .update_fields(acp::ToolCallUpdateFields::new().title("Web Search Failed"));
                            return Err(WebSearchToolOutput::Error { error: err.to_string() });
                        }
                    }
                }
                _ = event_stream.cancelled_by_user().fuse() => {
                    return Err(WebSearchToolOutput::Error { error: "Web search cancelled by user".to_string() });
                }
            };

            emit_update(&response, &event_stream);
            Ok(WebSearchToolOutput::Success(WebSearchToolSuccess {
                query: input.query,
                provider: provider_id,
                response,
            }))
        })
    }

    fn replay(
        &self,
        _input: Self::Input,
        output: Self::Output,
        event_stream: ToolCallEventStream,
        _cx: &mut App,
    ) -> Result<()> {
        match &output {
            WebSearchToolOutput::Success(success) => emit_update(&success.response, &event_stream),
            WebSearchToolOutput::LegacySuccess(response) => emit_update(response, &event_stream),
            WebSearchToolOutput::Error { .. } => {}
        }
        Ok(())
    }
}

fn emit_update(response: &WebSearchResponse, event_stream: &ToolCallEventStream) {
    let result_text = if response.results.len() == 1 {
        "1 result".to_string()
    } else {
        format!("{} results", response.results.len())
    };
    event_stream.update_fields(
        acp::ToolCallUpdateFields::new()
            .title(format!("Searched the web: {result_text}"))
            .content(
                response
                    .results
                    .iter()
                    .map(|result| {
                        acp::ToolCallContent::Content(acp::Content::new(
                            acp::ContentBlock::ResourceLink(
                                acp::ResourceLink::new(result.title.clone(), result.url.clone())
                                    .title(result.title.clone())
                                    .description(result.text.clone()),
                            ),
                        ))
                    })
                    .collect::<Vec<_>>(),
            ),
    );
}
