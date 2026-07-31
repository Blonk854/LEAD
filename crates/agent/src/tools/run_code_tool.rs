use std::{io::Write as _, rc::Rc, sync::Arc, time::Duration};

use agent_client_protocol::schema as acp;
use futures::FutureExt as _;
use gpui::{App, Entity, Task};
use project::Project;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::SharedString;
use util::shell::ShellKind;

use crate::{AgentTool, ThreadEnvironment, ToolCallEventStream, ToolInput, ToolPermissionContext};

const OUTPUT_LIMIT: u64 = 16 * 1024;
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Executes a source-code snippet using an installed local interpreter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct RunCodeToolInput {
    /// Interpreter language: `python`, `node`, `powershell`, or `bash`.
    pub language: String,
    /// Source code to execute.
    pub code: String,
    /// Absolute working directory. Defaults to the first project worktree.
    pub cd: Option<String>,
    /// Maximum runtime in milliseconds (default 120000, maximum 1800000).
    pub timeout_ms: Option<u64>,
}

pub struct RunCodeTool {
    project: Entity<Project>,
    environment: Rc<dyn ThreadEnvironment>,
}

impl RunCodeTool {
    pub fn new(project: Entity<Project>, environment: Rc<dyn ThreadEnvironment>) -> Self {
        Self {
            project,
            environment,
        }
    }
}

impl AgentTool for RunCodeTool {
    type Input = RunCodeToolInput;
    type Output = String;

    const NAME: &'static str = "run_code";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Execute
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) => format!("Run {} code", input.language).into(),
            Err(_) => "Run code".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| error.to_string())?;
            if !cx.update(|cx| crate::full_access_enabled(cx)) {
                return Err(
                    "Unleashed is disabled. Enable it in Settings → AI → Unleashed (or set agent.full_access.enabled).".into(),
                );
            }
            if input.code.trim().is_empty() {
                return Err("run_code requires non-empty source code.".into());
            }

            let (program, args, suffix) = interpreter(&input.language)?;
            let mut script = tempfile::Builder::new()
                .prefix("lead-agent-")
                .suffix(suffix)
                .tempfile()
                .map_err(|error| error.to_string())?;
            script
                .write_all(input.code.as_bytes())
                .map_err(|error| error.to_string())?;
            script.flush().map_err(|error| error.to_string())?;

            let script_path = script.path().to_string_lossy().into_owned();
            let quoted_script = ShellKind::system()
                .try_quote(&script_path)
                .ok_or_else(|| "Unable to quote temporary script path for the system shell.".to_string())?;
            let command = format!("{program} {} {quoted_script}", args.join(" "));
            let requested_cd = input.cd.as_deref();

            let mut working_dir = cx.update(|cx| -> Result<Option<std::path::PathBuf>, String> {
                if let Some(cd) = requested_cd {
                    super::terminal_tool::working_dir(cd, &self.project, cx)
                        .map_err(|error| error.to_string())
                } else {
                    Ok(self.project
                        .read(cx)
                        .worktrees(cx)
                        .next()
                        .map(|worktree| worktree.read(cx).abs_path().to_path_buf()))
                }
            })?;
            if let Some(path) = working_dir.as_ref() {
                let fs = self
                    .project
                    .read_with(cx, |project, _cx| project.fs().clone());
                working_dir = Some(crate::canonicalize_for_access(path, fs.as_ref()).await?);
            }

            let (authorize, escape_authorize) = cx.update(|cx| {
                let authorize = event_stream.authorize(
                    format!("Run {} code", input.language),
                    ToolPermissionContext::new(
                        Self::NAME,
                        vec![format!("{}:{}", input.language, input.code)],
                    ),
                    cx,
                );
                let escape_authorize = working_dir
                    .as_ref()
                    .map(|working_dir| {
                        crate::escape_gate(
                            &self.project,
                            std::slice::from_ref(working_dir),
                            Self::NAME,
                            cx,
                        )
                    })
                    .transpose()?
                    .flatten()
                    .map(|context| {
                        event_stream.authorize(
                            format!(
                                "Run code outside project: {}",
                                requested_cd.unwrap_or("<default>")
                            ),
                            context,
                            cx,
                        )
                    });
                Ok::<_, String>((authorize, escape_authorize))
            })?;

            authorize.await.map_err(|error| error.to_string())?;
            if let Some(authorize) = escape_authorize {
                authorize.await.map_err(|error| error.to_string())?;
            }

            let terminal = self
                .environment
                .create_terminal(
                    command,
                    Vec::new(),
                    working_dir,
                    Some(OUTPUT_LIMIT),
                    None,
                    cx,
                )
                .await
                .map_err(|error| error.to_string())?;
            let terminal_id = terminal.id(cx).map_err(|error| error.to_string())?;
            event_stream.update_fields(acp::ToolCallUpdateFields::new().content(vec![
                acp::ToolCallContent::Terminal(acp::Terminal::new(terminal_id)),
            ]));

            let wait_for_exit = terminal.wait_for_exit(cx).map_err(|error| error.to_string())?;
            let timeout = Duration::from_millis(
                input
                    .timeout_ms
                    .unwrap_or(DEFAULT_TIMEOUT_MS)
                    .min(MAX_TIMEOUT_MS),
            );
            let timer = cx.background_executor().timer(timeout);
            futures::select! {
                _ = wait_for_exit.clone().fuse() => {}
                _ = timer.fuse() => {
                    terminal.kill(cx).map_err(|error| error.to_string())?;
                    wait_for_exit.await;
                    return Err(format!("Code execution timed out after {} ms.", timeout.as_millis()));
                }
                _ = event_stream.cancelled_by_user().fuse() => {
                    terminal.kill(cx).map_err(|error| error.to_string())?;
                    wait_for_exit.await;
                    return Err("Code execution cancelled by user.".into());
                }
            }

            // Keep the temporary file alive until the interpreter exits.
            drop(script);
            let output = terminal.current_output(cx).map_err(|error| error.to_string())?;
            let exit_code = output.exit_status.as_ref().and_then(|status| status.exit_code);
            let mut text = output.output;
            if output.truncated {
                text.push_str("\n[output truncated]");
            }
            if exit_code == Some(0) {
                Ok(text)
            } else {
                Err(format!(
                    "Code interpreter exited with status {:?}.\n{}",
                    exit_code, text
                ))
            }
        })
    }
}

fn interpreter(language: &str) -> Result<(&'static str, Vec<&'static str>, &'static str), String> {
    match language.trim().to_ascii_lowercase().as_str() {
        "python" | "python3" => Ok(("python", vec![], ".py")),
        "node" | "javascript" | "js" => Ok(("node", vec![], ".js")),
        "powershell" | "pwsh" => Ok((
            if cfg!(windows) { "powershell" } else { "pwsh" },
            vec!["-NoProfile", "-NonInteractive", "-File"],
            ".ps1",
        )),
        "bash" | "shell" => Ok(("bash", vec![], ".sh")),
        _ => Err("Unsupported language. Use python, node, powershell, or bash.".into()),
    }
}
