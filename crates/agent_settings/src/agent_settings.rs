mod agent_profile;
mod user_agents_md;

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};

use collections::{HashSet, IndexMap};
use fs::Fs;
use futures::channel::oneshot;
use gpui::{App, Pixels, SharedString, px};
use language_model::LanguageModel;
use project::DisableAiSettings;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use settings::{
    DockPosition, DockSide, LanguageModelParameters, LanguageModelProviderSetting,
    LanguageModelSelection, NotifyWhenAgentWaiting, PlaySoundWhenAgentDone, RegisterSetting,
    Settings, SettingsContent, SettingsStore, SidebarDockPosition, SidebarSide,
    ThinkingBlockDisplay, ToolPermissionMode, update_settings_file,
    update_settings_file_with_completion,
};

pub use crate::agent_profile::*;
pub use crate::user_agents_md::{UserAgentsMd, UserAgentsMdState, init as init_user_agents_md};

pub const SUMMARIZE_THREAD_PROMPT: &str = include_str!("prompts/summarize_thread_prompt.txt");
pub const SUMMARIZE_THREAD_DETAILED_PROMPT: &str =
    include_str!("prompts/summarize_thread_detailed_prompt.txt");
pub const COMPACTION_PROMPT: &str = include_str!("prompts/compaction_prompt.txt");
pub const GOAL_CHECKPOINT_PROMPT: &str = include_str!("prompts/goal_checkpoint_prompt.txt");
pub const HANDOFF_PROMPT: &str = include_str!("prompts/handoff_prompt.txt");
pub const JOURNAL_FACT_EXTRACT_PROMPT: &str =
    include_str!("prompts/journal_fact_extract_prompt.txt");

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PanelLayout {
    pub(crate) agent_dock: Option<DockPosition>,
    pub(crate) project_panel_dock: Option<DockSide>,
    pub(crate) outline_panel_dock: Option<DockSide>,
    pub(crate) collaboration_panel_dock: Option<DockPosition>,
    pub(crate) git_panel_dock: Option<DockPosition>,
}

impl PanelLayout {
    const AGENT: Self = Self {
        agent_dock: Some(DockPosition::Left),
        project_panel_dock: Some(DockSide::Right),
        outline_panel_dock: Some(DockSide::Right),
        collaboration_panel_dock: Some(DockPosition::Right),
        git_panel_dock: Some(DockPosition::Right),
    };

    const EDITOR: Self = Self {
        agent_dock: Some(DockPosition::Right),
        project_panel_dock: Some(DockSide::Left),
        outline_panel_dock: Some(DockSide::Left),
        collaboration_panel_dock: Some(DockPosition::Left),
        git_panel_dock: Some(DockPosition::Left),
    };

    pub fn is_agent_layout(&self) -> bool {
        *self == Self::AGENT
    }

    pub fn is_editor_layout(&self) -> bool {
        *self == Self::EDITOR
    }

    fn read_from(content: &SettingsContent) -> Self {
        Self {
            agent_dock: content.agent.as_ref().and_then(|a| a.dock),
            project_panel_dock: content.project_panel.as_ref().and_then(|p| p.dock),
            outline_panel_dock: content.outline_panel.as_ref().and_then(|p| p.dock),
            collaboration_panel_dock: content.collaboration_panel.as_ref().and_then(|p| p.dock),
            git_panel_dock: content.git_panel.as_ref().and_then(|p| p.dock),
        }
    }

    fn write_to(&self, settings: &mut SettingsContent) {
        settings.agent.get_or_insert_default().dock = self.agent_dock;
        settings.project_panel.get_or_insert_default().dock = self.project_panel_dock;
        settings.outline_panel.get_or_insert_default().dock = self.outline_panel_dock;
        settings.collaboration_panel.get_or_insert_default().dock = self.collaboration_panel_dock;
        settings.git_panel.get_or_insert_default().dock = self.git_panel_dock;
    }

    fn write_diff_to(&self, current_merged: &PanelLayout, settings: &mut SettingsContent) {
        if self.agent_dock != current_merged.agent_dock {
            settings.agent.get_or_insert_default().dock = self.agent_dock;
        }
        if self.project_panel_dock != current_merged.project_panel_dock {
            settings.project_panel.get_or_insert_default().dock = self.project_panel_dock;
        }
        if self.outline_panel_dock != current_merged.outline_panel_dock {
            settings.outline_panel.get_or_insert_default().dock = self.outline_panel_dock;
        }
        if self.collaboration_panel_dock != current_merged.collaboration_panel_dock {
            settings.collaboration_panel.get_or_insert_default().dock =
                self.collaboration_panel_dock;
        }
        if self.git_panel_dock != current_merged.git_panel_dock {
            settings.git_panel.get_or_insert_default().dock = self.git_panel_dock;
        }
    }

    fn backfill_to(&self, user_layout: &PanelLayout, settings: &mut SettingsContent) {
        if user_layout.agent_dock.is_none() {
            settings.agent.get_or_insert_default().dock = self.agent_dock;
        }
        if user_layout.project_panel_dock.is_none() {
            settings.project_panel.get_or_insert_default().dock = self.project_panel_dock;
        }
        if user_layout.outline_panel_dock.is_none() {
            settings.outline_panel.get_or_insert_default().dock = self.outline_panel_dock;
        }
        if user_layout.collaboration_panel_dock.is_none() {
            settings.collaboration_panel.get_or_insert_default().dock =
                self.collaboration_panel_dock;
        }
        if user_layout.git_panel_dock.is_none() {
            settings.git_panel.get_or_insert_default().dock = self.git_panel_dock;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowLayout {
    Editor(Option<PanelLayout>),
    Agent(Option<PanelLayout>),
    Custom(PanelLayout),
}

impl WindowLayout {
    pub fn agent() -> Self {
        Self::Agent(None)
    }

    pub fn editor() -> Self {
        Self::Editor(None)
    }
}

#[derive(Clone, Debug, RegisterSetting)]
pub struct AgentSettings {
    pub enabled: bool,
    pub button: bool,
    pub dock: DockPosition,
    pub starts_open: bool,
    pub flexible: bool,
    pub sidebar_side: SidebarDockPosition,
    pub default_width: Pixels,
    pub default_height: Pixels,
    pub max_content_width: Option<Pixels>,
    pub default_model: Option<LanguageModelSelection>,
    pub subagent_model: Option<LanguageModelSelection>,
    pub inline_assistant_model: Option<LanguageModelSelection>,
    pub inline_assistant_use_streaming_tools: bool,
    pub commit_message_model: Option<LanguageModelSelection>,
    pub commit_message_instructions: Option<String>,
    pub thread_summary_model: Option<LanguageModelSelection>,
    pub inline_alternatives: Vec<LanguageModelSelection>,
    pub favorite_models: Vec<LanguageModelSelection>,
    pub default_profile: AgentProfileId,
    pub profiles: IndexMap<AgentProfileId, AgentProfileSettings>,

    pub notify_when_agent_waiting: NotifyWhenAgentWaiting,
    pub play_sound_when_agent_done: PlaySoundWhenAgentDone,
    pub single_file_review: bool,
    pub model_parameters: Vec<LanguageModelParameters>,
    pub enable_feedback: bool,
    pub expand_edit_card: bool,
    pub expand_terminal_card: bool,
    pub thinking_display: ThinkingBlockDisplay,
    pub cancel_generation_on_terminal_stop: bool,
    pub use_modifier_to_send: bool,
    pub message_editor_min_lines: usize,
    pub show_turn_stats: bool,
    pub show_merge_conflict_indicator: bool,
    pub tool_permissions: ToolPermissions,
    pub sandbox_permissions: SandboxPermissions,
    pub full_access: FullAccessSettings,
    pub network_agent: NetworkAgentSettings,
    pub auto_thread_rollover: AutoThreadRollover,
    pub anti_loop: AntiLoopSettings,
    pub web_research: WebResearchSettings,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WebResearchSettings {
    pub enabled: bool,
    pub preferred_provider: String,
    pub max_search_results: usize,
    pub max_snippet_chars: usize,
    pub max_fetch_chars: usize,
    pub max_response_bytes: u64,
    pub max_redirects: u32,
    pub request_timeout_ms: u64,
    pub max_concurrency: usize,
    pub per_host_delay_ms: u64,
    pub browser_fallback_enabled: bool,
    pub browser_timeout_secs: u64,
    pub max_pages: usize,
    pub max_depth: usize,
    pub research_wall_clock_secs: u64,
    pub cache_ttl_serp_secs: u64,
    pub cache_ttl_page_secs: u64,
}

impl Default for WebResearchSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            preferred_provider: "local".into(),
            max_search_results: 5,
            max_snippet_chars: 300,
            max_fetch_chars: 12_000,
            max_response_bytes: 1024 * 1024,
            max_redirects: 5,
            request_timeout_ms: 15_000,
            max_concurrency: 2,
            per_host_delay_ms: 1_000,
            browser_fallback_enabled: false,
            browser_timeout_secs: 45,
            max_pages: 6,
            max_depth: 1,
            research_wall_clock_secs: 90,
            cache_ttl_serp_secs: 3_600,
            cache_ttl_page_secs: 604_800,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AntiLoopSettings {
    pub enabled: bool,
    pub max_recoveries_per_turn: usize,
    pub min_block_chars: usize,
    pub max_block_chars: usize,
    pub min_repeats: usize,
    pub min_text_chars_before_trip: usize,
    pub watch_thinking: bool,
    pub thinking_min_block_chars: usize,
    pub thinking_max_block_chars: usize,
    pub thinking_min_repeats: usize,
    pub thinking_min_text_chars_before_trip: usize,
}

impl Default for AntiLoopSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_recoveries_per_turn: 1,
            min_block_chars: 64,
            max_block_chars: 1_024,
            min_repeats: 3,
            min_text_chars_before_trip: 256,
            watch_thinking: true,
            thinking_min_block_chars: 128,
            thinking_max_block_chars: 1_024,
            thinking_min_repeats: 4,
            thinking_min_text_chars_before_trip: 512,
        }
    }
}

/// Resolved configuration for automatic thread rollover and hand-off.
///
/// When [`enabled`](Self::enabled) and the active thread's context exceeds
/// the effective share of the model's window (the configured
/// [`context_fraction`](Self::context_fraction) floor, raised above
/// auto-compaction when needed), LEAD rolls the conversation into a fresh
/// thread seeded with a hand-off summary.
#[derive(Clone, Debug, PartialEq)]
pub struct AutoThreadRollover {
    pub enabled: bool,
    pub context_fraction: f32,
}

impl Default for AutoThreadRollover {
    fn default() -> Self {
        Self {
            enabled: true,
            // Floor preference; runtime raises this above auto-compaction via
            // `effective_rollover_context_fraction`. Keep in sync with
            // assets/settings/default.json.
            context_fraction: 0.5,
        }
    }
}

/// Resolved configuration for the optional "Network Agent" mode.
///
/// When [`enabled`](Self::enabled) and a [`model`](Self::model) is configured,
/// a model served from a network endpoint drives the main agent while the
/// local `default_model` handles delegated subagent work.
/// How the network orchestrator delegates work to the local worker model.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NetworkAgentDelegationMode {
    /// Hide heavy execution tools from the main thread so the network model
    /// must delegate via `spawn_agent`.
    #[default]
    Balanced,
    /// Prompt-only guidance; main thread keeps all profile tools.
    Manual,
}

impl NetworkAgentDelegationMode {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Self::Manual,
            _ => Self::Balanced,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct NetworkAgentSettings {
    pub enabled: bool,
    pub model: Option<String>,
    pub delegation_mode: NetworkAgentDelegationMode,
    pub workers: NetworkAgentWorkers,
}

/// How hybrid subagent work is scheduled across worker endpoints.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkerScheduling {
    /// Prefer a worker with spare capacity; fall back to least-loaded.
    Pool,
    RoundRobin,
    #[default]
    LeastBusy,
}

impl WorkerScheduling {
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "pool" => Self::Pool,
            "round_robin" | "round-robin" => Self::RoundRobin,
            _ => Self::LeastBusy,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pool => "pool",
            Self::RoundRobin => "round_robin",
            Self::LeastBusy => "least_busy",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkAgentWorkerEndpoint {
    pub id: String,
    pub provider: LanguageModelProviderSetting,
    pub model: String,
    pub max_concurrent: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NetworkAgentWorkers {
    pub scheduling: WorkerScheduling,
    pub max_workers: u32,
    pub include_default_model: bool,
    pub endpoints: Vec<NetworkAgentWorkerEndpoint>,
}

impl Default for NetworkAgentWorkers {
    fn default() -> Self {
        Self {
            scheduling: WorkerScheduling::default(),
            max_workers: 10,
            include_default_model: true,
            endpoints: Vec::new(),
        }
    }
}

impl NetworkAgentWorkers {
    /// True when at least one remote worker endpoint is configured.
    pub fn is_active(&self) -> bool {
        self.endpoints.iter().any(|endpoint| endpoint.enabled)
    }

    /// Enabled endpoints capped at [`max_workers`], optionally including the default model.
    pub fn resolved_endpoints(
        &self,
        default_model: &Option<LanguageModelSelection>,
    ) -> Vec<NetworkAgentWorkerEndpoint> {
        let mut endpoints: Vec<NetworkAgentWorkerEndpoint> = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.enabled)
            .cloned()
            .collect();

        if self.include_default_model
            && let Some(default_model) = default_model
        {
            endpoints.insert(
                0,
                NetworkAgentWorkerEndpoint {
                    id: "default".into(),
                    provider: default_model.provider.clone(),
                    model: default_model.model.clone(),
                    max_concurrent: 1,
                    enabled: true,
                },
            );
        }

        endpoints.truncate(self.max_workers.max(1).min(10) as usize);
        endpoints
    }
}

impl AgentSettings {
    pub fn enabled(&self, cx: &App) -> bool {
        self.enabled && !DisableAiSettings::get_global(cx).disable_ai
    }

    pub fn temperature_for_model(model: &Arc<dyn LanguageModel>, cx: &App) -> Option<f32> {
        let settings = Self::get_global(cx);
        for setting in settings.model_parameters.iter().rev() {
            if let Some(provider) = &setting.provider
                && provider.0 != model.provider_id().0
            {
                continue;
            }
            if let Some(setting_model) = &setting.model
                && *setting_model != model.id().0
            {
                continue;
            }
            return setting.temperature;
        }
        return None;
    }

    pub fn sidebar_side(&self) -> SidebarSide {
        match self.sidebar_side {
            SidebarDockPosition::Left => SidebarSide::Left,
            SidebarDockPosition::Right => SidebarSide::Right,
        }
    }

    pub fn set_message_editor_max_lines(&self) -> usize {
        self.message_editor_min_lines * 2
    }

    pub fn favorite_model_ids(&self) -> HashSet<SharedString> {
        self.favorite_models
            .iter()
            .map(|sel| SharedString::from(format!("{}/{}", sel.provider.0, sel.model)))
            .collect()
    }

    /// The provider id used for the Network Agent's OpenAI-compatible endpoint.
    pub const NETWORK_AGENT_PROVIDER_ID: &'static str = "network-agent";

    /// True when the Network Agent toggle is on and a network model is configured.
    pub fn network_agent_active(&self) -> bool {
        self.network_agent.enabled && self.network_agent.model.is_some()
    }

    /// Whether guard-railed whole-PC native tools may be exposed.
    pub fn full_access_enabled(&self) -> bool {
        self.full_access.enabled
    }

    pub fn network_agent_delegation_mode(&self) -> NetworkAgentDelegationMode {
        self.network_agent.delegation_mode
    }

    /// Label for the local worker model used by subagents in hybrid mode.
    pub fn local_worker_model_label(&self) -> Option<String> {
        self.default_model
            .as_ref()
            .map(|selection| format!("{}/{}", selection.provider.0, selection.model))
    }

    /// Label for hybrid worker pool members, or the single local worker when no pool is configured.
    pub fn hybrid_worker_model_label(&self) -> Option<String> {
        if self.network_agent_active() && self.network_agent.workers.is_active() {
            let labels: Vec<String> = self
                .network_agent
                .workers
                .resolved_endpoints(&self.default_model)
                .iter()
                .map(|endpoint| format!("{}/{}", endpoint.provider.0, endpoint.model))
                .collect();
            if labels.is_empty() {
                None
            } else {
                Some(labels.join(", "))
            }
        } else {
            self.local_worker_model_label()
        }
    }

    /// The model selection that should drive the main agent, accounting for the
    /// Network Agent toggle. When active, this is the network-served model.
    pub fn effective_default_model(&self) -> Option<LanguageModelSelection> {
        if let Some(model) = self
            .network_agent
            .model
            .clone()
            .filter(|_| self.network_agent.enabled)
        {
            Some(LanguageModelSelection {
                provider: LanguageModelProviderSetting(Self::NETWORK_AGENT_PROVIDER_ID.into()),
                model,
                enable_thinking: false,
                effort: None,
                speed: None,
            })
        } else {
            self.default_model.clone()
        }
    }

    /// The model selection subagents should use. When the Network Agent is
    /// active, subagents run on the local `default_model` (the heavy lifting),
    /// otherwise the configured `subagent_model` is used.
    pub fn effective_subagent_model(&self) -> Option<LanguageModelSelection> {
        if self.network_agent_active() {
            self.default_model.clone()
        } else {
            self.subagent_model.clone()
        }
    }
}

pub fn language_model_to_selection(
    model: &Arc<dyn LanguageModel>,
    override_selection: Option<&LanguageModelSelection>,
) -> LanguageModelSelection {
    let provider = model.provider_id().0.to_string().into();
    let model_name = model.id().0.to_string();
    match override_selection {
        Some(current) => LanguageModelSelection {
            provider,
            model: model_name,
            enable_thinking: current.enable_thinking && model.supports_thinking(),
            effort: current
                .effort
                .clone()
                .filter(|value| {
                    model
                        .supported_effort_levels()
                        .iter()
                        .any(|level| level.value.as_ref() == value.as_str())
                })
                .or_else(|| {
                    model
                        .default_effort_level()
                        .map(|effort| effort.value.to_string())
                }),
            speed: current.speed.filter(|_| model.supports_fast_mode()),
        },
        None => LanguageModelSelection {
            provider,
            model: model_name,
            enable_thinking: model.supports_thinking(),
            effort: model
                .default_effort_level()
                .map(|effort| effort.value.to_string()),
            speed: None,
        },
    }
}

impl AgentSettings {
    pub fn get_layout(cx: &App) -> WindowLayout {
        let store = cx.global::<SettingsStore>();
        let merged = store.merged_settings();
        let user_layout = store
            .raw_user_settings()
            .map(|u| PanelLayout::read_from(u.content.as_ref()))
            .unwrap_or_default();
        let merged_layout = PanelLayout::read_from(merged);

        if merged_layout.is_agent_layout() {
            return WindowLayout::Agent(Some(user_layout));
        }

        if merged_layout.is_editor_layout() {
            return WindowLayout::Editor(Some(user_layout));
        }

        WindowLayout::Custom(user_layout)
    }

    pub fn backfill_editor_layout(fs: Arc<dyn Fs>, cx: &App) {
        let user_layout = cx
            .global::<SettingsStore>()
            .raw_user_settings()
            .map(|u| PanelLayout::read_from(u.content.as_ref()))
            .unwrap_or_default();

        update_settings_file(fs, cx, move |settings, _cx| {
            PanelLayout::EDITOR.backfill_to(&user_layout, settings);
        });
    }

    pub fn set_layout(
        layout: WindowLayout,
        fs: Arc<dyn Fs>,
        cx: &App,
    ) -> oneshot::Receiver<anyhow::Result<()>> {
        let merged = PanelLayout::read_from(cx.global::<SettingsStore>().merged_settings());

        match layout {
            WindowLayout::Agent(None) => {
                update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                    PanelLayout::AGENT.write_diff_to(&merged, settings);
                })
            }
            WindowLayout::Editor(None) => {
                update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                    PanelLayout::EDITOR.write_diff_to(&merged, settings);
                })
            }
            WindowLayout::Agent(Some(saved))
            | WindowLayout::Editor(Some(saved))
            | WindowLayout::Custom(saved) => {
                update_settings_file_with_completion(fs, cx, move |settings, _cx| {
                    saved.write_to(settings);
                })
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AgentProfileId(pub Arc<str>);

impl AgentProfileId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentProfileId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Default for AgentProfileId {
    fn default() -> Self {
        Self("write".into())
    }
}

/// Persistent "allow always" sandbox grants for agent-run terminal commands.
///
/// Coverage decisions for these grants are made in
/// `agent::sandboxing::ThreadSandboxGrants::covers_with_persistent`, which
/// combines them with the in-memory per-thread grants. `write_paths` are
/// stored as minimal, lexically-normalized subtrees (see
/// [`compile_sandbox_permissions`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SandboxPermissions {
    pub allow_network: bool,
    pub allow_fs_write_all: bool,
    pub allow_unsandboxed: bool,
    pub write_paths: Vec<PathBuf>,
}

/// Guard-railed access to resources outside open project worktrees.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FullAccessSettings {
    pub enabled: bool,
    pub allowed_roots: Vec<PathBuf>,
    pub denied_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub struct ToolPermissions {
    /// Global default permission when no tool-specific rules or patterns match.
    pub default: ToolPermissionMode,
    pub tools: collections::HashMap<Arc<str>, ToolRules>,
}

impl ToolPermissions {
    /// Returns all invalid regex patterns across all tools.
    pub fn invalid_patterns(&self) -> Vec<&InvalidRegexPattern> {
        self.tools
            .values()
            .flat_map(|rules| rules.invalid_patterns.iter())
            .collect()
    }

    /// Returns true if any tool has invalid regex patterns.
    pub fn has_invalid_patterns(&self) -> bool {
        self.tools
            .values()
            .any(|rules| !rules.invalid_patterns.is_empty())
    }
}

/// Represents a regex pattern that failed to compile.
#[derive(Clone, Debug)]
pub struct InvalidRegexPattern {
    /// The pattern string that failed to compile.
    pub pattern: String,
    /// Which rule list this pattern was in (e.g., "always_deny", "always_allow", "always_confirm").
    pub rule_type: String,
    /// The error message from the regex compiler.
    pub error: String,
}

#[derive(Clone, Debug, Default)]
pub struct ToolRules {
    pub default: Option<ToolPermissionMode>,
    pub always_allow: Vec<CompiledRegex>,
    pub always_deny: Vec<CompiledRegex>,
    pub always_confirm: Vec<CompiledRegex>,
    /// Patterns that failed to compile. If non-empty, tool calls should be blocked.
    pub invalid_patterns: Vec<InvalidRegexPattern>,
}

#[derive(Clone)]
pub struct CompiledRegex {
    pub pattern: String,
    pub case_sensitive: bool,
    pub regex: regex::Regex,
}

impl std::fmt::Debug for CompiledRegex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompiledRegex")
            .field("pattern", &self.pattern)
            .field("case_sensitive", &self.case_sensitive)
            .finish()
    }
}

impl CompiledRegex {
    pub fn new(pattern: &str, case_sensitive: bool) -> Option<Self> {
        Self::try_new(pattern, case_sensitive).ok()
    }

    pub fn try_new(pattern: &str, case_sensitive: bool) -> Result<Self, regex::Error> {
        let regex = regex::RegexBuilder::new(pattern)
            .case_insensitive(!case_sensitive)
            .build()?;
        Ok(Self {
            pattern: pattern.to_string(),
            case_sensitive,
            regex,
        })
    }

    pub fn is_match(&self, input: &str) -> bool {
        self.regex.is_match(input)
    }
}

pub const HARDCODED_SECURITY_DENIAL_MESSAGE: &str = "Blocked by built-in security rule. This operation is considered too \
     harmful to be allowed, and cannot be overridden by settings.";

/// Security rules that are always enforced and cannot be overridden by any setting.
/// These protect against catastrophic operations like wiping filesystems.
pub struct HardcodedSecurityRules {
    pub terminal_deny: Vec<CompiledRegex>,
}

pub static HARDCODED_SECURITY_RULES: LazyLock<HardcodedSecurityRules> = LazyLock::new(|| {
    const FLAGS: &str = r"(--[a-zA-Z0-9][-a-zA-Z0-9_]*(=[^\s]*)?\s+|-[a-zA-Z]+\s+)*";
    const TRAILING_FLAGS: &str = r"(\s+--[a-zA-Z0-9][-a-zA-Z0-9_]*(=[^\s]*)?|\s+-[a-zA-Z]+)*\s*";

    HardcodedSecurityRules {
        terminal_deny: vec![
            // Recursive deletion of root - "rm -rf /", "rm -rf /*"
            CompiledRegex::new(
                &format!(r"\brm\s+{FLAGS}(--\s+)?/\*?{TRAILING_FLAGS}$"),
                false,
            )
            .expect("hardcoded regex should compile"),
            // Recursive deletion of home via tilde - "rm -rf ~", "rm -rf ~/"
            CompiledRegex::new(
                &format!(r"\brm\s+{FLAGS}(--\s+)?~/?\*?{TRAILING_FLAGS}$"),
                false,
            )
            .expect("hardcoded regex should compile"),
            // Recursive deletion of home via env var - "rm -rf $HOME", "rm -rf ${HOME}"
            CompiledRegex::new(
                &format!(r"\brm\s+{FLAGS}(--\s+)?(\$HOME|\$\{{HOME\}})/?(\*)?{TRAILING_FLAGS}$"),
                false,
            )
            .expect("hardcoded regex should compile"),
            // Recursive deletion of current directory - "rm -rf .", "rm -rf ./"
            CompiledRegex::new(
                &format!(r"\brm\s+{FLAGS}(--\s+)?\./?\*?{TRAILING_FLAGS}$"),
                false,
            )
            .expect("hardcoded regex should compile"),
            // Recursive deletion of parent directory - "rm -rf ..", "rm -rf ../"
            CompiledRegex::new(
                &format!(r"\brm\s+{FLAGS}(--\s+)?\.\./?\*?{TRAILING_FLAGS}$"),
                false,
            )
            .expect("hardcoded regex should compile"),
        ],
    }
});

/// Checks if input matches any hardcoded security rules that cannot be bypassed.
/// Returns the denial reason string if blocked, None otherwise.
///
/// `terminal_tool_name` should be the tool name used for the terminal tool
/// (e.g. `"terminal"`). `extracted_commands` can optionally provide parsed
/// sub-commands for chained command checking; callers with access to a shell
/// parser should extract sub-commands and pass them here.
pub fn check_hardcoded_security_rules(
    tool_name: &str,
    terminal_tool_name: &str,
    input: &str,
    extracted_commands: Option<&[String]>,
) -> Option<String> {
    if tool_name != terminal_tool_name {
        return None;
    }

    let rules = &*HARDCODED_SECURITY_RULES;
    let terminal_patterns = &rules.terminal_deny;

    if matches_hardcoded_patterns(input, terminal_patterns) {
        return Some(HARDCODED_SECURITY_DENIAL_MESSAGE.into());
    }

    if let Some(commands) = extracted_commands {
        for command in commands {
            if matches_hardcoded_patterns(command, terminal_patterns) {
                return Some(HARDCODED_SECURITY_DENIAL_MESSAGE.into());
            }
        }
    }

    None
}

fn matches_hardcoded_patterns(command: &str, patterns: &[CompiledRegex]) -> bool {
    for pattern in patterns {
        if pattern.is_match(command) {
            return true;
        }
    }

    for expanded in expand_rm_to_single_path_commands(command) {
        for pattern in patterns {
            if pattern.is_match(&expanded) {
                return true;
            }
        }
    }

    false
}

fn expand_rm_to_single_path_commands(command: &str) -> Vec<String> {
    let trimmed = command.trim();

    let first_token = trimmed.split_whitespace().next();
    if !first_token.is_some_and(|t| t.eq_ignore_ascii_case("rm")) {
        return vec![];
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let mut flags = Vec::new();
    let mut paths = Vec::new();
    let mut past_double_dash = false;

    for part in parts.iter().skip(1) {
        if !past_double_dash && *part == "--" {
            past_double_dash = true;
            flags.push(*part);
            continue;
        }
        if !past_double_dash && part.starts_with('-') {
            flags.push(*part);
        } else {
            paths.push(*part);
        }
    }

    let flags_str = if flags.is_empty() {
        String::new()
    } else {
        format!("{} ", flags.join(" "))
    };

    let mut results = Vec::new();
    for path in &paths {
        if path.starts_with('$') {
            let home_prefix = if path.starts_with("${HOME}") {
                Some("${HOME}")
            } else if path.starts_with("$HOME") {
                Some("$HOME")
            } else {
                None
            };

            if let Some(prefix) = home_prefix {
                let suffix = &path[prefix.len()..];
                if suffix.is_empty() {
                    results.push(format!("rm {flags_str}{path}"));
                } else if suffix.starts_with('/') {
                    let normalized_suffix = normalize_path(suffix);
                    let reconstructed = if normalized_suffix == "/" {
                        prefix.to_string()
                    } else {
                        format!("{prefix}{normalized_suffix}")
                    };
                    results.push(format!("rm {flags_str}{reconstructed}"));
                } else {
                    results.push(format!("rm {flags_str}{path}"));
                }
            } else {
                results.push(format!("rm {flags_str}{path}"));
            }
            continue;
        }

        let mut normalized = normalize_path(path);
        if normalized.is_empty() && !Path::new(path).has_root() {
            normalized = ".".to_string();
        }

        results.push(format!("rm {flags_str}{normalized}"));
    }

    results
}

pub fn normalize_path(raw: &str) -> String {
    let is_absolute = Path::new(raw).has_root();
    let mut components: Vec<&str> = Vec::new();
    for component in Path::new(raw).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if components.last() == Some(&"..") {
                    components.push("..");
                } else if !components.is_empty() {
                    components.pop();
                } else if !is_absolute {
                    components.push("..");
                }
            }
            Component::Normal(segment) => {
                if let Some(s) = segment.to_str() {
                    components.push(s);
                }
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    let joined = components.join("/");
    if is_absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

impl Settings for AgentSettings {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        let agent = content.agent.clone().unwrap();
        Self {
            enabled: agent.enabled.unwrap(),
            button: agent.button.unwrap(),
            dock: agent.dock.unwrap(),
            starts_open: agent.starts_open.unwrap(),
            sidebar_side: agent.sidebar_side.unwrap(),
            default_width: px(agent.default_width.unwrap()),
            default_height: px(agent.default_height.unwrap()),
            max_content_width: if agent.limit_content_width.unwrap() {
                Some(px(agent.max_content_width.unwrap()))
            } else {
                None
            },
            flexible: agent.flexible.unwrap(),
            default_model: Some(agent.default_model.unwrap()),
            subagent_model: agent.subagent_model,
            inline_assistant_model: agent.inline_assistant_model,
            inline_assistant_use_streaming_tools: agent
                .inline_assistant_use_streaming_tools
                .unwrap_or(true),
            commit_message_model: agent.commit_message_model,
            commit_message_instructions: agent.commit_message_instructions,
            thread_summary_model: agent.thread_summary_model,
            inline_alternatives: agent.inline_alternatives.unwrap_or_default(),
            favorite_models: agent.favorite_models,
            default_profile: AgentProfileId(agent.default_profile.unwrap()),
            profiles: agent
                .profiles
                .unwrap()
                .into_iter()
                .map(|(key, val)| (AgentProfileId(key), val.into()))
                .collect(),

            notify_when_agent_waiting: agent.notify_when_agent_waiting.unwrap(),
            play_sound_when_agent_done: agent.play_sound_when_agent_done.unwrap_or_default(),
            single_file_review: agent.single_file_review.unwrap(),
            model_parameters: agent.model_parameters,
            enable_feedback: agent.enable_feedback.unwrap(),
            expand_edit_card: agent.expand_edit_card.unwrap(),
            expand_terminal_card: agent.expand_terminal_card.unwrap(),
            thinking_display: agent.thinking_display.unwrap(),
            cancel_generation_on_terminal_stop: agent.cancel_generation_on_terminal_stop.unwrap(),
            use_modifier_to_send: agent.use_modifier_to_send.unwrap(),
            message_editor_min_lines: agent.message_editor_min_lines.unwrap(),
            show_turn_stats: agent.show_turn_stats.unwrap(),
            show_merge_conflict_indicator: agent.show_merge_conflict_indicator.unwrap(),
            tool_permissions: compile_tool_permissions(agent.tool_permissions),
            sandbox_permissions: compile_sandbox_permissions(agent.sandbox_permissions),
            full_access: compile_full_access_settings(agent.full_access),
            network_agent: agent
                .network_agent
                .map(|content| NetworkAgentSettings {
                    enabled: content.enabled.unwrap_or(false),
                    model: content.model.filter(|model| !model.is_empty()),
                    delegation_mode: content
                        .delegation_mode
                        .as_deref()
                        .map(NetworkAgentDelegationMode::parse)
                        .unwrap_or_default(),
                    workers: compile_network_agent_workers(content.workers),
                })
                .unwrap_or_default(),
            auto_thread_rollover: agent
                .auto_thread_rollover
                .map(|content| {
                    let defaults = AutoThreadRollover::default();
                    AutoThreadRollover {
                        enabled: content.enabled.unwrap_or(defaults.enabled),
                        context_fraction: content
                            .context_fraction
                            .map(|fraction| fraction.clamp(0.1, 0.95))
                            .unwrap_or(defaults.context_fraction),
                    }
                })
                .unwrap_or_default(),
            anti_loop: compile_anti_loop_settings(agent.anti_loop),
            web_research: compile_web_research_settings(agent.web_research),
        }
    }
}

fn compile_web_research_settings(
    content: Option<settings::WebResearchSettingsContent>,
) -> WebResearchSettings {
    let defaults = WebResearchSettings::default();
    let Some(content) = content else {
        return defaults;
    };
    WebResearchSettings {
        enabled: content.enabled.unwrap_or(defaults.enabled),
        preferred_provider: content
            .preferred_provider
            .map(|value| match value.trim().to_ascii_lowercase().as_str() {
                "cloud" => "cloud".to_string(),
                "auto" => "auto".to_string(),
                _ => "local".to_string(),
            })
            .unwrap_or(defaults.preferred_provider),
        max_search_results: content
            .max_search_results
            .filter(|value| (1..=20).contains(value))
            .unwrap_or(defaults.max_search_results),
        max_snippet_chars: content
            .max_snippet_chars
            .filter(|value| (80..=2_000).contains(value))
            .unwrap_or(defaults.max_snippet_chars),
        max_fetch_chars: content
            .max_fetch_chars
            .filter(|value| (1_000..=100_000).contains(value))
            .unwrap_or(defaults.max_fetch_chars),
        max_response_bytes: content
            .max_response_bytes
            .filter(|value| (16_384..=8 * 1024 * 1024).contains(value))
            .unwrap_or(defaults.max_response_bytes),
        max_redirects: content
            .max_redirects
            .filter(|value| (0..=20).contains(value))
            .unwrap_or(defaults.max_redirects),
        request_timeout_ms: content
            .request_timeout_ms
            .filter(|value| (1_000..=120_000).contains(value))
            .unwrap_or(defaults.request_timeout_ms),
        max_concurrency: content
            .max_concurrency
            .filter(|value| (1..=8).contains(value))
            .unwrap_or(defaults.max_concurrency),
        per_host_delay_ms: content
            .per_host_delay_ms
            .filter(|value| (0..=30_000).contains(value))
            .unwrap_or(defaults.per_host_delay_ms),
        browser_fallback_enabled: content
            .browser_fallback_enabled
            .unwrap_or(defaults.browser_fallback_enabled),
        browser_timeout_secs: content
            .browser_timeout_secs
            .filter(|value| (5..=180).contains(value))
            .unwrap_or(defaults.browser_timeout_secs),
        max_pages: content
            .max_pages
            .filter(|value| (1..=12).contains(value))
            .unwrap_or(defaults.max_pages),
        max_depth: content
            .max_depth
            .filter(|value| (0..=3).contains(value))
            .unwrap_or(defaults.max_depth),
        research_wall_clock_secs: content
            .research_wall_clock_secs
            .filter(|value| (15..=300).contains(value))
            .unwrap_or(defaults.research_wall_clock_secs),
        cache_ttl_serp_secs: content
            .cache_ttl_serp_secs
            .filter(|value| (60..=86_400).contains(value))
            .unwrap_or(defaults.cache_ttl_serp_secs),
        cache_ttl_page_secs: content
            .cache_ttl_page_secs
            .filter(|value| (300..=30 * 24 * 60 * 60).contains(value))
            .unwrap_or(defaults.cache_ttl_page_secs),
    }
}

fn compile_anti_loop_settings(
    content: Option<settings::AntiLoopSettingsContent>,
) -> AntiLoopSettings {
    // Keep this aligned with the detector's rolling window so resolved settings
    // never advertise a larger candidate block than the watchdog can inspect.
    const ROLLING_WINDOW_CHARS: usize = 16 * 1024;
    const MAX_BLOCK_CHARS_LIMIT: usize = 4_096;
    const MAX_RECOVERIES_LIMIT: usize = 3;
    const MAX_REPEATS_LIMIT: usize = 8;

    let defaults = AntiLoopSettings::default();
    let Some(content) = content else {
        return defaults;
    };

    let min_repeats = content
        .min_repeats
        .filter(|value| (2..=MAX_REPEATS_LIMIT).contains(value))
        .unwrap_or(defaults.min_repeats);
    let min_block_chars = content
        .min_block_chars
        .filter(|value| (16..=MAX_BLOCK_CHARS_LIMIT).contains(value))
        .unwrap_or(defaults.min_block_chars);
    let detector_max_block_chars = (ROLLING_WINDOW_CHARS / min_repeats).max(min_block_chars);
    let max_block_chars = content
        .max_block_chars
        .filter(|value| {
            (min_block_chars..=MAX_BLOCK_CHARS_LIMIT.min(detector_max_block_chars)).contains(value)
        })
        .unwrap_or_else(|| {
            defaults
                .max_block_chars
                .max(min_block_chars)
                .min(detector_max_block_chars)
        });
    let thinking_min_repeats = content
        .thinking_min_repeats
        .filter(|value| (2..=MAX_REPEATS_LIMIT).contains(value))
        .unwrap_or(defaults.thinking_min_repeats);
    let thinking_min_block_chars = content
        .thinking_min_block_chars
        .filter(|value| (16..=MAX_BLOCK_CHARS_LIMIT).contains(value))
        .unwrap_or(defaults.thinking_min_block_chars);
    let thinking_detector_max_block_chars =
        (ROLLING_WINDOW_CHARS / thinking_min_repeats).max(thinking_min_block_chars);
    let thinking_max_block_chars = content
        .thinking_max_block_chars
        .filter(|value| {
            (thinking_min_block_chars
                ..=MAX_BLOCK_CHARS_LIMIT.min(thinking_detector_max_block_chars))
                .contains(value)
        })
        .unwrap_or_else(|| {
            defaults
                .thinking_max_block_chars
                .max(thinking_min_block_chars)
                .min(thinking_detector_max_block_chars)
        });

    AntiLoopSettings {
        enabled: content.enabled.unwrap_or(defaults.enabled),
        max_recoveries_per_turn: content
            .max_recoveries_per_turn
            .filter(|value| *value <= MAX_RECOVERIES_LIMIT)
            .unwrap_or(defaults.max_recoveries_per_turn),
        min_block_chars,
        max_block_chars,
        min_repeats,
        min_text_chars_before_trip: content
            .min_text_chars_before_trip
            .filter(|value| *value >= min_block_chars.saturating_mul(min_repeats))
            .unwrap_or_else(|| {
                defaults
                    .min_text_chars_before_trip
                    .max(min_block_chars.saturating_mul(min_repeats))
            }),
        watch_thinking: content.watch_thinking.unwrap_or(defaults.watch_thinking),
        thinking_min_block_chars,
        thinking_max_block_chars,
        thinking_min_repeats,
        thinking_min_text_chars_before_trip: content
            .thinking_min_text_chars_before_trip
            .filter(|value| *value >= thinking_min_block_chars.saturating_mul(thinking_min_repeats))
            .unwrap_or_else(|| {
                defaults
                    .thinking_min_text_chars_before_trip
                    .max(thinking_min_block_chars.saturating_mul(thinking_min_repeats))
            }),
    }
}

fn compile_network_agent_workers(
    content: Option<settings::NetworkAgentWorkersContent>,
) -> NetworkAgentWorkers {
    let Some(content) = content else {
        return NetworkAgentWorkers::default();
    };

    let defaults = NetworkAgentWorkers::default();
    let max_workers = content
        .max_workers
        .unwrap_or(defaults.max_workers)
        .clamp(1, 10);

    let endpoints = content
        .endpoints
        .unwrap_or_default()
        .into_iter()
        .filter_map(|endpoint| {
            let id = endpoint.id.filter(|id| !id.trim().is_empty())?;
            let provider = endpoint
                .provider
                .filter(|provider| !provider.trim().is_empty())?;
            let model = endpoint.model.filter(|model| !model.trim().is_empty())?;
            Some(NetworkAgentWorkerEndpoint {
                id,
                provider: LanguageModelProviderSetting(provider.into()),
                model,
                max_concurrent: endpoint.max_concurrent.unwrap_or(1).max(1),
                enabled: endpoint.enabled.unwrap_or(true),
            })
        })
        .collect();

    NetworkAgentWorkers {
        scheduling: content
            .scheduling
            .as_deref()
            .map(WorkerScheduling::parse)
            .unwrap_or_default(),
        max_workers,
        include_default_model: content
            .include_default_model
            .unwrap_or(defaults.include_default_model),
        endpoints,
    }
}

fn compile_full_access_settings(
    content: Option<settings::FullAccessSettingsContent>,
) -> FullAccessSettings {
    let Some(content) = content else {
        return FullAccessSettings::default();
    };

    let mut allowed_roots = Vec::new();
    for path in content
        .allowed_roots
        .map(|paths| paths.0)
        .unwrap_or_default()
    {
        if path.is_absolute()
            && let Ok(normalized) = util::paths::normalize_lexically(&path)
        {
            util::paths::insert_subtree(&mut allowed_roots, normalized);
        }
    }

    let mut denied_roots = Vec::new();
    for path in content
        .denied_roots
        .map(|paths| paths.0)
        .unwrap_or_default()
    {
        if path.is_absolute()
            && let Ok(normalized) = util::paths::normalize_lexically(&path)
        {
            util::paths::insert_subtree(&mut denied_roots, normalized);
        }
    }

    FullAccessSettings {
        enabled: content.enabled.unwrap_or(false),
        allowed_roots,
        denied_roots,
    }
}

fn compile_sandbox_permissions(
    content: Option<settings::SandboxPermissionsContent>,
) -> SandboxPermissions {
    let Some(content) = content else {
        return SandboxPermissions::default();
    };

    let mut write_paths = Vec::new();
    for path in content.write_paths.map(|paths| paths.0).unwrap_or_default() {
        // Normalize away `..`/`.` before storing, since coverage checks are
        // purely lexical; drop paths that escape the filesystem root.
        if let Ok(normalized) = util::paths::normalize_lexically(&path) {
            util::paths::insert_subtree(&mut write_paths, normalized);
        }
    }

    SandboxPermissions {
        allow_network: content.allow_network.unwrap_or(false),
        allow_fs_write_all: content.allow_fs_write_all.unwrap_or(false),
        allow_unsandboxed: content.allow_unsandboxed.unwrap_or(false),
        write_paths,
    }
}

fn compile_tool_permissions(content: Option<settings::ToolPermissionsContent>) -> ToolPermissions {
    let Some(content) = content else {
        return ToolPermissions::default();
    };

    let tools = content
        .tools
        .into_iter()
        .map(|(tool_name, rules_content)| {
            let mut invalid_patterns = Vec::new();

            let (always_allow, allow_errors) = compile_regex_rules(
                rules_content.always_allow.map(|v| v.0).unwrap_or_default(),
                "always_allow",
            );
            invalid_patterns.extend(allow_errors);

            let (always_deny, deny_errors) = compile_regex_rules(
                rules_content.always_deny.map(|v| v.0).unwrap_or_default(),
                "always_deny",
            );
            invalid_patterns.extend(deny_errors);

            let (always_confirm, confirm_errors) = compile_regex_rules(
                rules_content
                    .always_confirm
                    .map(|v| v.0)
                    .unwrap_or_default(),
                "always_confirm",
            );
            invalid_patterns.extend(confirm_errors);

            // Log invalid patterns for debugging. Users will see an error when they
            // attempt to use a tool with invalid patterns in their settings.
            for invalid in &invalid_patterns {
                log::error!(
                    "Invalid regex pattern in tool_permissions for '{}' tool ({}): '{}' - {}",
                    tool_name,
                    invalid.rule_type,
                    invalid.pattern,
                    invalid.error,
                );
            }

            let rules = ToolRules {
                // Preserve tool-specific default; None means fall back to global default at decision time
                default: rules_content.default,
                always_allow,
                always_deny,
                always_confirm,
                invalid_patterns,
            };
            (tool_name, rules)
        })
        .collect();

    ToolPermissions {
        default: content.default.unwrap_or_default(),
        tools,
    }
}

fn compile_regex_rules(
    rules: Vec<settings::ToolRegexRule>,
    rule_type: &str,
) -> (Vec<CompiledRegex>, Vec<InvalidRegexPattern>) {
    let mut compiled = Vec::new();
    let mut errors = Vec::new();

    for rule in rules {
        if rule.pattern.is_empty() {
            errors.push(InvalidRegexPattern {
                pattern: rule.pattern,
                rule_type: rule_type.to_string(),
                error: "empty regex patterns are not allowed".to_string(),
            });
            continue;
        }
        let case_sensitive = rule.case_sensitive.unwrap_or(false);
        match CompiledRegex::try_new(&rule.pattern, case_sensitive) {
            Ok(regex) => compiled.push(regex),
            Err(error) => {
                errors.push(InvalidRegexPattern {
                    pattern: rule.pattern,
                    rule_type: rule_type.to_string(),
                    error: error.to_string(),
                });
            }
        }
    }

    (compiled, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{TestAppContext, UpdateGlobal};
    use serde_json::json;
    use settings::ToolPermissionMode;
    use settings::ToolPermissionsContent;

    #[test]
    fn test_compiled_regex_case_insensitive() {
        let regex = CompiledRegex::new("rm\\s+-rf", false).unwrap();
        assert!(regex.is_match("rm -rf /"));
        assert!(regex.is_match("RM -RF /"));
        assert!(regex.is_match("Rm -Rf /"));
    }

    #[test]
    fn test_compiled_regex_case_sensitive() {
        let regex = CompiledRegex::new("DROP\\s+TABLE", true).unwrap();
        assert!(regex.is_match("DROP TABLE users"));
        assert!(!regex.is_match("drop table users"));
    }

    #[test]
    fn test_invalid_regex_returns_none() {
        let result = CompiledRegex::new("[invalid(regex", false);
        assert!(result.is_none());
    }

    #[test]
    fn test_tool_permissions_parsing() {
        let json = json!({
            "tools": {
                "terminal": {
                    "default": "allow",
                    "always_deny": [
                        { "pattern": "rm\\s+-rf" }
                    ],
                    "always_allow": [
                        { "pattern": "^git\\s" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal_rules = permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal_rules.default, Some(ToolPermissionMode::Allow));
        assert_eq!(terminal_rules.always_deny.len(), 1);
        assert_eq!(terminal_rules.always_allow.len(), 1);
        assert!(terminal_rules.always_deny[0].is_match("rm -rf /"));
        assert!(terminal_rules.always_allow[0].is_match("git status"));
    }

    #[test]
    fn test_tool_rules_default() {
        let json = json!({
            "tools": {
                "edit_file": {
                    "default": "deny"
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let rules = permissions.tools.get("edit_file").unwrap();
        assert_eq!(rules.default, Some(ToolPermissionMode::Deny));
    }

    #[test]
    fn test_tool_permissions_empty() {
        let permissions = compile_tool_permissions(None);
        assert!(permissions.tools.is_empty());
        assert_eq!(permissions.default, ToolPermissionMode::Confirm);
    }

    #[test]
    fn test_sandbox_permissions_empty() {
        let permissions = compile_sandbox_permissions(None);
        assert_eq!(permissions, SandboxPermissions::default());
    }

    #[test]
    fn test_full_access_settings_require_absolute_normalized_roots() {
        let (allowed, denied) = if cfg!(windows) {
            (r"C:\tmp\agent\..\agent", r"C:\protected")
        } else {
            ("/tmp/agent/../agent", "/protected")
        };
        let content: settings::FullAccessSettingsContent = serde_json::from_value(json!({
            "enabled": true,
            "allowed_roots": [allowed, "relative/path"],
            "denied_roots": [denied]
        }))
        .unwrap();
        let settings = compile_full_access_settings(Some(content));

        assert!(settings.enabled);
        assert_eq!(
            settings.allowed_roots,
            vec![if cfg!(windows) {
                PathBuf::from(r"C:\tmp\agent")
            } else {
                PathBuf::from("/tmp/agent")
            }]
        );
        assert_eq!(settings.denied_roots, vec![PathBuf::from(denied)]);
    }

    #[test]
    fn test_sandbox_permissions_parsing_and_pruning() {
        let json = json!({
            "allow_network": true,
            "allow_unsandboxed": true,
            "write_paths": [
                "/tmp/build/cache",
                "/tmp/build",
                "/var/log"
            ]
        });

        let content: settings::SandboxPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_sandbox_permissions(Some(content));

        assert!(permissions.allow_network);
        assert!(!permissions.allow_fs_write_all);
        assert!(permissions.allow_unsandboxed);
        assert_eq!(
            permissions.write_paths,
            vec![PathBuf::from("/tmp/build"), PathBuf::from("/var/log")]
        );
    }

    #[test]
    fn test_sandbox_permissions_normalizes_and_prunes_parent_traversal() {
        let json = json!({
            "write_paths": [
                "/tmp/build/../build/cache",
                "/tmp/build",
            ]
        });

        let content: settings::SandboxPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_sandbox_permissions(Some(content));

        // `/tmp/build/../build/cache` normalizes to `/tmp/build/cache`, which is
        // then pruned as a redundant child of `/tmp/build`.
        assert_eq!(permissions.write_paths, vec![PathBuf::from("/tmp/build")]);
    }

    #[test]
    fn test_tool_rules_default_returns_confirm() {
        let default_rules = ToolRules::default();
        assert_eq!(default_rules.default, None);
        assert!(default_rules.always_allow.is_empty());
        assert!(default_rules.always_deny.is_empty());
        assert!(default_rules.always_confirm.is_empty());
    }

    #[test]
    fn test_tool_permissions_with_multiple_tools() {
        let json = json!({
            "tools": {
                "terminal": {
                    "default": "allow",
                    "always_deny": [{ "pattern": "rm\\s+-rf" }]
                },
                "edit_file": {
                    "default": "confirm",
                    "always_deny": [{ "pattern": "\\.env$" }]
                },
                "delete_path": {
                    "default": "deny"
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        assert_eq!(permissions.tools.len(), 3);

        let terminal = permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal.default, Some(ToolPermissionMode::Allow));
        assert_eq!(terminal.always_deny.len(), 1);

        let edit_file = permissions.tools.get("edit_file").unwrap();
        assert_eq!(edit_file.default, Some(ToolPermissionMode::Confirm));
        assert!(edit_file.always_deny[0].is_match("secrets.env"));

        let delete_path = permissions.tools.get("delete_path").unwrap();
        assert_eq!(delete_path.default, Some(ToolPermissionMode::Deny));
    }

    #[test]
    fn test_tool_permissions_with_all_rule_types() {
        let json = json!({
            "tools": {
                "terminal": {
                    "always_deny": [{ "pattern": "rm\\s+-rf" }],
                    "always_confirm": [{ "pattern": "sudo\\s" }],
                    "always_allow": [{ "pattern": "^git\\s+status" }]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal = permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal.always_deny.len(), 1);
        assert_eq!(terminal.always_confirm.len(), 1);
        assert_eq!(terminal.always_allow.len(), 1);

        assert!(terminal.always_deny[0].is_match("rm -rf /"));
        assert!(terminal.always_confirm[0].is_match("sudo apt install"));
        assert!(terminal.always_allow[0].is_match("git status"));
    }

    #[test]
    fn test_invalid_regex_is_tracked_and_valid_ones_still_compile() {
        let json = json!({
            "tools": {
                "terminal": {
                    "always_deny": [
                        { "pattern": "[invalid(regex" },
                        { "pattern": "valid_pattern" }
                    ],
                    "always_allow": [
                        { "pattern": "[another_bad" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal = permissions.tools.get("terminal").unwrap();

        // Valid patterns should still be compiled
        assert_eq!(terminal.always_deny.len(), 1);
        assert!(terminal.always_deny[0].is_match("valid_pattern"));

        // Invalid patterns should be tracked (order depends on processing order)
        assert_eq!(terminal.invalid_patterns.len(), 2);

        let deny_invalid = terminal
            .invalid_patterns
            .iter()
            .find(|p| p.rule_type == "always_deny")
            .expect("should have invalid pattern from always_deny");
        assert_eq!(deny_invalid.pattern, "[invalid(regex");
        assert!(!deny_invalid.error.is_empty());

        let allow_invalid = terminal
            .invalid_patterns
            .iter()
            .find(|p| p.rule_type == "always_allow")
            .expect("should have invalid pattern from always_allow");
        assert_eq!(allow_invalid.pattern, "[another_bad");

        // ToolPermissions helper methods should work
        assert!(permissions.has_invalid_patterns());
        assert_eq!(permissions.invalid_patterns().len(), 2);
    }

    #[test]
    fn test_deny_takes_precedence_over_allow_and_confirm() {
        let json = json!({
            "tools": {
                "terminal": {
                    "default": "allow",
                    "always_deny": [{ "pattern": "dangerous" }],
                    "always_confirm": [{ "pattern": "dangerous" }],
                    "always_allow": [{ "pattern": "dangerous" }]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));
        let terminal = permissions.tools.get("terminal").unwrap();

        assert!(
            terminal.always_deny[0].is_match("run dangerous command"),
            "Deny rule should match"
        );
        assert!(
            terminal.always_allow[0].is_match("run dangerous command"),
            "Allow rule should also match (but deny takes precedence at evaluation time)"
        );
        assert!(
            terminal.always_confirm[0].is_match("run dangerous command"),
            "Confirm rule should also match (but deny takes precedence at evaluation time)"
        );
    }

    #[test]
    fn test_confirm_takes_precedence_over_allow() {
        let json = json!({
            "tools": {
                "terminal": {
                    "default": "allow",
                    "always_confirm": [{ "pattern": "risky" }],
                    "always_allow": [{ "pattern": "risky" }]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));
        let terminal = permissions.tools.get("terminal").unwrap();

        assert!(
            terminal.always_confirm[0].is_match("do risky thing"),
            "Confirm rule should match"
        );
        assert!(
            terminal.always_allow[0].is_match("do risky thing"),
            "Allow rule should also match (but confirm takes precedence at evaluation time)"
        );
    }

    #[test]
    fn test_regex_matches_anywhere_in_string_not_just_anchored() {
        let json = json!({
            "tools": {
                "terminal": {
                    "always_deny": [
                        { "pattern": "rm\\s+-rf" },
                        { "pattern": "/etc/passwd" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));
        let terminal = permissions.tools.get("terminal").unwrap();

        assert!(
            terminal.always_deny[0].is_match("echo hello && rm -rf /"),
            "Should match rm -rf in the middle of a command chain"
        );
        assert!(
            terminal.always_deny[0].is_match("cd /tmp; rm -rf *"),
            "Should match rm -rf after semicolon"
        );
        assert!(
            terminal.always_deny[1].is_match("cat /etc/passwd | grep root"),
            "Should match /etc/passwd in a pipeline"
        );
        assert!(
            terminal.always_deny[1].is_match("vim /etc/passwd"),
            "Should match /etc/passwd as argument"
        );
    }

    #[test]
    fn test_fork_bomb_pattern_matches() {
        let fork_bomb_regex = CompiledRegex::new(r":\(\)\{\s*:\|:&\s*\};:", false).unwrap();
        assert!(
            fork_bomb_regex.is_match(":(){ :|:& };:"),
            "Should match the classic fork bomb"
        );
        assert!(
            fork_bomb_regex.is_match(":(){ :|:&};:"),
            "Should match fork bomb without spaces"
        );
    }

    #[test]
    fn test_compiled_regex_stores_case_sensitivity() {
        let case_sensitive = CompiledRegex::new("test", true).unwrap();
        let case_insensitive = CompiledRegex::new("test", false).unwrap();

        assert!(case_sensitive.case_sensitive);
        assert!(!case_insensitive.case_sensitive);
    }

    #[test]
    fn test_invalid_regex_is_skipped_not_fail() {
        let json = json!({
            "tools": {
                "terminal": {
                    "always_deny": [
                        { "pattern": "[invalid(regex" },
                        { "pattern": "valid_pattern" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal = permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal.always_deny.len(), 1);
        assert!(terminal.always_deny[0].is_match("valid_pattern"));
    }

    #[test]
    fn test_unconfigured_tool_not_in_permissions() {
        let json = json!({
            "tools": {
                "terminal": {
                    "default": "allow"
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        assert!(permissions.tools.contains_key("terminal"));
        assert!(!permissions.tools.contains_key("edit_file"));
        assert!(!permissions.tools.contains_key("fetch"));
    }

    #[test]
    fn test_always_allow_pattern_only_matches_specified_commands() {
        // Reproduces user-reported bug: when always_allow has pattern "^echo\s",
        // only "echo hello" should be allowed, not "git status".
        //
        // User config:
        //   always_allow_tool_actions: false
        //   tool_permissions.tools.terminal.always_allow: [{ pattern: "^echo\\s" }]
        let json = json!({
            "tools": {
                "terminal": {
                    "always_allow": [
                        { "pattern": "^echo\\s" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal = permissions.tools.get("terminal").unwrap();

        // Verify the pattern was compiled
        assert_eq!(
            terminal.always_allow.len(),
            1,
            "Should have one always_allow pattern"
        );

        // Verify the pattern matches "echo hello"
        assert!(
            terminal.always_allow[0].is_match("echo hello"),
            "Pattern ^echo\\s should match 'echo hello'"
        );

        // Verify the pattern does NOT match "git status"
        assert!(
            !terminal.always_allow[0].is_match("git status"),
            "Pattern ^echo\\s should NOT match 'git status'"
        );

        // Verify the pattern does NOT match "echoHello" (no space)
        assert!(
            !terminal.always_allow[0].is_match("echoHello"),
            "Pattern ^echo\\s should NOT match 'echoHello' (requires whitespace)"
        );

        assert_eq!(
            terminal.default, None,
            "default should be None when not specified"
        );
    }

    #[test]
    fn test_empty_regex_pattern_is_invalid() {
        let json = json!({
            "tools": {
                "terminal": {
                    "always_allow": [
                        { "pattern": "" }
                    ],
                    "always_deny": [
                        { "case_sensitive": true }
                    ],
                    "always_confirm": [
                        { "pattern": "" },
                        { "pattern": "valid_pattern" }
                    ]
                }
            }
        });

        let content: ToolPermissionsContent = serde_json::from_value(json).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        let terminal = permissions.tools.get("terminal").unwrap();

        assert_eq!(terminal.always_allow.len(), 0);
        assert_eq!(terminal.always_deny.len(), 0);
        assert_eq!(terminal.always_confirm.len(), 1);
        assert!(terminal.always_confirm[0].is_match("valid_pattern"));

        assert_eq!(terminal.invalid_patterns.len(), 3);
        for invalid in &terminal.invalid_patterns {
            assert_eq!(invalid.pattern, "");
            assert!(invalid.error.contains("empty"));
        }
    }

    #[test]
    fn test_default_json_tool_permissions_parse() {
        let default_json = include_str!("../../../assets/settings/default.json");
        let value: serde_json_lenient::Value = serde_json_lenient::from_str(default_json).unwrap();
        let agent = value
            .get("agent")
            .expect("default.json should have 'agent' key");
        let tool_permissions_value = agent
            .get("tool_permissions")
            .expect("agent should have 'tool_permissions' key");

        let content: ToolPermissionsContent =
            serde_json_lenient::from_value(tool_permissions_value.clone()).unwrap();
        let permissions = compile_tool_permissions(Some(content));

        assert_eq!(permissions.default, ToolPermissionMode::Confirm);

        assert!(
            permissions.tools.is_empty(),
            "default.json should not have any active tool-specific rules, found: {:?}",
            permissions.tools.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_tool_permissions_explicit_global_default() {
        let json_allow = json!({
            "default": "allow"
        });
        let content: ToolPermissionsContent = serde_json::from_value(json_allow).unwrap();
        let permissions = compile_tool_permissions(Some(content));
        assert_eq!(permissions.default, ToolPermissionMode::Allow);

        let json_deny = json!({
            "default": "deny"
        });
        let content: ToolPermissionsContent = serde_json::from_value(json_deny).unwrap();
        let permissions = compile_tool_permissions(Some(content));
        assert_eq!(permissions.default, ToolPermissionMode::Deny);
    }

    #[gpui::test]
    fn test_auto_thread_rollover_defaults_and_overrides(cx: &mut gpui::App) {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        project::DisableAiSettings::register(cx);
        AgentSettings::register(cx);

        // Defaults: enabled with a 0.5 context-fraction floor (raised at
        // runtime above auto-compaction when the model requires it).
        let defaults = &AgentSettings::get_global(cx).auto_thread_rollover;
        assert!(defaults.enabled);
        assert_eq!(defaults.context_fraction, 0.5);

        // User overrides are applied, and out-of-range fractions are clamped.
        SettingsStore::update_global(cx, |store, cx| {
            store
                .set_user_settings(
                    r#"{ "agent": { "auto_thread_rollover": { "enabled": false, "context_fraction": 5.0 } } }"#,
                    cx,
                )
                .unwrap();
        });

        let overridden = &AgentSettings::get_global(cx).auto_thread_rollover;
        assert!(!overridden.enabled);
        assert_eq!(overridden.context_fraction, 0.95);
    }

    #[gpui::test]
    fn test_anti_loop_defaults_and_bounds(cx: &mut gpui::App) {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        project::DisableAiSettings::register(cx);
        AgentSettings::register(cx);

        let defaults = &AgentSettings::get_global(cx).anti_loop;
        assert!(defaults.enabled);
        assert_eq!(defaults.max_recoveries_per_turn, 1);
        assert_eq!(defaults.min_block_chars, 64);
        assert_eq!(defaults.max_block_chars, 1_024);
        assert_eq!(defaults.min_repeats, 3);
        assert_eq!(defaults.min_text_chars_before_trip, 256);
        assert!(defaults.watch_thinking);
        assert_eq!(defaults.thinking_min_block_chars, 128);
        assert_eq!(defaults.thinking_max_block_chars, 1_024);
        assert_eq!(defaults.thinking_min_repeats, 4);
        assert_eq!(defaults.thinking_min_text_chars_before_trip, 512);

        SettingsStore::update_global(cx, |store, cx| {
            store
                .set_user_settings(
                    r#"{
                        "agent": {
                            "anti_loop": {
                                "enabled": false,
                                "max_recoveries_per_turn": 99,
                                "min_block_chars": 1,
                                "max_block_chars": 2,
                                "min_repeats": 1,
                                "min_text_chars_before_trip": 1,
                                "watch_thinking": false,
                                "thinking_min_block_chars": 1,
                                "thinking_max_block_chars": 2,
                                "thinking_min_repeats": 1,
                                "thinking_min_text_chars_before_trip": 1
                            }
                        }
                    }"#,
                    cx,
                )
                .unwrap();
        });

        let bounded = &AgentSettings::get_global(cx).anti_loop;
        assert!(!bounded.enabled);
        assert_eq!(bounded.max_recoveries_per_turn, 1);
        assert_eq!(bounded.min_block_chars, 64);
        assert_eq!(bounded.max_block_chars, 1_024);
        assert_eq!(bounded.min_repeats, 3);
        assert_eq!(bounded.min_text_chars_before_trip, 256);
        assert!(!bounded.watch_thinking);
        assert_eq!(bounded.thinking_min_block_chars, 128);
        assert_eq!(bounded.thinking_max_block_chars, 1_024);
        assert_eq!(bounded.thinking_min_repeats, 4);
        assert_eq!(bounded.thinking_min_text_chars_before_trip, 512);
    }

    #[gpui::test]
    fn test_get_layout(cx: &mut gpui::App) {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        project::DisableAiSettings::register(cx);
        AgentSettings::register(cx);

        // Should be Agent with an empty user layout (user hasn't customized).
        let layout = AgentSettings::get_layout(cx);
        let WindowLayout::Agent(Some(user_layout)) = layout else {
            panic!("expected Agent(Some), got {:?}", layout);
        };
        assert_eq!(user_layout, PanelLayout::default());

        // User explicitly sets agent dock to left (matching the default).
        // The merged result is still agent, but the user layout captures
        // only what the user wrote.
        SettingsStore::update_global(cx, |store, cx| {
            store
                .set_user_settings(r#"{ "agent": { "dock": "left" } }"#, cx)
                .unwrap();
        });

        let layout = AgentSettings::get_layout(cx);
        let WindowLayout::Agent(Some(user_layout)) = layout else {
            panic!("expected Agent(Some), got {:?}", layout);
        };
        assert_eq!(user_layout.agent_dock, Some(DockPosition::Left));
        assert_eq!(user_layout.project_panel_dock, None);
        assert_eq!(user_layout.outline_panel_dock, None);
        assert_eq!(user_layout.collaboration_panel_dock, None);
        assert_eq!(user_layout.git_panel_dock, None);

        // User sets a combination that doesn't match either preset:
        // agent on the left but project panel also on the left.
        SettingsStore::update_global(cx, |store, cx| {
            store
                .set_user_settings(
                    r#"{
                        "agent": { "dock": "left" },
                        "project_panel": { "dock": "left" }
                    }"#,
                    cx,
                )
                .unwrap();
        });

        let layout = AgentSettings::get_layout(cx);
        let WindowLayout::Custom(user_layout) = layout else {
            panic!("expected Custom, got {:?}", layout);
        };
        assert_eq!(user_layout.agent_dock, Some(DockPosition::Left));
        assert_eq!(user_layout.project_panel_dock, Some(DockSide::Left));
    }

    #[gpui::test]
    fn test_set_layout_round_trip(cx: &mut gpui::App) {
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        project::DisableAiSettings::register(cx);
        AgentSettings::register(cx);

        // User has a custom layout: agent on the right with project panel
        // also on the right. This doesn't match either preset.
        SettingsStore::update_global(cx, |store, cx| {
            store
                .set_user_settings(
                    r#"{
                        "agent": { "dock": "right" },
                        "project_panel": { "dock": "right" }
                    }"#,
                    cx,
                )
                .unwrap();
        });

        let original = AgentSettings::get_layout(cx);
        let WindowLayout::Custom(ref original_user_layout) = original else {
            panic!("expected Custom, got {:?}", original);
        };
        assert_eq!(original_user_layout.agent_dock, Some(DockPosition::Right));
        assert_eq!(
            original_user_layout.project_panel_dock,
            Some(DockSide::Right)
        );
        assert_eq!(original_user_layout.outline_panel_dock, None);

        // Switch to the agent layout. This overwrites the user settings.
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |settings| {
                PanelLayout::AGENT.write_to(settings);
            });
        });

        let layout = AgentSettings::get_layout(cx);
        assert!(matches!(layout, WindowLayout::Agent(_)));

        // Restore the original custom layout.
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |settings| {
                original_user_layout.write_to(settings);
            });
        });

        // Should be back to the same custom layout.
        let restored = AgentSettings::get_layout(cx);
        let WindowLayout::Custom(restored_user_layout) = restored else {
            panic!("expected Custom, got {:?}", restored);
        };
        assert_eq!(restored_user_layout.agent_dock, Some(DockPosition::Right));
        assert_eq!(
            restored_user_layout.project_panel_dock,
            Some(DockSide::Right)
        );
        assert_eq!(restored_user_layout.outline_panel_dock, None);
    }

    #[gpui::test]
    async fn test_set_layout_minimal_diff(cx: &mut TestAppContext) {
        let fs = fs::FakeFs::new(cx.background_executor.clone());
        fs.save(
            paths::settings_file().as_path(),
            &serde_json::json!({
                "agent": { "dock": "left" },
                "project_panel": { "dock": "left" }
            })
            .to_string()
            .into(),
            Default::default(),
        )
        .await
        .unwrap();

        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            project::DisableAiSettings::register(cx);
            AgentSettings::register(cx);

            // User has agent=left (matches preset) and project_panel=left (does not)
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(
                        r#"{
                            "agent": { "dock": "left" },
                            "project_panel": { "dock": "left" }
                        }"#,
                        cx,
                    )
                    .unwrap();
            });

            let layout = AgentSettings::get_layout(cx);
            assert!(matches!(layout, WindowLayout::Custom(_)));

            AgentSettings::set_layout(WindowLayout::agent(), fs.clone(), cx)
        })
        .await
        .ok();

        cx.run_until_parked();

        let written = fs.load(paths::settings_file().as_path()).await.unwrap();
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.set_user_settings(&written, cx).unwrap();
            });

            // The user settings should still have agent=left (preserved)
            // and now project_panel=right (changed to match preset).
            let store = cx.global::<SettingsStore>();
            let user_layout = store
                .raw_user_settings()
                .map(|u| PanelLayout::read_from(u.content.as_ref()))
                .unwrap_or_default();

            assert_eq!(user_layout.agent_dock, Some(DockPosition::Left));
            assert_eq!(user_layout.project_panel_dock, Some(DockSide::Right));
            // Other fields weren't in user settings and didn't need changing.
            assert_eq!(user_layout.outline_panel_dock, None);

            // And the merged result should now match agent.
            let layout = AgentSettings::get_layout(cx);
            assert!(matches!(layout, WindowLayout::Agent(_)));
        });
    }

    #[gpui::test]
    async fn test_backfill_editor_layout(cx: &mut TestAppContext) {
        let fs = fs::FakeFs::new(cx.background_executor.clone());
        // User has only customized project_panel to "right".
        fs.save(
            paths::settings_file().as_path(),
            &serde_json::json!({
                "project_panel": { "dock": "right" }
            })
            .to_string()
            .into(),
            Default::default(),
        )
        .await
        .unwrap();

        cx.update(|cx| {
            let store = SettingsStore::test(cx);
            cx.set_global(store);
            project::DisableAiSettings::register(cx);
            AgentSettings::register(cx);

            // Simulate pre-migration state: editor defaults (the old world).
            SettingsStore::update_global(cx, |store, cx| {
                store.update_default_settings(cx, |defaults| {
                    PanelLayout::EDITOR.write_to(defaults);
                });
            });

            // User has only customized project_panel to "right".
            SettingsStore::update_global(cx, |store, cx| {
                store
                    .set_user_settings(r#"{ "project_panel": { "dock": "right" } }"#, cx)
                    .unwrap();
            });

            // Run the one-time backfill while still on old defaults.
            AgentSettings::backfill_editor_layout(fs.clone(), cx);
        });

        cx.run_until_parked();

        // Read back the file and apply it.
        let written = fs.load(paths::settings_file().as_path()).await.unwrap();
        cx.update(|cx| {
            SettingsStore::update_global(cx, |store, cx| {
                store.set_user_settings(&written, cx).unwrap();
            });

            // The user's project_panel=right should be preserved (they set it).
            // All other fields should now have the editor preset values
            // written into user settings.
            let store = cx.global::<SettingsStore>();
            let user_layout = store
                .raw_user_settings()
                .map(|u| PanelLayout::read_from(u.content.as_ref()))
                .unwrap_or_default();

            assert_eq!(user_layout.agent_dock, Some(DockPosition::Right));
            assert_eq!(user_layout.project_panel_dock, Some(DockSide::Right));
            assert_eq!(user_layout.outline_panel_dock, Some(DockSide::Left));
            assert_eq!(
                user_layout.collaboration_panel_dock,
                Some(DockPosition::Left)
            );
            assert_eq!(user_layout.git_panel_dock, Some(DockPosition::Left));

            // Even though defaults are now agent, the backfilled user settings
            // keep everything in the editor layout. The user's experience
            // hasn't changed.
            let layout = AgentSettings::get_layout(cx);
            let WindowLayout::Custom(user_layout) = layout else {
                panic!(
                    "expected Custom (editor values override agent defaults), got {:?}",
                    layout
                );
            };
            assert_eq!(user_layout.agent_dock, Some(DockPosition::Right));
            assert_eq!(user_layout.project_panel_dock, Some(DockSide::Right));
        });
    }

    #[test]
    fn test_effective_default_model_uses_network_when_active() {
        let settings = AgentSettings {
            network_agent: NetworkAgentSettings {
                enabled: true,
                model: Some("net-model".into()),
                delegation_mode: NetworkAgentDelegationMode::Balanced,
                workers: NetworkAgentWorkers::default(),
            },
            default_model: Some(LanguageModelSelection {
                provider: LanguageModelProviderSetting("lmstudio".into()),
                model: "local-model".into(),
                enable_thinking: false,
                effort: None,
                speed: None,
            }),
            enabled: false,
            button: false,
            dock: settings::DockPosition::Right,
            starts_open: false,
            flexible: true,
            default_width: gpui::px(300.),
            default_height: gpui::px(600.),
            max_content_width: None,
            subagent_model: None,
            inline_assistant_model: None,
            inline_assistant_use_streaming_tools: false,
            commit_message_model: None,
            commit_message_instructions: None,
            thread_summary_model: None,
            inline_alternatives: vec![],
            favorite_models: vec![],
            default_profile: AgentProfileId::default(),
            profiles: Default::default(),
            notify_when_agent_waiting: settings::NotifyWhenAgentWaiting::default(),
            play_sound_when_agent_done: settings::PlaySoundWhenAgentDone::Never,
            single_file_review: false,
            model_parameters: vec![],
            enable_feedback: false,
            expand_edit_card: true,
            expand_terminal_card: true,
            cancel_generation_on_terminal_stop: true,
            use_modifier_to_send: false,
            message_editor_min_lines: 1,
            tool_permissions: Default::default(),
            sandbox_permissions: Default::default(),
            full_access: Default::default(),
            show_turn_stats: false,
            show_merge_conflict_indicator: true,
            sidebar_side: Default::default(),
            thinking_display: Default::default(),
            auto_thread_rollover: Default::default(),
            anti_loop: Default::default(),
            web_research: Default::default(),
        };

        let effective = settings.effective_default_model().unwrap();
        assert_eq!(
            effective.provider.0,
            AgentSettings::NETWORK_AGENT_PROVIDER_ID
        );
        assert_eq!(effective.model, "net-model");

        let subagent = settings.effective_subagent_model().unwrap();
        assert_eq!(subagent.provider.0, "lmstudio");
        assert_eq!(subagent.model, "local-model");
    }

    #[test]
    fn test_delegation_mode_parsing() {
        assert_eq!(
            NetworkAgentDelegationMode::parse("balanced"),
            NetworkAgentDelegationMode::Balanced
        );
        assert_eq!(
            NetworkAgentDelegationMode::parse("manual"),
            NetworkAgentDelegationMode::Manual
        );
        assert_eq!(
            NetworkAgentDelegationMode::parse("unknown"),
            NetworkAgentDelegationMode::Balanced
        );
    }
}
