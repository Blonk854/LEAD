use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol::schema as acp;
use anyhow::Result;
use futures::FutureExt as _;
use gpui::{App, AppContext as _, Task};
use http_client::HttpClientWithUrl;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use ui::SharedString;
use util::markdown::{MarkdownEscaped, MarkdownInlineCode};
use web_research::{FetchError, FetchOutcome, PageFetcher};

use crate::{
    AgentTool, BrowsePageTool, ToolCallEventStream, ToolInput, ToolPermissionContext,
    full_access_enabled,
};
use super::browse_page_tool::web_research_config_from_settings;
use url::Url;

/// Fetches a URL and returns the content as Markdown inside an untrusted envelope.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchToolInput {
    /// The URL to fetch.
    url: String,
}

pub struct FetchTool {
    http_client: Arc<HttpClientWithUrl>,
}

impl FetchTool {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self { http_client }
    }
}

impl AgentTool for FetchTool {
    type Input = FetchToolInput;
    type Output = String;

    const NAME: &'static str = "fetch";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Fetch {}", MarkdownEscaped(&input.url)).into(),
            Err(_) => "Fetch URL".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let http_client = self.http_client.clone();
        cx.spawn(async move |cx| {
            let input: FetchToolInput = input.recv().await.map_err(|e| e.to_string())?;

            let authorize = cx.update(|cx| {
                let docsrs_alt = rewrite_docsrs_crate_overview(&input.url);
                let urls = match docsrs_alt {
                    Some(alt) => vec![input.url.clone(), alt],
                    None => vec![input.url.clone()],
                };
                let context = crate::ToolPermissionContext::new(Self::NAME, urls);

                event_stream.authorize(
                    format!("Fetch {}", MarkdownInlineCode(&input.url)),
                    context,
                    cx,
                )
            });

            let settings = cx.update(|cx| agent_settings::AgentSettings::get_global(cx).clone());
            let config = web_research_config_from_settings(&settings.web_research);
            // HTTP path never auto-enables browser; escalate is gated below.
            let mut http_config = config.clone();
            http_config.browser_fallback_enabled = false;

            let url = input.url.clone();
            let docsrs_alt = rewrite_docsrs_crate_overview(&url);
            let http_client_for_fetch = http_client.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_flag = cancel.clone();
            let fetch_task = cx.background_spawn(async move {
                authorize.await.map_err(|error| error.to_string())?;
                let fetcher = PageFetcher::new(http_client_for_fetch, http_config);

                let first = fetcher
                    .fetch_with_cancel(&url, Some(cancel.clone()))
                    .await;

                match first {
                    Ok(outcome) => Ok(outcome),
                    Err(FetchError::Other(message))
                        if docsrs_alt
                            .as_ref()
                            .is_some_and(|alt| alt != &url)
                            && message.contains("status error 400") =>
                    {
                        let alt = docsrs_alt.expect("checked above");
                        fetcher
                            .fetch_with_cancel(&alt, Some(cancel.clone()))
                            .await
                            .map_err(|error| error.to_string())
                    }
                    Err(error) => Err(error.to_string()),
                }
            });

            let outcome = futures::select! {
                result = fetch_task.fuse() => result?,
                _ = event_stream.cancelled_by_user().fuse() => {
                    cancel_flag.store(true, Ordering::SeqCst);
                    return Err("Fetch cancelled by user".to_string());
                }
            };

            let text = match outcome {
                FetchOutcome::Fetched(page) => page.model_output(),
                FetchOutcome::NeedsBrowser { reason, url } => {
                    if !settings.web_research.browser_fallback_enabled {
                        return Err(format!(
                            "needs_browser: {reason} (url={url}). Enable agent.web_research.browser_fallback_enabled and Unleashed to render with the isolated system browser."
                        ));
                    }
                    if !cx.update(|cx| full_access_enabled(cx)) {
                        return Err(format!(
                            "needs_browser: {reason} (url={url}). Unleashed is required for isolated browser fallback."
                        ));
                    }

                    let browse_authorize = cx.update(|cx| {
                        event_stream.authorize(
                            format!(
                                "Browse {} in isolated browser (HTTP extract failed)",
                                MarkdownInlineCode(&url)
                            ),
                            ToolPermissionContext::new(
                                BrowsePageTool::NAME,
                                vec![url.clone()],
                            ),
                            cx,
                        )
                    });
                    browse_authorize
                        .await
                        .map_err(|error| error.to_string())?;

                    event_stream.update_fields(
                        acp::ToolCallUpdateFields::new()
                            .title(format!("Browsing {}…", MarkdownEscaped(&url))),
                    );

                    let cancel = Arc::new(AtomicBool::new(false));
                    let cancel_flag = cancel.clone();
                    let browse_url = url.clone();
                    let browse = cx.background_spawn({
                        let http_client = http_client.clone();
                        let mut config = config;
                        config.browser_fallback_enabled = true;
                        async move {
                            let fetcher = PageFetcher::new(http_client, config);
                            fetcher
                                .fetch_via_browser(&browse_url, &browse_url, Some(cancel))
                                .await
                                .map(|page| page.model_output())
                                .map_err(|error| error.to_string())
                        }
                    });

                    futures::select! {
                        result = browse.fuse() => result?,
                        _ = event_stream.cancelled_by_user().fuse() => {
                            cancel_flag.store(true, Ordering::SeqCst);
                            return Err("Fetch cancelled by user".to_string());
                        }
                    }
                }
            };

            if text.trim().is_empty() {
                return Err("no textual content found".to_string());
            }
            Ok(text)
        })
    }
}

fn rewrite_docsrs_crate_overview(url: &str) -> Option<String> {
    // Common invalid docs.rs pattern some agents guess:
    //   https://docs.rs/crate/<crate>/overview
    // which often yields HTTP 400. A better target is:
    //   https://docs.rs/<crate>/latest/<crate>/
    let trimmed = url.trim();
    let with_scheme = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    };

    let parsed = Url::parse(&with_scheme).ok()?;
    if parsed.host_str()? != "docs.rs" {
        return None;
    }
    let mut segments = parsed.path_segments()?.filter(|s| !s.is_empty());
    let first = segments.next()?;
    let crate_name = segments.next()?;
    let third = segments.next()?;
    // No more segments (so we match exactly `/crate/<crate>/overview`)
    if segments.next().is_some() {
        return None;
    }
    if first == "crate" && third == "overview" {
        Some(format!(
            "https://docs.rs/{}/latest/{}/",
            crate_name, crate_name
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_docsrs_crate_overview() {
        assert_eq!(
            rewrite_docsrs_crate_overview("https://docs.rs/crate/gpui/overview"),
            Some("https://docs.rs/gpui/latest/gpui/".into())
        );
        assert_eq!(
            rewrite_docsrs_crate_overview("docs.rs/crate/serde/overview"),
            Some("https://docs.rs/serde/latest/serde/".into())
        );
    }

    #[test]
    fn leaves_valid_docsrs_urls_alone() {
        assert_eq!(
            rewrite_docsrs_crate_overview("https://docs.rs/gpui/latest/gpui/"),
            None
        );
        assert_eq!(
            rewrite_docsrs_crate_overview("https://docs.rs/crate/gpui/latest"),
            None
        );
        assert_eq!(
            rewrite_docsrs_crate_overview("https://example.com/crate/gpui/overview"),
            None
        );
    }
}
