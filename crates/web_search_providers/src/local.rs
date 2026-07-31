use std::sync::Arc;

use anyhow::{Context as _, Result, bail};
use cloud_llm_client::{WebSearchResponse, WebSearchResult};
use futures::AsyncReadExt as _;
use gpui::{App, AppContext as _, Task};
use http_client::{
    AsyncBody, HttpClient, HttpClientWithUrl, HttpRequestExt as _, Method, RedirectPolicy, Request,
};
use url::Url;
use web_research::{DiscoveryAdapter, DiscoveryHealth, DiscoveryOutcome};
use web_search::{WebSearchProvider, WebSearchProviderId};

use crate::local_html::{BingHtmlAdapter, DuckDuckGoHtmlAdapter, StartpageHtmlAdapter};

pub const LOCAL_WEB_SEARCH_PROVIDER_ID: &str = "local_html";

pub struct LocalHtmlWebSearchProvider {
    http_client: Arc<HttpClientWithUrl>,
    max_results: usize,
    max_snippet_chars: usize,
}

impl LocalHtmlWebSearchProvider {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self {
            http_client,
            max_results: 5,
            max_snippet_chars: 300,
        }
    }

    pub fn with_limits(mut self, max_results: usize, max_snippet_chars: usize) -> Self {
        self.max_results = max_results;
        self.max_snippet_chars = max_snippet_chars;
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
        cx.background_spawn(async move {
            search_html_engines(http_client, query, max_results, max_snippet_chars).await
        })
    }
}

async fn search_html_engines(
    http_client: Arc<HttpClientWithUrl>,
    query: String,
    max_results: usize,
    max_snippet_chars: usize,
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

    let mut last_detail = String::from("all discovery adapters failed");
    for (serp_url, adapter) in adapters {
        match fetch_serp_html(http_client.clone(), &serp_url).await {
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

async fn fetch_serp_html(http_client: Arc<HttpClientWithUrl>, url: &str) -> Result<String> {
    let parsed = Url::parse(url).context("invalid SERP URL")?;
    web_research::ensure_public_http_url(&parsed)
        .map_err(|error| anyhow::anyhow!("ssrf_blocked: {error}"))?;

    let request = Request::builder()
        .method(Method::GET)
        .uri(url)
        .header(
            "Accept",
            "text/html,application/xhtml+xml;q=0.9,*/*;q=0.8",
        )
        .header("Accept-Language", "en-US,en;q=0.8")
        .follow_redirects(RedirectPolicy::FollowLimit(5))
        .body(AsyncBody::default())
        .context("build SERP request")?;

    let mut response = http_client
        .send(request)
        .await
        .with_context(|| format!("SERP request failed for {url}"))?;
    let status = response.status();
    let mut body = Vec::new();
    response
        .body_mut()
        .take(1024 * 1024 + 1)
        .read_to_end(&mut body)
        .await
        .context("read SERP body")?;
    if !status.is_success() {
        bail!("SERP HTTP {}", status.as_u16());
    }
    // Re-check final URL host if the client followed redirects to a private IP literal in Location.
    // Full DNS revalidation of redirect chains for SERP is best-effort here; page fetch does hop checks.
    Ok(String::from_utf8_lossy(&body).into_owned())
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
