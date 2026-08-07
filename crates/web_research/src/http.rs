//! Shared hop-safe HTTP fetch used by page fetch, SERP discovery, and Unleashed
//! `http_request`. Every redirect hop is re-validated with [`ensure_public_http_url`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;
use futures::AsyncReadExt as _;
use futures::{FutureExt as _, select_biased};
use http_client::{
    AsyncBody, HttpClient, HttpClientWithUrl, HttpRequestExt as _, Method, RedirectPolicy, Request,
};
use thiserror::Error;
use url::Url;

use crate::rate_limit::HostRateLimiter;
use crate::url_policy::{canonicalize_url, ensure_public_http_url};

/// Default Accept for research page / SERP fetches (no `*/*`).
pub const RESEARCH_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/json,text/plain;q=0.9";

#[derive(Debug, Error)]
pub enum SafeHttpError {
    #[error("ssrf_blocked: {0}")]
    SsrfBlocked(String),
    #[error("timeout")]
    Timeout,
    #[error("cancelled")]
    Cancelled,
    #[error("response exceeded max_response_bytes ({0})")]
    ResponseTooLarge(u64),
    #[error("{0}")]
    Other(String),
}

impl From<anyhow::Error> for SafeHttpError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value.to_string())
    }
}

#[derive(Clone)]
pub struct SafeHttpOptions {
    pub method: Method,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub max_redirects: u32,
    pub max_response_bytes: u64,
    pub request_timeout: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
    pub limiter: Option<HostRateLimiter>,
}

impl Default for SafeHttpOptions {
    fn default() -> Self {
        Self {
            method: Method::GET,
            headers: vec![
                ("Accept".into(), RESEARCH_ACCEPT.into()),
                ("Accept-Language".into(), "en-US,en;q=0.8".into()),
            ],
            body: None,
            max_redirects: 5,
            max_response_bytes: 1024 * 1024,
            request_timeout: Duration::from_millis(15_000),
            cancel: None,
            limiter: None,
        }
    }
}

#[derive(Debug)]
pub struct SafeHttpResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub final_url: Url,
}

/// GET with hop-safe redirects, SSRF re-check, timeout, and body cap.
pub async fn safe_http_get(
    http_client: &HttpClientWithUrl,
    url: &Url,
    options: SafeHttpOptions,
) -> Result<SafeHttpResponse, SafeHttpError> {
    let mut options = options;
    options.method = Method::GET;
    options.body = None;
    safe_http_request(http_client, url, options).await
}

/// Perform an HTTP request with manual redirect following and per-hop SSRF checks.
pub async fn safe_http_request(
    http_client: &HttpClientWithUrl,
    url: &Url,
    options: SafeHttpOptions,
) -> Result<SafeHttpResponse, SafeHttpError> {
    ensure_public_http_url(url).map_err(|error| SafeHttpError::SsrfBlocked(error.to_string()))?;

    let mut current = canonicalize_url(url);
    let mut redirects = 0u32;
    let mut method = options.method.clone();
    let mut body = options.body.clone();

    loop {
        if options
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            return Err(SafeHttpError::Cancelled);
        }

        ensure_public_http_url(&current)
            .map_err(|error| SafeHttpError::SsrfBlocked(error.to_string()))?;

        if let Some(limiter) = &options.limiter {
            if let Some(host) = current.host_str() {
                limiter.wait_turn(host).await;
            }
        }

        let mut builder = Request::builder()
            .method(method.clone())
            .uri(current.as_str())
            .follow_redirects(RedirectPolicy::NoFollow);
        for (name, value) in &options.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let request = builder
            .body(
                body.as_ref()
                    .map(|bytes| AsyncBody::from(bytes.clone()))
                    .unwrap_or_default(),
            )
            .map_err(|error| SafeHttpError::Other(error.to_string()))?;

        let mut response = send_with_timeout(http_client, request, options.request_timeout).await?;

        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.to_string());
        let response_headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("<binary>").to_string(),
                )
            })
            .collect();

        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| SafeHttpError::Other("redirect missing Location".into()))?;
            let next = current
                .join(location)
                .map_err(|error| SafeHttpError::Other(error.to_string()))?;
            redirects += 1;
            if redirects > options.max_redirects {
                return Err(SafeHttpError::Other(format!(
                    "exceeded max redirects ({})",
                    options.max_redirects
                )));
            }
            ensure_public_http_url(&next).map_err(|error| {
                SafeHttpError::SsrfBlocked(format!("redirect to blocked URL: {error}"))
            })?;
            let code = status.as_u16();
            // 303 always becomes GET; 301/302 typically become GET for non-GET methods.
            if code == 303
                || ((code == 301 || code == 302)
                    && method != Method::GET
                    && method != Method::HEAD)
            {
                method = Method::GET;
                body = None;
            }
            current = canonicalize_url(&next);
            continue;
        }

        let mut bytes = Vec::new();
        response
            .body_mut()
            .take(options.max_response_bytes + 1)
            .read_to_end(&mut bytes)
            .await
            .context("error reading response body")?;
        if bytes.len() as u64 > options.max_response_bytes {
            return Err(SafeHttpError::ResponseTooLarge(options.max_response_bytes));
        }

        return Ok(SafeHttpResponse {
            status: status.as_u16(),
            content_type,
            headers: response_headers,
            body: bytes,
            final_url: current,
        });
    }
}

async fn send_with_timeout(
    http_client: &HttpClientWithUrl,
    request: Request<AsyncBody>,
    timeout: Duration,
) -> Result<http_client::Response<AsyncBody>, SafeHttpError> {
    let send = http_client.send(request).fuse();
    let timer = smol::Timer::after(timeout).fuse();
    futures::pin_mut!(send, timer);
    select_biased! {
        result = send => result.map_err(|error| SafeHttpError::Other(error.to_string())),
        _ = timer => Err(SafeHttpError::Timeout),
    }
}

/// Resolve a redirect Location against a base URL and enforce public-http policy.
///
/// Useful for unit tests without standing up an HTTP stack.
pub fn validate_redirect_target(base: &Url, location: &str) -> Result<Url, SafeHttpError> {
    let next = base
        .join(location)
        .map_err(|error| SafeHttpError::Other(error.to_string()))?;
    ensure_public_http_url(&next)
        .map_err(|error| SafeHttpError::SsrfBlocked(format!("redirect to blocked URL: {error}")))?;
    Ok(canonicalize_url(&next))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_redirect_to_private_ip() {
        let base = Url::parse("https://example.com/start").unwrap();
        let err = validate_redirect_target(&base, "http://127.0.0.1/secret").unwrap_err();
        assert!(
            matches!(err, SafeHttpError::SsrfBlocked(_)),
            "expected ssrf_blocked, got {err}"
        );
    }

    #[test]
    fn blocks_redirect_to_metadata_host() {
        let base = Url::parse("https://example.com/start").unwrap();
        let err =
            validate_redirect_target(&base, "http://metadata.google.internal/").unwrap_err();
        assert!(matches!(err, SafeHttpError::SsrfBlocked(_)));
    }

    #[test]
    fn allows_redirect_to_public_https() {
        let base = Url::parse("https://example.com/start").unwrap();
        let next = validate_redirect_target(&base, "https://docs.rs/gpui").unwrap();
        assert_eq!(next.host_str(), Some("docs.rs"));
    }
}
