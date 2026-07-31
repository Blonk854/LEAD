use agent_settings::AgentSettings;
use gpui::App;
use settings::Settings;

use crate::network_agent::resolve_model_selection;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelHealth {
    Unknown,
    Ready,
    Warning(String),
    Unavailable(String),
}

impl ModelHealth {
    pub fn label(&self) -> &str {
        match self {
            Self::Unknown => "Unknown",
            Self::Ready => "Ready",
            Self::Warning(message) | Self::Unavailable(message) => message,
        }
    }
}

/// Registry resolution check for orchestrator and local worker models.
pub fn check_hybrid_models_in_registry(cx: &mut App) -> (ModelHealth, ModelHealth) {
    let settings = AgentSettings::get_global(cx).clone();

    let orchestrator = match settings.effective_default_model() {
        Some(selection) => {
            if resolve_model_selection(&selection.provider.0, &selection.model, cx).is_some() {
                ModelHealth::Ready
            } else {
                ModelHealth::Unavailable(
                    "Network orchestrator model not available — check Configure Network Agent"
                        .into(),
                )
            }
        }
        None => ModelHealth::Unavailable("Network orchestrator not configured".into()),
    };

    let local = if !settings.network_agent_active() {
        ModelHealth::Unknown
    } else if settings
        .default_model
        .as_ref()
        .is_some_and(|model| model.provider.0 != "lmstudio")
    {
        ModelHealth::Warning(
            "Local worker is not LM Studio — hybrid mode works best with a local LM Studio model"
                .into(),
        )
    } else {
        match settings.effective_subagent_model() {
            Some(selection) => {
                if resolve_model_selection(&selection.provider.0, &selection.model, cx).is_some() {
                    ModelHealth::Ready
                } else {
                    ModelHealth::Unavailable(format!(
                        "Local worker `{}/{}` unavailable — start LM Studio",
                        selection.provider.0, selection.model
                    ))
                }
            }
            None => ModelHealth::Unavailable(
                "Set agent.default_model to your LM Studio worker model".into(),
            ),
        }
    };

    (orchestrator, local)
}
