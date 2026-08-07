use std::sync::Arc;
use std::time::Duration;

use anyhow::{Result, bail};
use cloud_llm_client::{WebSearchResponse, WebSearchResult};
use gpui::{App, AppContext as _, Task};
use http_client::HttpClientWithUrl;
use web_research::{
    DiscoveryAdapter, DiscoveryHealth, DiscoveryOutcome, HostRateLimiter, RESEARCH_ACCEPT,
    SafeHttpOptions, canonicalize_url, safe_http_get,
};
use web_search::{WebSearchProvider, WebSearchProviderId};

use crate::local_html::{BingHtmlAdapter, DuckDuckGoHtmlAdapter, StartpageHtmlAdapter};

pub const LOCAL_WEB_SEARCH_PROVIDER_ID: &str = "local_html";

pub struct LocalHtmlWebSearchProvider {
    http_client: Arc<HttpClientWithUrl>,
    max_results: usize,
    max_snippet_chars: usize,
    max_redirects: u32,
    max_response_bytes: u64,
    request_timeout: Duration,
    per_host_delay: Duration,
}

impl LocalHtmlWebSearchProvider {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self {
            http_client,
            max_results: 5,
            max_snippet_chars: 300,
            max_redirects: 5,
            max_response_bytes: 1024 * 1024,
            request_timeout: Duration::from_millis(15_000),
            per_host_delay: Duration::from_millis(1_000),
        }
    }

    pub fn with_limits(mut self, max_results: usize, max_snippet_chars: usize) -> Self {
        self.max_results = max_results;
        self.max_snippet_chars = max_snippet_chars;
        self
    }

    pub fn with_http_policy(
        mut self,
        max_redirects: u32,
        max_response_bytes: u64,
        request_timeout: Duration,
        per_host_delay: Duration,
    ) -> Self {
        self.max_redirects = max_redirects;
        self.max_response_bytes = max_response_bytes;
        self.request_timeout = request_timeout;
        self.per_host_delay = per_host_delay;
        self
    }
}

impl WebSearchProvider for LocalHtmlWebSearchProvider {
    fn id(&self) -> WebSearchProviderId {
        WebSearchProviderId(LOCAL_WEB_SEARCH_PROVIDER_ID.into())
    }

    fn search(&self, query: String, cx: &mut App) -> Task<Result<WebSearchResponse>> {
        let http_client = self.http_client.clone();
        let max_results = self.max_results;
        let max_snippet_chars = self.max_snippet_chars;
        let max_redirects = self.max_redirects;
        let max_response_bytes = self.max_response_bytes;
        let request_timeout = self.request_timeout;
        let per_host_delay = self.per_host_delay;
        cx.background_spawn(async move {
            search_html_engines(
                http_client,
                query,
                max_results,
                max_snippet_chars,
                max_redirects,
                max_response_bytes,
                request_timeout,
                per_host_delay,
            )
            .await
        })
    }
}

async fn search_html_engines(
    http_client: Arc<HttpClientWithUrl>,
    query: String,
    max_results: usize,
    max_snippet_chars: usize,
    max_redirects: u32,
    max_response_bytes: u64,
    request_timeout: Duration,
    per_host_delay: Duration,
) -> Result<WebSearchResponse> {
    let adapters: Vec<(String, Box<dyn DiscoveryAdapter>)> = vec![
        (
            format!(
                "https://html.duckduckgo.com/html/?q={}",
                urlencoding_encode(&query)
            ),
            Box::new(DuckDuckGoHtmlAdapter { max_results }),
        ),
        (
            format!(
                "https://www.bing.com/search?q={}",
                urlencoding_encode(&query)
            ),
            Box::new(BingHtmlAdapter { max_results }),
        ),
        (
            format!(
                "https://www.startpage.com/sp/search?query={}",
                urlencoding_encode(&query)
            ),
            Box::new(StartpageHtmlAdapter { max_results }),
        ),
    ];

    let limiter = HostRateLimiter::process_shared(per_host_delay);
    let mut last_detail = String::from("all discovery adapters failed");
    for (serp_url, adapter) in adapters {
        match fetch_serp_html(
            http_client.clone(),
            &serp_url,
            max_redirects,
            max_response_bytes,
            request_timeout,
            limiter.clone(),
        )
        .await
        {
            Ok(html) => {
                let outcome = adapter.search(&query, &html);
                last_detail = format!(
                    "{}: {:?}{}",
                    outcome.adapter_id,
                    outcome.health,
                    outcome
                        .detail
                        .as_ref()
                        .map(|detail| format!(" ({detail})"))
                        .unwrap_or_default()
                );
                if outcome.health == DiscoveryHealth::Ok && !outcome.hits.is_empty() {
                    return Ok(to_web_search_response(outcome, max_results, max_snippet_chars));
                }
            }
            Err(error) => {
                last_detail = format!("fetch {serp_url} failed: {error}");
            }
        }
    }

    bail!("discovery_failed: {last_detail}")
}

async fn fetch_serp_html(
    http_client: Arc<HttpClientWithUrl>,
    url: &str,
    max_redirects: u32,
    max_response_bytes: u64,
    request_timeout: Duration,
    limiter: HostRateLimiter,
) -> Result<String> {
    let parsed = url::Url::parse(url)?;
    let options = SafeHttpOptions {
        method: http_client::Method::GET,
        headers: vec![
            ("Accept".into(), RESEARCH_ACCEPT.into()),
            ("Accept-Language".into(), "en-US,en;q=0.8".into()),
        ],
        body: None,
        max_redirects,
        max_response_bytes,
        request_timeout,
        cancel: None,
        limiter: Some(limiter),
    };
    let response = safe_http_get(&http_client, &parsed, options)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if !(200..300).contains(&response.status) {
        bail!("SERP HTTP {}", response.status);
    }
    let _ = canonicalize_url(&response.final_url);
    Ok(String::from_utf8_lossy(&response.body).into_owned())
}

fn to_web_search_response(
    outcome: DiscoveryOutcome,
    max_results: usize,
    max_snippet_chars: usize,
) -> WebSearchResponse {
    let results = outcome
        .hits
        .into_iter()
        .take(max_results)
        .map(|hit| WebSearchResult {
            title: hit.title,
            url: hit.url,
            text: truncate_chars(&hit.snippet, max_snippet_chars),
        })
        .collect();
    WebSearchResponse { results }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn urlencoding_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
