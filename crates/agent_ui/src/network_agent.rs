use agent_settings::AgentSettings;
use anyhow::Result;
use fs::Fs;
use gpui::{
    App, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Task, TaskExt,
};
use language_models::AllLanguageModelSettings;
use language_models::provider::open_ai_compatible::{AvailableModel, ModelCapabilities};
use settings::{OpenAiCompatibleSettingsContent, Settings, update_settings_file};
use ui::{Banner, KeyBinding, Modal, ModalFooter, ModalHeader, Section, prelude::*};
use ui_input::InputField;
use workspace::{ModalView, Workspace};

/// The reserved provider id used for the Network Agent's OpenAI-compatible endpoint.
pub const NETWORK_AGENT_PROVIDER_ID: &str = AgentSettings::NETWORK_AGENT_PROVIDER_ID;

const DEFAULT_MAX_TOKENS: u64 = 32_768;

/// Persists `agent.network_agent.enabled` so the toggle survives restarts.
pub fn set_network_agent_enabled(enabled: bool, cx: &mut App) {
    let fs = <dyn Fs>::global(cx);
    update_settings_file(fs, cx, move |settings, _cx| {
        let network_agent = settings
            .agent
            .get_or_insert_default()
            .network_agent
            .get_or_insert_default();
        network_agent.enabled = Some(enabled);
    });
}

fn single_line_input(
    label: impl Into<SharedString>,
    placeholder: &str,
    text: Option<&str>,
    tab_index: isize,
    window: &mut Window,
    cx: &mut App,
) -> Entity<InputField> {
    cx.new(|cx| {
        let input = InputField::new(window, cx, placeholder)
            .label(label)
            .tab_index(tab_index)
            .tab_stop(true);
        if let Some(text) = text {
            input.set_text(text, window, cx);
        }
        input
    })
}

struct NetworkAgentInput {
    api_url: Entity<InputField>,
    api_key: Entity<InputField>,
    model: Entity<InputField>,
    max_tokens: Entity<InputField>,
}

impl NetworkAgentInput {
    fn new(window: &mut Window, cx: &mut App) -> Self {
        let existing = AllLanguageModelSettings::get_global(cx)
            .openai_compatible
            .get(NETWORK_AGENT_PROVIDER_ID)
            .cloned();
        let existing_model = existing.as_ref().and_then(|s| s.available_models.first());

        let api_url = single_line_input(
            "Endpoint URL",
            "http://192.168.1.50:1234/v1",
            existing.as_ref().map(|s| s.api_url.as_str()),
            1,
            window,
            cx,
        );
        let api_key = cx.new(|cx| {
            InputField::new(window, cx, "Optional for most local servers")
                .label("API Key")
                .tab_index(2)
                .tab_stop(true)
                .masked(true)
        });
        let model = single_line_input(
            "Model",
            "e.g. qwen/qwen3-32b",
            existing_model.map(|m| m.name.as_str()),
            3,
            window,
            cx,
        );
        let max_tokens = single_line_input(
            "Context Window (max tokens)",
            "32768",
            Some(
                &existing_model
                    .map(|m| m.max_tokens)
                    .unwrap_or(DEFAULT_MAX_TOKENS)
                    .to_string(),
            ),
            4,
            window,
            cx,
        );

        Self {
            api_url,
            api_key,
            model,
            max_tokens,
        }
    }
}

fn save_network_agent_to_settings(
    input: &NetworkAgentInput,
    cx: &mut App,
) -> Task<Result<(), SharedString>> {
    let api_url = input.api_url.read(cx).text(cx);
    if api_url.is_empty() {
        return Task::ready(Err("Endpoint URL cannot be empty".into()));
    }

    let model_name = input.model.read(cx).text(cx);
    if model_name.is_empty() {
        return Task::ready(Err("Model cannot be empty".into()));
    }

    let max_tokens = match input.max_tokens.read(cx).text(cx).parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => return Task::ready(Err("Context Window must be a positive number".into())),
    };

    // Most local servers (LM Studio, Ollama) ignore the bearer token, but the
    // OpenAI-compatible provider only counts as authenticated once a key is
    // stored, so fall back to a harmless placeholder when none is provided.
    let api_key = {
        let entered = input.api_key.read(cx).text(cx);
        if entered.is_empty() {
            "network-agent".to_string()
        } else {
            entered
        }
    };

    let model = AvailableModel {
        name: model_name.clone(),
        display_name: Some(format!("{model_name} (network)")),
        max_completion_tokens: None,
        max_output_tokens: None,
        max_tokens,
        reasoning_effort: None,
        capabilities: ModelCapabilities {
            tools: true,
            images: false,
            parallel_tool_calls: false,
            prompt_cache_key: false,
            chat_completions: true,
            interleaved_reasoning: false,
        },
    };

    let fs = <dyn Fs>::global(cx);
    let credentials = cx.write_credentials(&api_url, "Bearer", api_key.as_bytes());
    cx.spawn(async move |cx| {
        credentials
            .await
            .map_err(|_| SharedString::from("Failed to write API key to keychain"))?;
        let _ = cx.update(|cx| {
            update_settings_file(fs, cx, move |settings, _cx| {
                settings
                    .language_models
                    .get_or_insert_default()
                    .openai_compatible
                    .get_or_insert_default()
                    .insert(
                        NETWORK_AGENT_PROVIDER_ID.into(),
                        OpenAiCompatibleSettingsContent {
                            api_url,
                            available_models: vec![model],
                            custom_headers: None,
                        },
                    );

                let network_agent = settings
                    .agent
                    .get_or_insert_default()
                    .network_agent
                    .get_or_insert_default();
                network_agent.model = Some(model_name);
                // Turn the Network Agent on once it has been configured.
                network_agent.enabled = Some(true);
            });
        });
        Ok(())
    })
}

pub struct NetworkAgentModal {
    input: NetworkAgentInput,
    focus_handle: FocusHandle,
    last_error: Option<SharedString>,
}

impl NetworkAgentModal {
    pub fn toggle(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        workspace.toggle_modal(window, cx, |window, cx| Self::new(window, cx));
    }

    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            input: NetworkAgentInput::new(window, cx),
            focus_handle: cx.focus_handle(),
            last_error: None,
        }
    }

    fn confirm(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        let task = save_network_agent_to_settings(&self.input, cx);
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| match result {
                Ok(_) => cx.emit(DismissEvent),
                Err(error) => {
                    this.last_error = Some(error);
                    cx.notify();
                }
            })
        })
        .detach_and_log_err(cx);
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn on_tab(&mut self, _: &menu::SelectNext, window: &mut Window, _cx: &mut Context<Self>) {
        window.focus_next(_cx);
    }

    fn on_tab_prev(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        window.focus_prev(_cx);
    }
}

impl EventEmitter<DismissEvent> for NetworkAgentModal {}

impl Focusable for NetworkAgentModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for NetworkAgentModal {}

impl Render for NetworkAgentModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.focus_handle(cx);

        v_flex()
            .id("network-agent-modal")
            .key_context("NetworkAgentModal")
            .w(rems(30.))
            .elevation_3(cx)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::on_tab))
            .on_action(cx.listener(Self::on_tab_prev))
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                this.focus_handle(cx).focus(window, cx);
            }))
            .child(
                Modal::new("network-agent", None)
                    .header(
                        ModalHeader::new().headline("Configure Network Agent").description(
                            "A model served on your network drives the agent; your local model \
                             handles delegated subagent work.",
                        ),
                    )
                    .when_some(self.last_error.clone(), |this, error| {
                        this.section(
                            Section::new().child(
                                Banner::new()
                                    .severity(Severity::Warning)
                                    .child(div().text_xs().child(error)),
                            ),
                        )
                    })
                    .child(
                        v_flex()
                            .id("network-agent-content")
                            .tab_group()
                            .pl_3()
                            .pr_4()
                            .pb_2()
                            .gap_2()
                            .child(self.input.api_url.clone())
                            .child(self.input.api_key.clone())
                            .child(self.input.model.clone())
                            .child(self.input.max_tokens.clone()),
                    )
                    .footer(
                        ModalFooter::new().end_slot(
                            h_flex()
                                .gap_1()
                                .child(
                                    Button::new("cancel", "Cancel")
                                        .key_binding(
                                            KeyBinding::for_action_in(
                                                &menu::Cancel,
                                                &focus_handle,
                                                cx,
                                            )
                                            .map(|kb| kb.size(rems_from_px(12.))),
                                        )
                                        .on_click(cx.listener(|this, _event, window, cx| {
                                            this.cancel(&menu::Cancel, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("save", "Save & Enable")
                                        .key_binding(
                                            KeyBinding::for_action_in(
                                                &menu::Confirm,
                                                &focus_handle,
                                                cx,
                                            )
                                            .map(|kb| kb.size(rems_from_px(12.))),
                                        )
                                        .on_click(cx.listener(|this, _event, window, cx| {
                                            this.confirm(&menu::Confirm, window, cx)
                                        })),
                                ),
                        ),
                    ),
            )
    }
}
