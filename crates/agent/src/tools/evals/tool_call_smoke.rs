//! Fast smoke evals for native tool calling across providers/models.
//!
//! Run one model:
//!   ZED_AGENT_MODEL=lmstudio/opus4.7-gods.ghost.codex-4b.gguf cargo nextest run -p agent --features unit-eval eval_tool_call_smoke --no-capture
//!
//! Run the full local matrix:
//!   ./script/run-local-tool-eval-matrix

use super::model::load_eval_model;
use crate::{SystemPromptTemplate, Templates, templates::Template};
use anyhow::Result;
use futures::{FutureExt as _, StreamExt as _, future::select};
use gpui::TestAppContext;
use language_model::{
    LanguageModel, LanguageModelCompletionEvent, LanguageModelRequest,
    LanguageModelRequestMessage, LanguageModelToolChoice, LanguageModelToolUse, MessageContent,
    Role,
};
use prompt_store::{ProjectContext, WorktreeContext};
use std::{fmt, path::Path, sync::Arc, time::Duration};

#[derive(Clone, Debug)]
struct SmokeCase {
    name: &'static str,
    prompt: &'static str,
    expected_tools: &'static [&'static str],
}

#[derive(Debug)]
struct SmokeResult {
    case: &'static str,
    got_tool: bool,
    tool_name: Option<String>,
    json_parse_errors: Vec<String>,
    streamed_text_preview: String,
    expected_tools: Vec<&'static str>,
}

impl fmt::Display for SmokeResult {
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

impl SmokeResult {
    fn passed(&self) -> bool {
        self.got_tool
            && self.tool_name.as_ref().is_some_and(|name| {
                self.expected_tools
                    .iter()
                    .any(|expected| *expected == name.as_str())
            })
            && self.json_parse_errors.is_empty()
    }
}

const SMOKE_TOOL_NAMES: &[&str] = &[
    "read_file",
    "terminal",
    "grep",
    "list_directory",
    "find_path",
];

fn smoke_tools() -> Vec<language_model::LanguageModelRequestTool> {
    crate::built_in_tools()
        .filter(|tool| SMOKE_TOOL_NAMES.contains(&tool.name.as_str()))
        .collect()
}

const CASES: &[SmokeCase] = &[
    SmokeCase {
        name: "terminal_git_status",
        prompt: "You must call the terminal tool. Run `git status` in the project root.",
        expected_tools: &["terminal"],
    },
    SmokeCase {
        name: "read_file",
        prompt: "You must call the read_file tool (do not guess). Read Cargo.toml.",
        expected_tools: &["read_file"],
    },
    SmokeCase {
        name: "list_or_search",
        prompt: "You must call a search or list tool (grep, list_directory, or find_path). Find Rust source files under src/. Do not use the terminal.",
        expected_tools: &["grep", "list_directory", "find_path"],
    },
];

struct ToolCallSmokeTest {
    model: Arc<dyn LanguageModel>,
}

impl ToolCallSmokeTest {
    async fn new(cx: &mut TestAppContext) -> Self {
        let model = load_eval_model(cx).await;
        Self { model }
    }

    /// Prime the model after load/reload. Large reasoning models (e.g. Qwen3-14B)
    /// can stream blank content for minutes on the first heavy prompt.
    async fn warmup(&self, cx: &mut TestAppContext) -> Result<()> {
        if std::env::var("ZED_TOOL_EVAL_SKIP_WARMUP").is_ok() {
            return Ok(());
        }

        eprintln!("Warming up model before smoke cases...");
        let warmup = SmokeCase {
            name: "warmup",
            prompt: "Call list_directory with path \".\". Tool call only, no prose.",
            expected_tools: &["list_directory"],
        };
        let _ = self.run_case_once(&warmup, cx).await?;
        Ok(())
    }

    async fn run_case(&self, case: &SmokeCase, cx: &mut TestAppContext) -> Result<SmokeResult> {
        const MAX_ATTEMPTS: usize = 2;

        for attempt in 1..=MAX_ATTEMPTS {
            let result = self.run_case_once(case, cx).await?;
            if result.passed() {
                return Ok(result);
            }
            if attempt < MAX_ATTEMPTS {
                eprintln!(
                    "Retrying {} (attempt {}/{})",
                    case.name, attempt + 1, MAX_ATTEMPTS
                );
            } else {
                return Ok(result);
            }
        }

        unreachable!()
    }

    async fn run_case_once(&self, case: &SmokeCase, cx: &mut TestAppContext) -> Result<SmokeResult> {
        let tools = smoke_tools();
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
                model_name: Some(self.model.name().0.to_string()),
                date: chrono::Local::now().format("%Y-%m-%d").to_string(),
                user_agents_md: None,
                sandboxing: false,
                active_goal: None,
            }
            .render(&Templates::new())?
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
            // Tool-call smoke tests only need a single tool invocation; reasoning
            // models like Qwen3 can stream thinking for minutes and peg the GPU.
            thinking_allowed: false,
            thinking_effort: None,
            ..Default::default()
        };

        let extraction = extract_tool_call(&self.model, request, cx).await?;

        Ok(SmokeResult {
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
}

struct ToolCallExtraction {
    tool_use: Option<LanguageModelToolUse>,
    json_parse_errors: Vec<String>,
    streamed_text: String,
}

async fn extract_tool_call(
    model: &Arc<dyn LanguageModel>,
    request: LanguageModelRequest,
    cx: &mut TestAppContext,
) -> Result<ToolCallExtraction> {
    let tool_names: Vec<String> = request.tools.iter().map(|tool| tool.name.clone()).collect();
    let tool_name_refs: Vec<&str> = tool_names.iter().map(String::as_str).collect();

    let model = model.clone();
    let events = cx
        .update(|cx| {
            let async_cx = cx.to_async();
            cx.foreground_executor().spawn(async move {
                model.stream_completion(request, &async_cx).await
            })
        })
        .await
        .map_err(|err| anyhow::anyhow!("completion error: {err}"))?;

    let mut tool_use = None;
    let mut json_parse_errors = Vec::new();
    let mut streamed_text = String::new();
    let mut full_text = String::new();
    let mut whitespace_text_len = 0usize;
    const MAX_WHITESPACE_TEXT: usize = 512;

    let mut events = events.fuse();
    let wall_timeout = stream_wall_timeout();
    let deadline = std::time::Instant::now() + wall_timeout;

    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            json_parse_errors.push(format!(
                "stream timed out after {}s (set ZED_TOOL_EVAL_STREAM_TIMEOUT_SECS to override)",
                wall_timeout.as_secs()
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
                    "stream timed out after {}s (set ZED_TOOL_EVAL_STREAM_TIMEOUT_SECS to override)",
                    wall_timeout.as_secs()
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

fn stream_wall_timeout() -> Duration {
    const DEFAULT_SECS: u64 = 90;
    std::env::var("ZED_TOOL_EVAL_STREAM_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(DEFAULT_SECS))
}

fn normalize_smoke_tool_use(tool_use: LanguageModelToolUse) -> LanguageModelToolUse {
    let name = tool_use.name.as_ref();
    if !name.contains('<') {
        return tool_use;
    }

    if let Some(repaired) =
        language_model::repair_malformed_tool_use(name, &tool_use.raw_input)
    {
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

fn run_smoke_matrix() -> eval_utils::EvalOutput<()> {
    super::run_gpui_eval(
        |cx| {
            async move {
                let test = ToolCallSmokeTest::new(cx).await;
                test.warmup(cx).await?;
                let mut lines = String::new();
                let mut failures = 0usize;

                for case in CASES {
                    let result = test.run_case(case, cx).await?;
                    let status = if result.passed() { "PASS" } else { "FAIL" };
                    eprintln!("{status} {result}");
                    eprintln!("---");
                    if !result.passed() {
                        failures += 1;
                    }
                    lines.push_str(&format!("{result}\n---\n"));
                }

                if failures > 0 {
                    anyhow::bail!("{failures}/{} smoke cases failed\n{lines}", CASES.len());
                }

                Ok(lines)
            }
            .boxed_local()
        },
        |_| eval_utils::OutcomeKind::Passed,
    )
}

#[test]
#[cfg_attr(not(feature = "unit-eval"), ignore)]
fn eval_tool_call_smoke() {
    // One iteration per model — the matrix script runs this once per entry.
    eval_utils::eval(1, 1.0, eval_utils::NoProcessor, run_smoke_matrix);
}
