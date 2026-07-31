use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use futures::AsyncReadExt as _;
use http_client::{
    AsyncBody, HttpClient, HttpClientWithUrl, HttpRequestExt as _, Method, RedirectPolicy, Request,
};
use thiserror::Error;

use crate::browser::{BrowserError, BrowserFetchRequest, fetch_rendered_html};
use crate::config::WebResearchConfig;
use crate::envelope::{content_sha256, render_web_envelope};
use crate::extract::{ExtractedPage, extract_page_content};
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
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for FetchError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value.to_string())
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
    http_client: Arc<HttpClientWithUrl>,
    config: WebResearchConfig,
    limiter: HostRateLimiter,
}

impl PageFetcher {
    pub fn new(http_client: Arc<HttpClientWithUrl>, config: WebResearchConfig) -> Self {
        let limiter = HostRateLimiter::new(config.per_host_delay);
        Self {
            http_client,
            config,
            limiter,
        }
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

        let mut current = canonicalize_url(&requested);
        let mut redirects = 0u32;
        let (status, content_type, body, final_url) = loop {
            if cancel
                .as_ref()
                .is_some_and(|flag| flag.load(std::sync::atomic::Ordering::SeqCst))
            {
                return Err(FetchError::Other("cancelled".into()));
            }

            let host = current
                .host_str()
                .ok_or_else(|| FetchError::Other("URL missing host".into()))?;
            self.limiter.wait_turn_blocking(host);

            ensure_public_http_url(&current)
                .map_err(|error| FetchError::SsrfBlocked(error.to_string()))?;

            let request = Request::builder()
                .method(Method::GET)
                .uri(current.as_str())
                .header(
                    "Accept",
                    "text/html,application/xhtml+xml,application/json,text/plain;q=0.9,*/*;q=0.8",
                )
                .header("Accept-Language", "en-US,en;q=0.8")
                .follow_redirects(RedirectPolicy::NoFollow)
                .body(AsyncBody::default())
                .map_err(|error| FetchError::Other(error.to_string()))?;

            let mut response = self
                .http_client
                .send(request)
                .await
                .map_err(|error| FetchError::Other(error.to_string()))?;

            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok())
                .map(|value| value.to_string());

            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| FetchError::Other("redirect missing Location".into()))?;
                let next = current
                    .join(location)
                    .map_err(|error| FetchError::Other(error.to_string()))?;
                redirects += 1;
                if redirects > self.config.max_redirects {
                    return Err(FetchError::Other(format!(
                        "exceeded max redirects ({})",
                        self.config.max_redirects
                    )));
                }
                ensure_public_http_url(&next).map_err(|error| {
                    FetchError::SsrfBlocked(format!("redirect to blocked URL: {error}"))
                })?;
                current = canonicalize_url(&next);
                continue;
            }

            let mut body = Vec::new();
            response
                .body_mut()
                .take(self.config.max_response_bytes + 1)
                .read_to_end(&mut body)
                .await
                .context("error reading response body")?;
            if body.len() as u64 > self.config.max_response_bytes {
                body.truncate(self.config.max_response_bytes as usize);
            }
            break (status, content_type, body, current.clone());
        };

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
                return Ok(FetchOutcome::Fetched(FetchedPage {
                    requested_url: requested.to_string(),
                    final_url: final_url.to_string(),
                    source_id,
                    retrieved_at_unix_ms,
                    http_status: status,
                    content_type,
                    fetch_mode: FetchMode::Http,
                    content_sha256: hash,
                    extracted,
                }));
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
            &normalize_url_string(navigate_url).map_err(|error| FetchError::SsrfBlocked(error.to_string()))?,
        )
        .map_err(|error| FetchError::SsrfBlocked(error.to_string()))?;

        let host = url::Url::parse(navigate_url)
            .ok()
            .and_then(|url| url.host_str().map(|host| host.to_string()))
            .unwrap_or_else(|| "browser".into());
        self.limiter.wait_turn_blocking(&host);

        let request = BrowserFetchRequest {
            url: navigate_url.to_string(),
            profile_dir,
            download_dir,
            timeout: self.config.browser_timeout,
            cancel,
        };

        let browser_result = {
            let request = request.clone();
            // Blocking process wait; keep HTTP event loops responsive.
            smol::unblock(move || fetch_rendered_html(&request)).await
        }?;

        if browser_result.html.len() as u64 > self.config.max_response_bytes {
            return Err(FetchError::Other(format!(
                "browser response exceeded max_response_bytes ({})",
                self.config.max_response_bytes
            )));
        }

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
}
