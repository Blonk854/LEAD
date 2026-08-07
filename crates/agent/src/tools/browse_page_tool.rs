use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
use web_research::{PageFetcher, disk_cache_dir};

use crate::{
    AgentTool, ToolCallEventStream, ToolInput, ToolPermissionContext, full_access_enabled,
};

/// Force-fetch a URL via the isolated system browser (Unleashed).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BrowsePageToolInput {
    /// Absolute http(s) URL to render in the isolated browser profile.
    pub url: String,
}

pub struct BrowsePageTool {
    http_client: Arc<HttpClientWithUrl>,
}

impl BrowsePageTool {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self { http_client }
    }
}

impl AgentTool for BrowsePageTool {
    type Input = BrowsePageToolInput;
    type Output = String;

    const NAME: &'static str = "browse_page";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Browse {}", MarkdownEscaped(&input.url)).into(),
            Err(_) => "Browse page".into(),
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
            let input = input.recv().await.map_err(|error| error.to_string())?;

            if !cx.update(|cx| full_access_enabled(cx)) {
                return Err(
                    "Unleashed is disabled. Enable it in Settings → AI → Unleashed to use browse_page."
                        .into(),
                );
            }

            let settings = cx.update(|cx| agent_settings::AgentSettings::get_global(cx).clone());
            if !settings.web_research.enabled {
                return Err("Web research is disabled (agent.web_research.enabled=false).".into());
            }
            if !settings.web_research.browser_fallback_enabled {
                return Err(
                    "Browser fallback is disabled (agent.web_research.browser_fallback_enabled=false)."
                        .into(),
                );
            }

            let authorize = cx.update(|cx| {
                event_stream.authorize(
                    format!(
                        "Browse {} in isolated browser",
                        MarkdownInlineCode(&input.url)
                    ),
                    ToolPermissionContext::new(Self::NAME, vec![input.url.clone()]),
                    cx,
                )
            });
            authorize.await.map_err(|error| error.to_string())?;

            event_stream.update_fields(
                acp::ToolCallUpdateFields::new().title(format!(
                    "Browsing {}…",
                    MarkdownEscaped(&truncate_for_title(&input.url, 48))
                )),
            );

            let mut config = web_research_config_from_settings(&settings.web_research);
            config.browser_fallback_enabled = true;
            // Force browser path even if HTTP would succeed.
            let url = input.url.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            let cancel_flag = cancel.clone();
            let browse = cx.background_spawn(async move {
                let fetcher = PageFetcher::new(http_client, config);
                fetcher
                    .fetch_via_browser(&url, &url, Some(cancel))
                    .await
                    .map(|page| page.model_output())
                    .map_err(|error| error.to_string())
            });

            let text = futures::select! {
                result = browse.fuse() => result?,
                _ = event_stream.cancelled_by_user().fuse() => {
                    cancel_flag.store(true, Ordering::SeqCst);
                    return Err("Browse cancelled by user".into());
                }
            };

            if text.trim().is_empty() {
                return Err("no textual content found".into());
            }
            Ok(text)
        })
    }
}

fn truncate_for_title(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn browse_page_is_registered() {
        assert!(
            crate::ALL_TOOL_NAMES.contains(&"browse_page"),
            "browse_page must be registered in tools!"
        );
    }
}

/// Shared settings → `WebResearchConfig`, including isolated browser dirs.
pub fn web_research_config_from_settings(
    research: &agent_settings::WebResearchSettings,
) -> web_research::WebResearchConfig {
    let (profile_dir, download_dir) = browser_data_dirs();
    web_research::WebResearchConfig {
        enabled: research.enabled,
        max_search_results: research.max_search_results,
        max_snippet_chars: research.max_snippet_chars,
        max_fetch_chars: research.max_fetch_chars,
        max_response_bytes: research.max_response_bytes,
        max_redirects: research.max_redirects,
        request_timeout: Duration::from_millis(research.request_timeout_ms),
        max_concurrency: research.max_concurrency,
        per_host_delay: Duration::from_millis(research.per_host_delay_ms),
        cache_ttl_serp: Duration::from_secs(research.cache_ttl_serp_secs),
        cache_ttl_page: Duration::from_secs(research.cache_ttl_page_secs),
        cache_dir: Some(disk_cache_dir(paths::data_dir())),
        browser_fallback_enabled: research.browser_fallback_enabled,
        browser_profile_dir: Some(profile_dir),
        browser_download_dir: Some(download_dir),
        browser_timeout: Duration::from_secs(research.browser_timeout_secs),
        max_pages: research.max_pages,
        max_depth: research.max_depth,
        research_wall_clock: Duration::from_secs(research.research_wall_clock_secs),
    }
}

pub fn browser_data_dirs() -> (PathBuf, PathBuf) {
    let root = paths::data_dir().join("web_research");
    (root.join("browser_profile"), root.join("download_quarantine"))
}
