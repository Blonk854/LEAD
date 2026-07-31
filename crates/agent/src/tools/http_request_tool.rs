use std::{collections::HashMap, sync::Arc};

use agent_client_protocol::schema as acp;
use futures::{AsyncReadExt as _, FutureExt as _};
use gpui::{App, AppContext as _, Task};
use http_client::{AsyncBody, HttpClient as _, HttpClientWithUrl, Request};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::SharedString;

use crate::{AgentTool, ToolCallEventStream, ToolInput, ToolPermissionContext};

const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;

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
            if !matches!(method.as_str(), "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
                return Err(
                    "Unsupported HTTP method. Use GET, POST, PUT, PATCH, or DELETE.".into(),
                );
            }
            let url = url::Url::parse(&input.url).map_err(|error| error.to_string())?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err("Only http:// and https:// URLs are supported.".into());
            }

            if method != "GET" {
                let authorize = cx.update(|cx| {
                    event_stream.authorize(
                        format!("{method} {url}"),
                        ToolPermissionContext::new(Self::NAME, vec![url.to_string()]),
                        cx,
                    )
                });
                authorize.await.map_err(|error| error.to_string())?;
            }

            let mut builder = Request::builder().method(method.as_str()).uri(url.as_str());
            for (name, value) in input.headers {
                builder = builder.header(name, value);
            }
            let request = builder
                .body(input.body.map(AsyncBody::from).unwrap_or_default())
                .map_err(|error| error.to_string())?;

            let request_task = cx.background_spawn(async move {
                let mut response = client
                    .send(request)
                    .await
                    .map_err(|error| error.to_string())?;
                let status = response.status();
                let headers = response
                    .headers()
                    .iter()
                    .map(|(name, value)| {
                        format!("{}: {}", name, value.to_str().unwrap_or("<binary>"))
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut body = Vec::new();
                response
                    .body_mut()
                    .take(MAX_RESPONSE_BYTES + 1)
                    .read_to_end(&mut body)
                    .await
                    .map_err(|error| error.to_string())?;
                let truncated = body.len() as u64 > MAX_RESPONSE_BYTES;
                body.truncate(MAX_RESPONSE_BYTES as usize);
                let body = String::from_utf8_lossy(&body);
                Ok::<_, String>(format!(
                    "HTTP {}\n{}\n\n{}{}",
                    status.as_u16(),
                    headers,
                    body,
                    if truncated {
                        "\n\n[response truncated at 1 MiB]"
                    } else {
                        ""
                    }
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
