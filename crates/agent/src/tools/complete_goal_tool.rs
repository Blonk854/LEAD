use crate::{AgentTool, Thread, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema as acp;
use gpui::{App, SharedString, Task, WeakEntity};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Ends an active goal once success criteria are met (stops auto-continuation).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct CompleteGoalToolInput {
    /// Optional one-line reason or evidence that the success criteria were met.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompleteGoalToolOutput {
    Success { message: String },
    Error { error: String },
}

impl From<CompleteGoalToolOutput> for LanguageModelToolResultContent {
    fn from(output: CompleteGoalToolOutput) -> Self {
        match output {
            CompleteGoalToolOutput::Success { message } => message.into(),
            CompleteGoalToolOutput::Error { error } => error.into(),
        }
    }
}

pub struct CompleteGoalTool {
    thread: WeakEntity<Thread>,
}

impl CompleteGoalTool {
    pub fn new(thread: WeakEntity<Thread>) -> Self {
        Self { thread }
    }
}

impl AgentTool for CompleteGoalTool {
    type Input = CompleteGoalToolInput;
    type Output = CompleteGoalToolOutput;

    const NAME: &'static str = "complete_goal";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Think
    }

    fn initial_title(
        &self,
        _input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        "Complete goal".into()
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        _event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        let thread = self.thread.clone();
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| CompleteGoalToolOutput::Error {
                    error: error.to_string(),
                })?;

            let reason = input
                .reason
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());

            thread
                .update(cx, |thread, cx| {
                    thread.complete_active_goal(reason.as_deref(), cx)
                })
                .map_err(|error| CompleteGoalToolOutput::Error {
                    error: error.to_string(),
                })?
                .map(|message| CompleteGoalToolOutput::Success { message })
                .map_err(|error| CompleteGoalToolOutput::Error { error })
        })
    }
}
