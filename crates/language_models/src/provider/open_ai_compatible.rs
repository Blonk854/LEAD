use anyhow::Result;
use convert_case::{Case, Casing};
use credentials_provider::CredentialsProvider;
use futures::{FutureExt, StreamExt, future::BoxFuture};
use gpui::{AnyView, App, AsyncApp, Context, Entity, SharedString, Task, TaskExt, Window};
use http_client::{CustomHeaders, HttpClient};
use language_model::{
    ApiKeyState, AuthenticateError, EnvVar, IconOrSvg, LanguageModel, LanguageModelCompletionError,
    LanguageModelCompletionEvent, LanguageModelId, LanguageModelName, LanguageModelProvider,
    LanguageModelProviderId, LanguageModelProviderName, LanguageModelProviderState,
    LanguageModelRequest, LanguageModelToolChoice, LanguageModelToolSchemaFormat, RateLimiter,
};
use menu;
use open_ai::{
    ResponseStreamEvent,
    responses::{Request as ResponseRequest, StreamEvent as ResponsesStreamEvent, stream_response},
    stream_completion,
};
use settings::{Settings, SettingsStore};
use std::sync::Arc;
use ui::{ElevationIndex, Tooltip, prelude::*};
use ui_input::InputField;
use util::ResultExt;

use crate::provider::open_ai::{
    OpenAiEventMapper, OpenAiResponseEventMapper, into_open_ai, into_open_ai_response,
};
pub use settings::OpenAiCompatibleAvailableModel as AvailableModel;
pub use settings::OpenAiCompatibleModelCapabilities as ModelCapabilities;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct OpenAiCompatibleSettings {
    pub api_url: String,
    pub available_models: Vec<AvailableModel>,
    pub custom_headers: CustomHeaders,
}

/// OpenAI-compatible endpoints expect `{base}/v1/chat/completions`. LM Studio and
/// similar servers break if `/v1` is omitted from the configured base URL.
pub fn normalize_openai_compatible_api_url(api_url: &str) -> String {
    let mut trimmed = api_url.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return String::new();
    }

    // Users sometimes paste a full completions URL from API docs.
    for suffix in ["/chat/completions", "/completions", "/embeddings"] {
        if let Some(base) = trimmed.strip_suffix(suffix) {
            trimmed = base.trim_end_matches('/').to_string();
            break;
        }
    }

    if trimmed.ends_with("/v1") {
        trimmed
    } else {
        format!("{trimmed}/v1")
    }
}

/// Local OpenAI-compatible servers (LM Studio, Ollama, etc.) require tool
/// `parameters` to be a JSON object with a `properties` map.
pub fn normalize_local_openai_tool_parameters(schema: serde_json::Value) -> serde_json::Value {
    let serde_json::Value::Object(mut map) = schema else {
        return serde_json::json!({ "type": "object", "properties": {} });
    };

    if !map.contains_key("properties") {
        map.insert("properties".into(), serde_json::json!({}));
    }
    if !map.contains_key("type") {
        map.insert("type".into(), serde_json::json!("object"));
    }
    let mut value = serde_json::Value::Object(map);
    language_model::minify_tool_schema_for_local(&mut value);
    value
}

/// Adjusts a chat-completions payload for local OpenAI-compatible servers.
pub fn sanitize_local_openai_compatible_request(request: &mut open_ai::Request) {
    for tool in &mut request.tools {
        if let open_ai::ToolDefinition::Function { function } = tool
            && let Some(parameters) = function.parameters.take()
        {
            function.parameters = Some(normalize_local_openai_tool_parameters(parameters));
        }
    }

    // Ask for usage on the final stream chunk so the agent panel context meter
    // can update. Modern LM Studio accepts this; older/quirky backends that
    // reject it still get an estimated meter via Thread::latest_token_usage.
    request.stream_options = Some(open_ai::StreamOptions {
        include_usage: true,
    });
    if request.temperature == Some(1.0) {
        request.temperature = None;
    }
}

/// Reserved provider id for the Network Agent's OpenAI-compatible endpoint.
pub const NETWORK_AGENT_PROVIDER_ID: &str = "network-agent";

/// Placeholder bearer token for local OpenAI-compatible servers that do not
/// require authentication (LM Studio, Ollama, etc.).
pub const LOCAL_OPENAI_COMPATIBLE_PLACEHOLDER_API_KEY: &str = "network-agent";

fn normalize_settings(mut settings: OpenAiCompatibleSettings) -> OpenAiCompatibleSettings {
    settings.api_url = normalize_openai_compatible_api_url(&settings.api_url);
    settings
}

pub struct OpenAiCompatibleLanguageModelProvider {
    id: LanguageModelProviderId,
    name: LanguageModelProviderName,
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
}

pub struct State {
    id: Arc<str>,
    api_key_state: ApiKeyState,
    settings: OpenAiCompatibleSettings,
    credentials_provider: Arc<dyn CredentialsProvider>,
}

impl State {
    fn is_network_agent(&self) -> bool {
        self.id.as_ref() == NETWORK_AGENT_PROVIDER_ID
    }

    fn is_configured(&self) -> bool {
        !self.settings.api_url.is_empty() && !self.settings.available_models.is_empty()
    }

    fn is_authenticated(&self) -> bool {
        if self.is_network_agent() {
            return self.is_configured();
        }
        self.api_key_state.has_key()
    }

    fn api_key_for_request(&self) -> Option<Arc<str>> {
        if let Some(key) = self.api_key_state.key(&self.settings.api_url) {
            return Some(key);
        }
        // Local OpenAI-compatible servers (LM Studio, Ollama) do not require auth.
        // Always send a harmless placeholder so requests are never blocked on keychain races.
        if self.is_network_agent() {
            return Some(Arc::from(LOCAL_OPENAI_COMPATIBLE_PLACEHOLDER_API_KEY));
        }
        None
    }

    fn ensure_network_agent_api_key(&mut self) {
        if !self.is_network_agent() || self.settings.api_url.is_empty() {
            return;
        }
        if self.api_key_state.key(&self.settings.api_url).is_some() {
            return;
        }
        self.api_key_state.set_in_memory_key(
            SharedString::new(self.settings.api_url.as_str()),
            LOCAL_OPENAI_COMPATIBLE_PLACEHOLDER_API_KEY,
        );
    }

    fn set_api_key(&mut self, api_key: Option<String>, cx: &mut Context<Self>) -> Task<Result<()>> {
        let credentials_provider = self.credentials_provider.clone();
        let api_url = SharedString::new(self.settings.api_url.as_str());
        self.api_key_state.store(
            api_url,
            api_key,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        )
    }

    fn authenticate(&mut self, cx: &mut Context<Self>) -> Task<Result<(), AuthenticateError>> {
        if self.is_network_agent() {
            if !self.is_configured() {
                return Task::ready(Err(AuthenticateError::CredentialsNotFound));
            }
            let was_authenticated = self.is_authenticated();
            self.ensure_network_agent_api_key();
            let credentials_provider = self.credentials_provider.clone();
            let api_url = SharedString::new(self.settings.api_url.clone());
            let _task = self.api_key_state.load_if_needed(
                api_url,
                |this| &mut this.api_key_state,
                credentials_provider,
                cx,
            );
            if !was_authenticated {
                cx.notify();
            }
            return Task::ready(Ok(()));
        }

        let credentials_provider = self.credentials_provider.clone();
        let api_url = SharedString::new(self.settings.api_url.clone());
        self.api_key_state.load_if_needed(
            api_url,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        )
    }
}

impl OpenAiCompatibleLanguageModelProvider {
    pub fn new(
        id: Arc<str>,
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        fn resolve_settings<'a>(id: &'a str, cx: &'a App) -> Option<&'a OpenAiCompatibleSettings> {
            crate::AllLanguageModelSettings::get_global(cx)
                .openai_compatible
                .get(id)
        }

        let api_key_env_var_name = format!("{}_API_KEY", id).to_case(Case::UpperSnake).into();
        let state = cx.new(|cx| {
            cx.observe_global::<SettingsStore>(|this: &mut State, cx| {
                let Some(settings) = resolve_settings(&this.id, cx).cloned() else {
                    return;
                };
                let settings = normalize_settings(settings);
                if &this.settings != &settings {
                    let credentials_provider = this.credentials_provider.clone();
                    let api_url = SharedString::new(settings.api_url.as_str());
                    this.api_key_state.handle_url_change(
                        api_url,
                        |this| &mut this.api_key_state,
                        credentials_provider,
                        cx,
                    );
                    this.settings = settings;
                    this.ensure_network_agent_api_key();
                    cx.notify();
                }
            })
            .detach();
            let settings = resolve_settings(&id, cx)
                .cloned()
                .map(normalize_settings)
                .unwrap_or_default();
            let mut state = State {
                id: id.clone(),
                api_key_state: ApiKeyState::new(
                    SharedString::new(settings.api_url.as_str()),
                    EnvVar::new(api_key_env_var_name),
                ),
                settings,
                credentials_provider,
            };
            state.ensure_network_agent_api_key();
            state
        });

        let provider_name = if id.as_ref() == NETWORK_AGENT_PROVIDER_ID {
            LanguageModelProviderName::from("Network Agent".to_string())
        } else {
            LanguageModelProviderName::from(id.to_string())
        };

        Self {
            id: LanguageModelProviderId::from(id.to_string()),
            name: provider_name,
            http_client,
            state,
        }
    }

    fn create_language_model(&self, model: AvailableModel) -> Arc<dyn LanguageModel> {
        Arc::new(OpenAiCompatibleLanguageModel {
            id: LanguageModelId::from(model.name.clone()),
            provider_id: self.id.clone(),
            provider_name: self.name.clone(),
            model,
            state: self.state.clone(),
            http_client: self.http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }
}

impl LanguageModelProviderState for OpenAiCompatibleLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OpenAiCompatibleLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelProviderName {
        self.name.clone()
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAiCompat)
    }

    fn default_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        self.state
            .read(cx)
            .settings
            .available_models
            .first()
            .map(|model| self.create_language_model(model.clone()))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        None
    }

    fn provided_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        self.state
            .read(cx)
            .settings
            .available_models
            .iter()
            .map(|model| self.create_language_model(model.clone()))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state.update(cx, |state, cx| state.authenticate(cx))
    }

    fn configuration_view(
        &self,
        _target_agent: language_model::ConfigurationViewTargetAgent,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyView {
        cx.new(|cx| ConfigurationView::new(self.state.clone(), window, cx))
            .into()
    }

    fn reset_credentials(&self, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.set_api_key(None, cx))
    }
}

pub struct OpenAiCompatibleLanguageModel {
    id: LanguageModelId,
    provider_id: LanguageModelProviderId,
    provider_name: LanguageModelProviderName,
    model: AvailableModel,
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

impl OpenAiCompatibleLanguageModel {
    fn is_network_agent(&self) -> bool {
        self.provider_id.0.as_ref() == NETWORK_AGENT_PROVIDER_ID
    }
}

impl OpenAiCompatibleLanguageModel {
    fn stream_completion(
        &self,
        request: open_ai::Request,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<'static, Result<ResponseStreamEvent>>,
            LanguageModelCompletionError,
        >,
    > {
        let http_client = self.http_client.clone();

        let (api_key, api_url, extra_headers) = self.state.read_with(cx, |state, _cx| {
            (
                state.api_key_for_request(),
                state.settings.api_url.clone(),
                state.settings.custom_headers.clone(),
            )
        });

        let provider = self.provider_name.clone();
        let future = self.request_limiter.stream(async move {
            let Some(api_key) = api_key else {
                return Err(LanguageModelCompletionError::NoApiKey { provider });
            };
            let request = stream_completion(
                http_client.as_ref(),
                provider.0.as_str(),
                &api_url,
                &api_key,
                request,
                &extra_headers,
            );
            let response = request.await?;
            Ok(response)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }

    fn stream_response(
        &self,
        request: ResponseRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<futures::stream::BoxStream<'static, Result<ResponsesStreamEvent>>>>
    {
        let http_client = self.http_client.clone();

        let (api_key, api_url, extra_headers) = self.state.read_with(cx, |state, _cx| {
            (
                state.api_key_for_request(),
                state.settings.api_url.clone(),
                state.settings.custom_headers.clone(),
            )
        });

        let provider = self.provider_name.clone();
        let future = self.request_limiter.stream(async move {
            let Some(api_key) = api_key else {
                return Err(LanguageModelCompletionError::NoApiKey { provider });
            };
            let request = stream_response(
                http_client.as_ref(),
                provider.0.as_str(),
                &api_url,
                &api_key,
                request,
                &extra_headers,
            );
            let response = request.await?;
            Ok(response)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }
}

impl LanguageModel for OpenAiCompatibleLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(
            self.model
                .display_name
                .clone()
                .unwrap_or_else(|| self.model.name.clone()),
        )
    }

    fn provider_id(&self) -> LanguageModelProviderId {
        self.provider_id.clone()
    }

    fn provider_name(&self) -> LanguageModelProviderName {
        self.provider_name.clone()
    }

    fn supports_tools(&self) -> bool {
        self.model.capabilities.tools
    }

    fn tool_input_format(&self) -> LanguageModelToolSchemaFormat {
        if self.is_network_agent() {
            // Local OpenAI-compatible servers reject OpenAPI-style JsonSchemaSubset
            // fields (`nullable`, `anyOf`, etc.) that cloud providers accept.
            LanguageModelToolSchemaFormat::JsonSchema
        } else {
            LanguageModelToolSchemaFormat::JsonSchemaSubset
        }
    }

    fn supports_images(&self) -> bool {
        self.model.capabilities.images
    }

    fn supports_tool_choice(&self, choice: LanguageModelToolChoice) -> bool {
        match choice {
            LanguageModelToolChoice::Auto => self.model.capabilities.tools,
            LanguageModelToolChoice::Any => self.model.capabilities.tools,
            LanguageModelToolChoice::None => true,
        }
    }

    fn supports_streaming_tools(&self) -> bool {
        true
    }

    fn supports_split_token_display(&self) -> bool {
        true
    }

    fn telemetry_id(&self) -> String {
        format!("openai/{}", self.model.name)
    }

    fn max_token_count(&self) -> u64 {
        self.model.max_tokens
    }

    fn max_output_tokens(&self) -> Option<u64> {
        self.model.max_output_tokens
    }

    fn stream_completion(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        if self.model.capabilities.chat_completions {
            let mut request = into_open_ai(
                request,
                &self.model.name,
                self.model.capabilities.parallel_tool_calls,
                self.model.capabilities.prompt_cache_key,
                self.max_output_tokens(),
                self.model.reasoning_effort,
                self.model.capabilities.interleaved_reasoning,
            );
            if self.is_network_agent() {
                sanitize_local_openai_compatible_request(&mut request);
            }
            let completions = self.stream_completion(request, cx);
            async move {
                let mapper = OpenAiEventMapper::new();
                Ok(mapper.map_stream(completions.await?).boxed())
            }
            .boxed()
        } else {
            let request = into_open_ai_response(
                request,
                &self.model.name,
                self.model.capabilities.parallel_tool_calls,
                self.model.capabilities.prompt_cache_key,
                self.max_output_tokens(),
                self.model
                    .reasoning_effort
                    .filter(|effort| *effort != open_ai::ReasoningEffort::None),
                self.model.reasoning_effort == Some(open_ai::ReasoningEffort::None),
            );
            let completions = self.stream_response(request, cx);
            async move {
                let mapper = OpenAiResponseEventMapper::new();
                Ok(mapper.map_stream(completions.await?).boxed())
            }
            .boxed()
        }
    }
}

struct ConfigurationView {
    api_key_editor: Entity<InputField>,
    state: Entity<State>,
    load_credentials_task: Option<Task<()>>,
}

impl ConfigurationView {
    fn new(state: Entity<State>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key_editor = cx.new(|cx| {
            InputField::new(
                window,
                cx,
                "000000000000000000000000000000000000000000000000000",
            )
        });

        cx.observe(&state, |_, _, cx| {
            cx.notify();
        })
        .detach();

        let load_credentials_task = Some(cx.spawn_in(window, {
            let state = state.clone();
            async move |this, cx| {
                if let Some(task) = Some(state.update(cx, |state, cx| state.authenticate(cx))) {
                    // We don't log an error, because "not signed in" is also an error.
                    let _ = task.await;
                }
                this.update(cx, |this, cx| {
                    this.load_credentials_task = None;
                    cx.notify();
                })
                .log_err();
            }
        }));

        Self {
            api_key_editor,
            state,
            load_credentials_task,
        }
    }

    fn save_api_key(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let api_key = self.api_key_editor.read(cx).text(cx).trim().to_string();
        if api_key.is_empty() {
            return;
        }

        // url changes can cause the editor to be displayed again
        self.api_key_editor
            .update(cx, |input, cx| input.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(Some(api_key), cx))
                .await
        })
        .detach_and_log_err(cx);
    }

    fn reset_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.api_key_editor
            .update(cx, |input, cx| input.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(None, cx))
                .await
        })
        .detach_and_log_err(cx);
    }

    fn should_render_editor(&self, cx: &Context<Self>) -> bool {
        !self.state.read(cx).is_authenticated()
    }
}

impl Render for ConfigurationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.state.read(cx);
        let env_var_set = state.api_key_state.is_from_env_var();
        let env_var_name = state.api_key_state.env_var_name();

        let api_key_section = if self.should_render_editor(cx) {
            v_flex()
                .on_action(cx.listener(Self::save_api_key))
                .child(Label::new("To use LEAD's agent with an OpenAI-compatible provider, you need to add an API key."))
                .child(
                    div()
                        .pt(DynamicSpacing::Base04.rems(cx))
                        .child(self.api_key_editor.clone())
                )
                .child(
                    Label::new(
                        format!("You can also set the {env_var_name} environment variable and restart LEAD."),
                    )
                    .size(LabelSize::Small).color(Color::Muted),
                )
                .into_any()
        } else {
            h_flex()
                .mt_1()
                .p_1()
                .justify_between()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().background)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(Icon::new(IconName::Check).color(Color::Success))
                        .child(
                            div()
                                .w_full()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(Label::new(
                                    if env_var_set {
                                        format!("API key set in {env_var_name} environment variable")
                                    } else {
                                        format!("API key configured for {}", &state.settings.api_url)
                                    }
                                ))
                        ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .child(
                            Button::new("reset-api-key", "Reset API Key")
                                .label_size(LabelSize::Small)
                                .start_icon(Icon::new(IconName::Undo).size(IconSize::Small))
                                .layer(ElevationIndex::ModalSurface)
                                .when(env_var_set, |this| {
                                    this.tooltip(Tooltip::text(format!("To reset your API key, unset the {env_var_name} environment variable.")))
                                })
                                .on_click(cx.listener(|this, _, window, cx| this.reset_api_key(window, cx))),
                        ),
                )
                .into_any()
        };

        if self.load_credentials_task.is_some() {
            div().child(Label::new("Loading credentials…")).into_any()
        } else {
            v_flex().size_full().child(api_key_section).into_any()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::AsyncReadExt;
    use http_client::{AsyncBody, HttpClient, Method, Request as HttpRequest};
    use language_model::{
        LanguageModelRequest, LanguageModelRequestMessage, LanguageModelRequestTool,
        LanguageModelToolChoice, MessageContent, Role,
    };
    use open_ai::completion::into_open_ai;

    #[test]
    fn normalize_openai_compatible_api_url_appends_v1() {
        assert_eq!(
            normalize_openai_compatible_api_url("http://192.168.1.50:1234"),
            "http://192.168.1.50:1234/v1"
        );
        assert_eq!(
            normalize_openai_compatible_api_url("http://192.168.1.50:1234/v1"),
            "http://192.168.1.50:1234/v1"
        );
    }

    #[test]
    fn normalize_openai_compatible_api_url_strips_chat_completions_suffix() {
        assert_eq!(
            normalize_openai_compatible_api_url("http://192.168.1.50:1234/v1/chat/completions"),
            "http://192.168.1.50:1234/v1"
        );
    }

    #[test]
    fn sanitize_local_openai_compatible_request_requests_usage() {
        let mut request = into_open_ai(
            LanguageModelRequest {
                messages: vec![LanguageModelRequestMessage {
                    role: Role::User,
                    content: vec![MessageContent::Text("hi".into())],
                    cache: false,
                    reasoning_details: None,
                }],
                ..Default::default()
            },
            "test-model",
            false,
            false,
            None,
            None,
            false,
        );
        request.stream_options = None;
        sanitize_local_openai_compatible_request(&mut request);
        let options = request
            .stream_options
            .expect("stream_options should be set");
        assert!(options.include_usage);
    }

    /// Live test against a local OpenAI-compatible server.
    /// Run with:
    /// `NETWORK_AGENT_TEST_URL=http://host:1234/v1 NETWORK_AGENT_TEST_MODEL=model cargo test -p language_models live_network_agent -- --nocapture`
    #[tokio::test]
    async fn live_network_agent_chat_with_tools() {
        let Ok(api_url) = std::env::var("NETWORK_AGENT_TEST_URL") else {
            return;
        };
        let model_name = std::env::var("NETWORK_AGENT_TEST_MODEL")
            .unwrap_or_else(|_| "qwen3.5-9b-claude-4.6-opus-uncensored-distilled".into());

        let client = reqwest_client::ReqwestClient::user_agent("lead-network-agent-test")
            .expect("http client");

        let mut request = into_open_ai(
            LanguageModelRequest {
                messages: vec![LanguageModelRequestMessage {
                    role: Role::User,
                    content: vec![MessageContent::Text(
                        "Reply with exactly the word OK.".into(),
                    )],
                    cache: false,
                    reasoning_details: None,
                }],
                tools: vec![LanguageModelRequestTool {
                    name: "terminal".into(),
                    description: "Run a shell command".into(),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {
                            "command": { "type": "string" },
                            "cd": { "type": "string" }
                        },
                        "required": ["command", "cd"],
                        "additionalProperties": false
                    }),
                    use_input_streaming: false,
                }],
                tool_choice: Some(LanguageModelToolChoice::None),
                ..Default::default()
            },
            &model_name,
            false,
            false,
            None,
            None,
            false,
        );
        sanitize_local_openai_compatible_request(&mut request);

        let uri = format!(
            "{}/chat/completions",
            normalize_openai_compatible_api_url(&api_url)
        );
        let http_request = HttpRequest::builder()
            .method(Method::POST)
            .uri(uri)
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                format!("Bearer {}", LOCAL_OPENAI_COMPATIBLE_PLACEHOLDER_API_KEY),
            )
            .body(AsyncBody::from(serde_json::to_string(&request).unwrap()))
            .unwrap();

        let mut response = client.send(http_request).await.expect("request");
        let status = response.status();
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .expect("read body");

        assert!(
            status.is_success(),
            "expected success from {api_url}, got {status}: {body}"
        );
        assert!(
            body.contains("OK") || body.contains("ok") || body.contains("chat.completion"),
            "unexpected body: {body}"
        );
    }
}
