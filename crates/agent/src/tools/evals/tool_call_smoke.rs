//! Fast smoke evals for native tool calling across providers/models.
//!
//! Run one model:
//!   ZED_AGENT_MODEL=lmstudio/opus4.7-gods.ghost.codex-4b.gguf cargo nextest run -p agent --features unit-eval eval_tool_call_smoke --no-capture
//!
//! Run the full local matrix:
//!   ./script/run-local-tool-eval-matrix

use super::model::load_eval_model;
use crate::{ToolCallSmokeOptions, run_tool_call_smoke};
use futures::FutureExt as _;

fn run_smoke_matrix() -> eval_utils::EvalOutput<()> {
    super::run_gpui_eval(
        |cx| {
            async move {
                let model = load_eval_model(cx).await;
                let mut async_cx = cx.to_async();
                let report =
                    run_tool_call_smoke(model, &mut async_cx, ToolCallSmokeOptions::default())
                        .await?;
                let summary = report.summary();
                if !report.passed() {
                    anyhow::bail!("{summary}");
                }
                Ok(summary)
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
