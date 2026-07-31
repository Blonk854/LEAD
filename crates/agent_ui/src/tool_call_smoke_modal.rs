use agent::{ToolCallSmokeOptions, run_tool_call_smoke};
use gpui::{
    App, Context, DismissEvent, EventEmitter, FocusHandle, Focusable, Render, SharedString, Task,
    Window,
};
use language_model::LanguageModel;
use std::sync::Arc;
use ui::{Banner, KeyBinding, Modal, ModalFooter, ModalHeader, Section, SpinnerLabel, prelude::*};
use workspace::{ModalView, Workspace};

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolCallSmokeModalPhase {
    Running,
    Complete,
    Failed,
}

pub struct ToolCallSmokeModal {
    model_label: SharedString,
    phase: ToolCallSmokeModalPhase,
    passed: Option<bool>,
    log: SharedString,
    focus_handle: FocusHandle,
    _run_task: Option<Task<()>>,
}

impl ToolCallSmokeModal {
    pub fn toggle(
        workspace: &mut Workspace,
        model: Arc<dyn LanguageModel>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, |window, cx| Self::new(model, window, cx));
    }

    fn new(model: Arc<dyn LanguageModel>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model_label: SharedString =
            format!("{}/{}", model.provider_id().0, model.id().0).into();
        let focus_handle = cx.focus_handle();

        let mut this = Self {
            model_label: model_label.clone(),
            phase: ToolCallSmokeModalPhase::Running,
            passed: None,
            log: format!("Model: {model_label}\n\nRunning tool-call smoke test…\n").into(),
            focus_handle,
            _run_task: None,
        };

        let weak = cx.weak_entity();
        let task = cx.spawn(async move |_, cx| {
            let outcome = run_tool_call_smoke(model, cx, ToolCallSmokeOptions::default()).await;
            weak.update(cx, |this, cx| match outcome {
                Ok(report) => {
                    this.passed = Some(report.passed());
                    this.log = report.summary().into();
                    this.phase = ToolCallSmokeModalPhase::Complete;
                    cx.notify();
                }
                Err(error) => {
                    this.passed = None;
                    let hint = if format!("{error:#}").contains("No API key")
                        || format!("{error:#}").contains("NoApiKey")
                    {
                        "\n\nHint: Re-open Configure Network Agent, save again, and ensure LEAD was rebuilt from the latest source."
                    } else {
                        ""
                    };
                    this.log = format!(
                        "Model: {}\n\nError: {error:#}{hint}",
                        this.model_label
                    )
                    .into();
                    this.phase = ToolCallSmokeModalPhase::Failed;
                    cx.notify();
                }
            })
            .ok();
        });

        this._run_task = Some(task);
        this
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn close(&mut self, _: &menu::Confirm, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn passed(&self) -> bool {
        self.passed == Some(true)
    }
}

impl EventEmitter<DismissEvent> for ToolCallSmokeModal {}

impl Focusable for ToolCallSmokeModal {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ToolCallSmokeModal {}

impl Render for ToolCallSmokeModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.focus_handle(cx);
        let running = self.phase == ToolCallSmokeModalPhase::Running;

        let (headline, description, severity) = match self.phase {
            ToolCallSmokeModalPhase::Running => (
                "Testing Tool Calling",
                "Sending short prompts that should trigger built-in tools. This can take a few minutes on local models.",
                None,
            ),
            ToolCallSmokeModalPhase::Complete if self.passed() => (
                "Tool Calling Test Passed",
                "This model returned valid tool calls for all smoke scenarios.",
                Some(Severity::Success),
            ),
            ToolCallSmokeModalPhase::Complete => (
                "Tool Calling Test Failed",
                "This model missed or malformed one or more expected tool calls.",
                Some(Severity::Warning),
            ),
            ToolCallSmokeModalPhase::Failed => (
                "Tool Calling Test Error",
                "The smoke test could not finish. Check that the model server is running.",
                Some(Severity::Error),
            ),
        };

        v_flex()
            .id("tool-call-smoke-modal")
            .key_context("ToolCallSmokeModal")
            .w(rems(36.))
            .elevation_3(cx)
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::close))
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                this.focus_handle(cx).focus(window, cx);
            }))
            .child(
                Modal::new("tool-call-smoke", None)
                    .header(
                        ModalHeader::new()
                            .headline(headline)
                            .description(description),
                    )
                    .when(running, |this| {
                        this.section(
                            Section::new().child(
                                Banner::new().severity(Severity::Info).child(
                                    h_flex()
                                        .gap_2()
                                        .child(SpinnerLabel::new().size(LabelSize::Small))
                                        .child(
                                            Label::new("Running smoke cases…")
                                                .size(LabelSize::Small),
                                        ),
                                ),
                            ),
                        )
                    })
                    .when_some(severity, |this, severity| {
                        this.section(
                            Section::new().child(
                                Banner::new().severity(severity).child(
                                    Label::new(match severity {
                                        Severity::Success => "All smoke cases passed.",
                                        Severity::Warning => "One or more smoke cases failed.",
                                        _ => "The smoke test failed to run.",
                                    })
                                    .size(LabelSize::Small),
                                ),
                            ),
                        )
                    })
                    .child(
                        div()
                            .id("tool-call-smoke-log")
                            .mx_3()
                            .mb_2()
                            .p_2()
                            .max_h(rems(24.))
                            .overflow_y_scroll()
                            .rounded_md()
                            .bg(cx.theme().colors().surface_background)
                            .text_xs()
                            .child(self.log.clone()),
                    )
                    .footer(
                        ModalFooter::new().end_slot(
                            Button::new(
                                "close-tool-call-smoke",
                                if running { "Cancel" } else { "Close" },
                            )
                            .key_binding(
                                KeyBinding::for_action_in(
                                    if running {
                                        &menu::Cancel
                                    } else {
                                        &menu::Confirm
                                    },
                                    &focus_handle,
                                    cx,
                                )
                                .map(|kb| kb.size(rems_from_px(12.))),
                            )
                            .on_click(cx.listener(
                                |this, _event, window, cx| {
                                    if this.phase == ToolCallSmokeModalPhase::Running {
                                        this.cancel(&menu::Cancel, window, cx);
                                    } else {
                                        this.close(&menu::Confirm, window, cx);
                                    }
                                },
                            )),
                        ),
                    ),
            )
    }
}
