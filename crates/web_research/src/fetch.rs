use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::browser::{BrowserError, BrowserFetchRequest, fetch_rendered_html};
use crate::cache::{CachedPage, DiskCache};
use crate::config::WebResearchConfig;
use crate::envelope::{content_sha256, render_web_envelope};
use crate::extract::{ExtractedPage, extract_page_content, heading_outline};
use crate::http::{RESEARCH_ACCEPT, SafeHttpError, SafeHttpOptions, safe_http_get};
use crate::rate_limit::HostRateLimiter;
use crate::url_policy::{canonicalize_url, ensure_public_http_url, normalize_url_string};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FetchMode {
    Http,
    Browser,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("{0}")]
    Policy(#[from] crate::url_policy::UrlPolicyError),
    #[error("ssrf_blocked: {0}")]
    SsrfBlocked(String),
    #[error("needs_browser: {0}")]
    NeedsBrowser(String),
    #[error("browser_unavailable: {0}")]
    BrowserUnavailable(String),
    #[error("timeout")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for FetchError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value.to_string())
    }
}

impl From<SafeHttpError> for FetchError {
    fn from(value: SafeHttpError) -> Self {
        match value {
            SafeHttpError::SsrfBlocked(message) => Self::SsrfBlocked(message),
            SafeHttpError::Timeout => Self::Timeout,
            SafeHttpError::Cancelled => Self::Other("cancelled".into()),
            SafeHttpError::ResponseTooLarge(limit) => Self::Other(format!(
                "response exceeded max_response_bytes ({limit})"
            )),
            SafeHttpError::Other(message) => Self::Other(message),
        }
    }
}

impl From<BrowserError> for FetchError {
    fn from(value: BrowserError) -> Self {
        match value {
            BrowserError::BrowserNotFound => {
                Self::BrowserUnavailable("no system Chrome/Edge binary found".into())
            }
            BrowserError::Cancelled => Self::Other("browser_cancelled".into()),
            BrowserError::Timeout => Self::Other("browser_timeout".into()),
            BrowserError::Unavailable(message) | BrowserError::Failed(message) => {
                Self::BrowserUnavailable(message)
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchedPage {
    pub requested_url: String,
    pub final_url: String,
    pub source_id: String,
    pub retrieved_at_unix_ms: u64,
    pub http_status: u16,
    pub content_type: Option<String>,
    pub fetch_mode: FetchMode,
    pub content_sha256: String,
    pub extracted: ExtractedPage,
}

impl FetchedPage {
    pub fn model_output(&self) -> String {
        let mut body = String::new();
        if !self.extracted.outline.is_empty() {
            body.push_str("## Outline\n");
            body.push_str(&self.extracted.outline);
            body.push_str("\n\n");
        }
        body.push_str(&self.extracted.markdown);
        render_web_envelope(
            &self.final_url,
            &self.source_id,
            self.extracted.title.as_deref(),
            match self.fetch_mode {
                FetchMode::Http => "http",
                FetchMode::Browser => "browser",
            },
            self.extracted.truncated,
            &body,
        )
    }
}

#[derive(Debug)]
pub enum FetchOutcome {
    Fetched(FetchedPage),
    NeedsBrowser { reason: String, url: String },
}

pub struct PageFetcher {
    http_client: Arc<http_client::HttpClientWithUrl>,
    config: WebResearchConfig,
    limiter: HostRateLimiter,
    cache: Option<DiskCache>,
}

impl PageFetcher {
    pub fn new(
        http_client: Arc<http_client::HttpClientWithUrl>,
        config: WebResearchConfig,
    ) -> Self {
        let limiter = HostRateLimiter::process_shared(config.per_host_delay);
        let cache = config
            .cache_dir
            .as_ref()
            .and_then(|dir| DiskCache::open(dir.clone()).ok());
        Self {
            http_client,
            config,
            limiter,
            cache,
        }
    }

    pub fn limiter(&self) -> &HostRateLimiter {
        &self.limiter
    }

    pub async fn fetch(&self, raw_url: &str) -> Result<FetchOutcome, FetchError> {
        self.fetch_with_cancel(raw_url, None).await
    }

    pub async fn fetch_with_cancel(
        &self,
        raw_url: &str,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<FetchOutcome, FetchError> {
        let requested = normalize_url_string(raw_url)?;
        ensure_public_http_url(&requested)
            .map_err(|error| FetchError::SsrfBlocked(error.to_string()))?;

        let canonical = canonicalize_url(&requested);
        if let Some(cache) = &self.cache {
            if let Ok(Some(cached)) = cache.get(canonical.as_str(), self.config.cache_ttl_page) {
                return Ok(FetchOutcome::Fetched(page_from_cache(
                    requested.as_str(),
                    &cached,
                )));
            }
        }

        let options = SafeHttpOptions {
            method: http_client::Method::GET,
            headers: vec![
                ("Accept".into(), RESEARCH_ACCEPT.into()),
                ("Accept-Language".into(), "en-US,en;q=0.8".into()),
            ],
            body: None,
            max_redirects: self.config.max_redirects,
            max_response_bytes: self.config.max_response_bytes,
            request_timeout: self.config.request_timeout,
            cancel: cancel.clone(),
            limiter: Some(self.limiter.clone()),
        };

        let response = safe_http_get(&self.http_client, &requested, options).await?;
        let status = response.status;
        let content_type = response.content_type;
        let body = response.body;
        let final_url = response.final_url;

        if !(200..300).contains(&status) {
            let preview = String::from_utf8_lossy(&body);
            let preview = preview.chars().take(400).collect::<String>();
            if let Some(host) = final_url.host_str() {
                self.limiter.penalize(host);
            }
            return Err(FetchError::Other(format!(
                "status error {status}, response: {preview:?}"
            )));
        }

        let needs_browser_reason = if looks_like_browser_shell(content_type.as_deref(), &body) {
            if let Some(host) = final_url.host_str() {
                self.limiter.penalize(host);
            }
            Some("page looks like a JS shell or challenge page".to_string())
        } else {
            let extracted = extract_page_content(
                final_url.as_str(),
                content_type.as_deref(),
                &body,
                self.config.max_fetch_chars,
            )?;
            if extracted.markdown.trim().len() < 40 && body.len() > 1_000 {
                if let Some(host) = final_url.host_str() {
                    self.limiter.penalize(host);
                }
                Some("extracted text was too thin relative to response size".to_string())
            } else {
                if let Some(host) = final_url.host_str() {
                    self.limiter.reward(host);
                }
                let hash = content_sha256(&body);
                let source_id = format!("web:{}", &hash[..12.min(hash.len())]);
                let retrieved_at_unix_ms = now_ms();
                let page = FetchedPage {
                    requested_url: requested.to_string(),
                    final_url: final_url.to_string(),
                    source_id,
                    retrieved_at_unix_ms,
                    http_status: status,
                    content_type: content_type.clone(),
                    fetch_mode: FetchMode::Http,
                    content_sha256: hash.clone(),
                    extracted: extracted.clone(),
                };
                if let Some(cache) = &self.cache {
                    let _ = cache.put(&CachedPage {
                        url: canonicalize_url(&final_url).to_string(),
                        stored_at_unix_ms: retrieved_at_unix_ms,
                        content_type,
                        markdown: extracted.markdown,
                        content_sha256: hash,
                    });
                }
                return Ok(FetchOutcome::Fetched(page));
            }
        };

        let reason = needs_browser_reason.expect("set above");
        if self.config.browser_fallback_enabled {
            match self
                .fetch_via_browser(requested.as_str(), final_url.as_str(), cancel.clone())
                .await
            {
                Ok(page) => return Ok(FetchOutcome::Fetched(page)),
                Err(FetchError::BrowserUnavailable(message)) => {
                    return Ok(FetchOutcome::NeedsBrowser {
                        reason: format!("{reason}; browser_unavailable: {message}"),
                        url: final_url.to_string(),
                    });
                }
                Err(error) => return Err(error),
            }
        }

        Ok(FetchOutcome::NeedsBrowser {
            reason,
            url: final_url.to_string(),
        })
    }

    /// Force isolated browser render (used by `browse_page` and explicit escalate).
    pub async fn fetch_via_browser(
        &self,
        requested_url: &str,
        navigate_url: &str,
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<FetchedPage, FetchError> {
        let profile_dir = self.config.browser_profile_dir.clone().ok_or_else(|| {
            FetchError::BrowserUnavailable("browser_profile_dir is not configured".into())
        })?;
        let download_dir = self.config.browser_download_dir.clone().ok_or_else(|| {
            FetchError::BrowserUnavailable("browser_download_dir is not configured".into())
        })?;

        ensure_public_http_url(
            &normalize_url_string(navigate_url)
                .map_err(|error| FetchError::SsrfBlocked(error.to_string()))?,
        )
        .map_err(|error| FetchError::SsrfBlocked(error.to_string()))?;

        let host = url::Url::parse(navigate_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.to_string()))
            .unwrap_or_else(|| "browser".into());
        self.limiter.wait_turn(&host).await;

        let request = BrowserFetchRequest {
            url: navigate_url.to_string(),
            profile_dir,
            download_dir,
            timeout: self.config.browser_timeout,
            max_response_bytes: self.config.max_response_bytes,
            cancel,
        };

        let browser_result = {
            let request = request.clone();
            // Blocking process wait; keep HTTP event loops responsive.
            smol::unblock(move || fetch_rendered_html(&request)).await
        }?;

        let extracted = extract_page_content(
            &browser_result.final_url,
            Some("text/html"),
            &browser_result.html,
            self.config.max_fetch_chars,
        )?;
        let hash = content_sha256(&browser_result.html);
        let source_id = format!("web:{}", &hash[..12.min(hash.len())]);

        if let Ok(url) = url::Url::parse(&browser_result.final_url) {
            if let Some(host) = url.host_str() {
                self.limiter.reward(host);
            }
        }

        Ok(FetchedPage {
            requested_url: requested_url.to_string(),
            final_url: browser_result.final_url,
            source_id,
            retrieved_at_unix_ms: now_ms(),
            http_status: 200,
            content_type: Some("text/html".into()),
            fetch_mode: FetchMode::Browser,
            content_sha256: hash,
            extracted,
        })
    }
}

fn page_from_cache(requested_url: &str, cached: &CachedPage) -> FetchedPage {
    let outline = heading_outline(&cached.markdown, 24);
    let title = outline.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#').trim().to_string())
            .filter(|title| !title.is_empty())
    });
    let source_id = format!(
        "web:{}",
        &cached.content_sha256[..12.min(cached.content_sha256.len())]
    );
    FetchedPage {
        requested_url: requested_url.to_string(),
        final_url: cached.url.clone(),
        source_id,
        retrieved_at_unix_ms: cached.stored_at_unix_ms,
        http_status: 200,
        content_type: cached.content_type.clone(),
        fetch_mode: FetchMode::Http,
        content_sha256: cached.content_sha256.clone(),
        extracted: ExtractedPage {
            title,
            markdown: cached.markdown.clone(),
            outline,
            truncated: false,
        },
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn looks_like_browser_shell(content_type: Option<&str>, body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if text.contains("cf-browser-verification")
        || text.contains("attention required! | cloudflare")
        || (text.contains("enable javascript") && text.contains("<noscript") && body.len() < 8_000)
    {
        return true;
    }
    let is_html = content_type
        .map(|value| value.to_ascii_lowercase().contains("html"))
        .unwrap_or(true);
    if is_html && body.len() < 1_500 {
        let has_root = text.contains("id=\"root\"")
            || text.contains("id=\"app\"")
            || text.contains("data-reactroot");
        if has_root {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_js_shell() {
        let html = br#"<!doctype html><html><body><div id="root"></div><script src="/app.js"></script></body></html>"#;
        assert!(looks_like_browser_shell(Some("text/html"), html));
    }

    #[test]
    fn shared_limiters_across_fetchers() {
        let a = HostRateLimiter::process_shared(std::time::Duration::from_millis(10));
        let b = HostRateLimiter::process_shared(std::time::Duration::from_millis(20));
        assert!(a.shares_map_with(&b));
    }
}
