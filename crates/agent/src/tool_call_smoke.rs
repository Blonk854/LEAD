//! Smoke tests for native tool calling against a language model.
//!
//! Used by the agent panel UI and by `script/run-local-tool-eval-matrix`.

use crate::{SystemPromptTemplate, Templates, built_in_tools};
use anyhow::Result;
use futures::{StreamExt as _, future::select};
use gpui::AsyncApp;
use language_model::{
    LanguageModel, LanguageModelCompletionEvent, LanguageModelRequest, LanguageModelRequestMessage,
    LanguageModelToolChoice, LanguageModelToolUse, MessageContent, Role,
};
use prompt_store::{ProjectContext, WorktreeContext};
use std::{fmt, path::Path, sync::Arc, time::Duration};

const SMOKE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "terminal",
    "grep",
    "list_directory",
    "find_path",
    "write_file",
    "edit_file",
];

const DEFAULT_STREAM_TIMEOUT: Duration = Duration::from_secs(90);

/// A single tool-call smoke scenario.
#[derive(Clone, Debug)]
pub struct ToolCallSmokeCase {
    pub name: &'static str,
    pub prompt: &'static str,
    pub expected_tools: &'static [&'static str],
}

/// Outcome of one smoke scenario.
#[derive(Clone, Debug)]
pub struct ToolCallSmokeCaseResult {
    pub case: &'static str,
    pub got_tool: bool,
    pub tool_name: Option<String>,
    pub json_parse_errors: Vec<String>,
    pub streamed_text_preview: String,
    pub expected_tools: Vec<&'static str>,
}

impl ToolCallSmokeCaseResult {
    pub fn passed(&self) -> bool {
        self.got_tool
            && self.tool_name.as_ref().is_some_and(|name| {
                self.expected_tools
                    .iter()
                    .any(|expected| *expected == name.as_str())
            })
            && self.json_parse_errors.is_empty()
    }
}

impl fmt::Display for ToolCallSmokeCaseResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Case: {}", self.case)?;
        writeln!(f, "Expected one of: {:?}", self.expected_tools)?;
        writeln!(
            f,
            "Got tool: {} ({})",
            self.got_tool,
            self.tool_name.as_deref().unwrap_or("none")
        )?;
        if !self.json_parse_errors.is_empty() {
            writeln!(f, "JSON parse errors:")?;
            for err in &self.json_parse_errors {
                writeln!(f, "  - {err}")?;
            }
        }
        if !self.streamed_text_preview.is_empty() {
            writeln!(f, "Streamed text preview:\n{}", self.streamed_text_preview)?;
        }
        Ok(())
    }
}

/// Full smoke-test report for one model.
#[derive(Clone, Debug)]
pub struct ToolCallSmokeReport {
    pub model_label: String,
    pub results: Vec<ToolCallSmokeCaseResult>,
}

impl ToolCallSmokeReport {
    pub fn passed(&self) -> bool {
        self.results.iter().all(|result| result.passed())
    }

    pub fn failure_count(&self) -> usize {
        self.results
            .iter()
            .filter(|result| !result.passed())
            .count()
    }

    pub fn summary(&self) -> String {
        let mut lines = format!("Model: {}\n", self.model_label);
        for result in &self.results {
            let status = if result.passed() { "PASS" } else { "FAIL" };
            lines.push_str(&format!("{status} {result}\n---\n"));
        }
        if self.passed() {
            lines.push_str(&format!("All {} smoke cases passed.\n", self.results.len()));
        } else {
            lines.push_str(&format!(
                "{}/{} smoke cases failed.\n",
                self.failure_count(),
                self.results.len()
            ));
        }
        lines
    }
}

/// Options for [`run_tool_call_smoke`].
#[derive(Clone, Debug)]
pub struct ToolCallSmokeOptions {
    pub skip_warmup: bool,
    pub stream_timeout: Duration,
}

impl Default for ToolCallSmokeOptions {
    fn default() -> Self {
        Self {
            skip_warmup: std::env::var("ZED_TOOL_EVAL_SKIP_WARMUP").is_ok(),
            stream_timeout: std::env::var("ZED_TOOL_EVAL_STREAM_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_STREAM_TIMEOUT),
        }
    }
}

pub const SMOKE_CASES: &[ToolCallSmokeCase] = &[
    ToolCallSmokeCase {
        name: "terminal_git_status",
        prompt: "You must call the terminal tool. Run `git status` in the project root.",
        expected_tools: &["terminal"],
    },
    ToolCallSmokeCase {
        name: "read_file",
        prompt: "You must call the read_file tool (do not guess). Read Cargo.toml.",
        expected_tools: &["read_file"],
    },
    ToolCallSmokeCase {
        name: "list_or_search",
        prompt: "You must call a search or list tool (grep, list_directory, or find_path). Find Rust source files under src/. Do not use the terminal.",
        expected_tools: &["grep", "list_directory", "find_path"],
    },
    // JSX in the arguments guards against `<` in argument values being
    // misrouted through XML tool-call repair (see finish_lmstudio_tool_call).
    ToolCallSmokeCase {
        name: "write_file_jsx",
        prompt: "You must call the write_file tool. Create `root/src/App.tsx` containing exactly:\nexport const App = () => <div className=\"app\">Hello</div>;",
        expected_tools: &["write_file"],
    },
    ToolCallSmokeCase {
        name: "edit_file_jsx",
        prompt: "You must call the edit_file tool. In `root/src/App.tsx`, replace the text `<div className=\"app\">Hello</div>` with `<div className=\"app\">Hello, LEAD!</div>`. One edit only.",
        expected_tools: &["edit_file"],
    },
];

/// Offer only the case's expected tools. The smoke test verifies tool-call
/// *formatting*, not tool selection: with the full set available, models
/// legitimately pick a different tool (e.g. read_file before edit_file),
/// which is fine in a real session but makes the smoke signal flaky.
fn smoke_tools_for(
    model: &dyn LanguageModel,
    allowed_tools: &[&str],
) -> Vec<language_model::LanguageModelRequestTool> {
    let format = model.tool_input_format();
    built_in_tools()
        .filter(|tool| {
            SMOKE_TOOL_NAMES.contains(&tool.name.as_str())
                && allowed_tools.contains(&tool.name.as_str())
        })
        .filter_map(|mut tool| {
            language_model::tool_schema::adapt_schema_to_format(&mut tool.input_schema, format)
                .ok()?;
            Some(tool)
        })
        .collect()
}

/// Run the tool-call smoke matrix against `model`.
pub async fn run_tool_call_smoke(
    model: Arc<dyn LanguageModel>,
    cx: &mut AsyncApp,
    options: ToolCallSmokeOptions,
) -> Result<ToolCallSmokeReport> {
    let model_label = format!("{}/{}", model.provider_id().0, model.id().0);

    if !options.skip_warmup {
        let warmup = ToolCallSmokeCase {
            name: "warmup",
            prompt: "Call list_directory with path \".\". Tool call only, no prose.",
            expected_tools: &["list_directory"],
        };
        let _ = run_case_once(&model, &warmup, cx, options.stream_timeout).await?;
    }

    let mut results = Vec::with_capacity(SMOKE_CASES.len());
    for case in SMOKE_CASES {
        results.push(run_case(&model, case, cx, options.stream_timeout).await?);
    }

    Ok(ToolCallSmokeReport {
        model_label,
        results,
    })
}

async fn run_case(
    model: &Arc<dyn LanguageModel>,
    case: &ToolCallSmokeCase,
    cx: &mut AsyncApp,
    stream_timeout: Duration,
) -> Result<ToolCallSmokeCaseResult> {
    const MAX_ATTEMPTS: usize = 2;

    for attempt in 1..=MAX_ATTEMPTS {
        let result = run_case_once(model, case, cx, stream_timeout).await?;
        if result.passed() {
            return Ok(result);
        }
        if attempt < MAX_ATTEMPTS {
            log::debug!(
                "Retrying tool-call smoke case {} (attempt {}/{})",
                case.name,
                attempt + 1,
                MAX_ATTEMPTS
            );
        } else {
            return Ok(result);
        }
    }

    unreachable!()
}

async fn run_case_once(
    model: &Arc<dyn LanguageModel>,
    case: &ToolCallSmokeCase,
    cx: &mut AsyncApp,
    stream_timeout: Duration,
) -> Result<ToolCallSmokeCaseResult> {
    let tools = smoke_tools_for(model.as_ref(), case.expected_tools);
    let system_prompt = {
        let worktrees = vec![WorktreeContext {
            root_name: "root".to_string(),
            abs_path: Path::new("/path/to/root").into(),
            rules_file: None,
        }];
        let project_context = ProjectContext::new(worktrees);
        let tool_names = tools.iter().map(|tool| tool.name.clone().into()).collect();
        SystemPromptTemplate {
            project: &project_context,
            available_tools: tool_names,
            model_name: Some(model.name().0.to_string()),
            date: chrono::Local::now().format("%Y-%m-%d").to_string(),
            user_agents_md: None,
            sandboxing: false,
            active_goal: None,
            project_memory: None,
            hybrid_mode: false,
            local_worker_model: None,
            balanced_delegation: false,
            full_access: false,
        }
        .render_for_style(&Templates::new(), true)?
    };

    let request = LanguageModelRequest {
        messages: vec![
            LanguageModelRequestMessage {
                role: Role::System,
                content: vec![MessageContent::Text(system_prompt)],
                cache: true,
                reasoning_details: None,
            },
            LanguageModelRequestMessage {
                role: Role::User,
                content: vec![MessageContent::Text(case.prompt.into())],
                cache: false,
                reasoning_details: None,
            },
        ],
        tools,
        tool_choice: Some(LanguageModelToolChoice::Any),
        thinking_allowed: false,
        thinking_effort: None,
        ..Default::default()
    };

    let extraction = extract_tool_call(model, request, cx, stream_timeout).await?;

    Ok(ToolCallSmokeCaseResult {
        case: case.name,
        got_tool: extraction.tool_use.is_some(),
        tool_name: extraction
            .tool_use
            .as_ref()
            .map(|tool| tool.name.to_string()),
        json_parse_errors: extraction.json_parse_errors,
        streamed_text_preview: extraction.streamed_text,
        expected_tools: case.expected_tools.to_vec(),
    })
}

struct ToolCallExtraction {
    tool_use: Option<LanguageModelToolUse>,
    json_parse_errors: Vec<String>,
    streamed_text: String,
}

async fn extract_tool_call(
    model: &Arc<dyn LanguageModel>,
    request: LanguageModelRequest,
    cx: &mut AsyncApp,
    stream_timeout: Duration,
) -> Result<ToolCallExtraction> {
    let tool_names: Vec<String> = request.tools.iter().map(|tool| tool.name.clone()).collect();
    let tool_name_refs: Vec<&str> = tool_names.iter().map(String::as_str).collect();

    let mut events = model
        .stream_completion(request, cx)
        .await
        .map_err(|err| anyhow::anyhow!("completion error: {err}"))?
        .fuse();

    let mut tool_use = None;
    let mut json_parse_errors = Vec::new();
    let mut streamed_text = String::new();
    let mut full_text = String::new();
    let mut whitespace_text_len = 0usize;
    const MAX_WHITESPACE_TEXT: usize = 512;

    let deadline = std::time::Instant::now() + stream_timeout;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            json_parse_errors.push(format!(
                "stream timed out after {}s",
                stream_timeout.as_secs()
            ));
            break;
        }

        match select(events.next(), async_io::Timer::after(remaining)).await {
            futures::future::Either::Left((Some(event), _)) => match event {
                Ok(LanguageModelCompletionEvent::ToolUse(candidate))
                    if candidate.is_input_complete =>
                {
                    tool_use = Some(normalize_smoke_tool_use(candidate));
                    break;
                }
                Ok(LanguageModelCompletionEvent::Text(text)) => {
                    full_text.push_str(&text);
                    if text.chars().all(char::is_whitespace) {
                        whitespace_text_len += text.len();
                        if whitespace_text_len >= MAX_WHITESPACE_TEXT && tool_use.is_none() {
                            json_parse_errors.push(
                                "model streamed whitespace without a tool call; aborting early"
                                    .into(),
                            );
                            break;
                        }
                    } else {
                        whitespace_text_len = 0;
                    }
                    if streamed_text.len() < 500 {
                        streamed_text.push_str(&text);
                    }
                }
                Ok(LanguageModelCompletionEvent::Thinking { text, .. }) => {
                    full_text.push_str(&text);
                }
                Ok(LanguageModelCompletionEvent::ToolUseJsonParseError {
                    tool_name,
                    raw_input,
                    json_parse_error,
                    ..
                }) => {
                    json_parse_errors.push(format!(
                        "{tool_name}: {json_parse_error}\nraw: {raw_input:?}"
                    ));
                }
                Err(err) => return Err(err.into()),
                _ => {}
            },
            futures::future::Either::Left((None, _)) => break,
            futures::future::Either::Right((_, _)) => {
                json_parse_errors.push(format!(
                    "stream timed out after {}s",
                    stream_timeout.as_secs()
                ));
                break;
            }
        }
    }

    if tool_use.is_none() {
        if let Some(call) =
            language_model::extract_tool_calls_from_text(&full_text, &tool_name_refs)
                .into_iter()
                .next()
        {
            tool_use = Some(LanguageModelToolUse {
                id: "embedded_tool_0".into(),
                name: call.name,
                is_input_complete: true,
                input: call.input,
                raw_input: call.raw_input,
                thought_signature: None,
            });
        }
    }

    Ok(ToolCallExtraction {
        tool_use,
        json_parse_errors,
        streamed_text,
    })
}

fn normalize_smoke_tool_use(tool_use: LanguageModelToolUse) -> LanguageModelToolUse {
    let name = tool_use.name.as_ref();
    if !name.contains('<') {
        return tool_use;
    }

    if let Some(repaired) = language_model::repair_malformed_tool_use(name, &tool_use.raw_input) {
        return LanguageModelToolUse {
            name: repaired.name,
            input: repaired.input,
            raw_input: repaired.raw_input,
            ..tool_use
        };
    }

    let blob = format!("{name}{}", tool_use.raw_input);
    if let Some(call) = language_model::extract_tool_calls_from_text(&blob, &[])
        .into_iter()
        .next()
    {
        return LanguageModelToolUse {
            name: call.name,
            input: call.input,
            raw_input: call.raw_input,
            ..tool_use
        };
    }

    tool_use
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_case_result_passes_when_tool_matches() {
        let result = ToolCallSmokeCaseResult {
            case: "read_file",
            got_tool: true,
            tool_name: Some("read_file".into()),
            json_parse_errors: Vec::new(),
            streamed_text_preview: String::new(),
            expected_tools: vec!["read_file"],
        };
        assert!(result.passed());
    }

    #[test]
    fn smoke_report_counts_failures() {
        let report = ToolCallSmokeReport {
            model_label: "lmstudio/test".into(),
            results: vec![
                ToolCallSmokeCaseResult {
                    case: "read_file",
                    got_tool: true,
                    tool_name: Some("read_file".into()),
                    json_parse_errors: Vec::new(),
                    streamed_text_preview: String::new(),
                    expected_tools: vec!["read_file"],
                },
                ToolCallSmokeCaseResult {
                    case: "terminal_git_status",
                    got_tool: false,
                    tool_name: None,
                    json_parse_errors: vec!["timeout".into()],
                    streamed_text_preview: String::new(),
                    expected_tools: vec!["terminal"],
                },
            ],
        };
        assert!(!report.passed());
        assert_eq!(report.failure_count(), 1);
        assert!(report.summary().contains("1/2 smoke cases failed"));
    }
}
