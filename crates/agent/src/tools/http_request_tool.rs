use std::{collections::HashMap, sync::Arc, time::Duration};

use agent_client_protocol::schema as acp;
use futures::FutureExt as _;
use gpui::{App, AppContext as _, Task};
use http_client::{HttpClientWithUrl, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::SharedString;
use web_research::{SafeHttpOptions, ensure_public_http_url, safe_http_request};

use crate::{AgentTool, ToolCallEventStream, ToolInput, ToolPermissionContext};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const MAX_REDIRECTS: u32 = 5;
const REQUEST_TIMEOUT: Duration = Duration::from_millis(15_000);

/// Sends an HTTP request and returns its status, headers, and response body.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct HttpRequestToolInput {
    /// HTTP method: GET, POST, PUT, PATCH, or DELETE.
    pub method: String,
    /// Absolute HTTP or HTTPS URL.
    pub url: String,
    /// Optional request headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Optional UTF-8 request body.
    pub body: Option<String>,
}

pub struct HttpRequestTool {
    http_client: Arc<HttpClientWithUrl>,
}

impl HttpRequestTool {
    pub fn new(http_client: Arc<HttpClientWithUrl>) -> Self {
        Self { http_client }
    }
}

impl AgentTool for HttpRequestTool {
    type Input = HttpRequestToolInput;
    type Output = String;

    const NAME: &'static str = "http_request";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Fetch
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("{} {}", input.method.to_ascii_uppercase(), input.url).into(),
            Err(_) => "Send HTTP request".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let client = self.http_client.clone();
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| error.to_string())?;
            if !cx.update(|cx| crate::full_access_enabled(cx)) {
                return Err(
                    "Unleashed is disabled. Enable it in Settings → AI → Unleashed (or set agent.full_access.enabled).".into(),
                );
            }

            let method = input.method.to_ascii_uppercase();
            let method = match method.as_str() {
                "GET" => Method::GET,
                "POST" => Method::POST,
                "PUT" => Method::PUT,
                "PATCH" => Method::PATCH,
                "DELETE" => Method::DELETE,
                _ => {
                    return Err(
                        "Unsupported HTTP method. Use GET, POST, PUT, PATCH, or DELETE.".into(),
                    );
                }
            };
            let url = url::Url::parse(&input.url).map_err(|error| error.to_string())?;
            ensure_public_http_url(&url).map_err(|error| format!("ssrf_blocked: {error}"))?;

            if method != Method::GET {
                let authorize = cx.update(|cx| {
                    event_stream.authorize(
                        format!("{method} {url}"),
                        ToolPermissionContext::new(Self::NAME, vec![url.to_string()]),
                        cx,
                    )
                });
                authorize.await.map_err(|error| error.to_string())?;
            }

            let headers: Vec<(String, String)> = input.headers.into_iter().collect();
            let body = input.body.map(|value| value.into_bytes());
            let request_task = cx.background_spawn(async move {
                let options = SafeHttpOptions {
                    method,
                    headers,
                    body,
                    max_redirects: MAX_REDIRECTS,
                    max_response_bytes: MAX_RESPONSE_BYTES,
                    request_timeout: REQUEST_TIMEOUT,
                    cancel: None,
                    limiter: None,
                };
                let response = safe_http_request(&client, &url, options)
                    .await
                    .map_err(|error| error.to_string())?;
                let headers = response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let body = String::from_utf8_lossy(&response.body);
                Ok::<_, String>(format!(
                    "HTTP {}\n{}\n\n{}",
                    response.status, headers, body
                ))
            });

            futures::select! {
                result = request_task.fuse() => result,
                _ = event_stream.cancelled_by_user().fuse() => {
                    Err("HTTP request cancelled by user.".into())
                }
            }
        })
    }
}
