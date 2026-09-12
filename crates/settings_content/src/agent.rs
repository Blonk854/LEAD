use collections::{HashMap, IndexMap};
use schemars::{JsonSchema, json_schema};
use serde::{Deserialize, Serialize};
use settings_macros::{MergeFrom, with_fallible_options};
use std::sync::Arc;
use std::{borrow::Cow, path::PathBuf};

use crate::ExtendingVec;

use crate::DockPosition;

/// Where to position the threads sidebar.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum SidebarDockPosition {
    /// Always show the sidebar on the left side.
    #[default]
    Left,
    /// Always show the sidebar on the right side.
    Right,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum SidebarSide {
    #[default]
    Left,
    Right,
}

/// How thinking blocks should be displayed by default in the agent panel.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingBlockDisplay {
    /// Thinking blocks fully expand during streaming, then auto-collapse
    /// when the model finishes thinking. Users can re-expand after collapse.
    #[default]
    Auto,
    /// Thinking blocks auto-expand with a height constraint during streaming,
    /// then remain in their constrained state when complete. Users can click
    /// to fully expand or collapse.
    Preview,
    /// Thinking blocks are always fully expanded by default (no height constraint).
    AlwaysExpanded,
    /// Thinking blocks are always collapsed by default.
    AlwaysCollapsed,
}

/// Which native-agent system prompt to send.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum AgentPromptStyle {
    /// Compact prompt for local models; full prompt for the hybrid orchestrator.
    #[default]
    Auto,
    /// Always use the compact local-model prompt.
    Compact,
    /// Always use the full Claude-style prompt.
    Full,
}

#[with_fallible_options]
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom, Debug, Default)]
pub struct AgentSettingsContent {
    /// Whether the Agent is enabled.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Whether to show the agent panel button in the status bar.
    ///
    /// Default: true
    pub button: Option<bool>,
    /// Where to dock the agent panel.
    ///
    /// Default: left
    pub dock: Option<DockPosition>,
    /// Whether the agent panel should open automatically when a workspace has a folder.
    ///
    /// Default: true
    pub starts_open: Option<bool>,
    /// Whether the agent panel should use flexible (proportional) sizing.
    ///
    /// Default: true
    pub flexible: Option<bool>,
    /// Where to position the threads sidebar.
    ///
    /// Default: left
    pub sidebar_side: Option<SidebarDockPosition>,
    /// Default width in pixels when the agent panel is docked to the left or right.
    ///
    /// Default: 640
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub default_width: Option<f32>,
    /// Default height in pixels when the agent panel is docked to the bottom.
    ///
    /// Default: 320
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub default_height: Option<f32>,
    /// Whether to limit the content width in the agent panel. When enabled,
    /// content will be constrained to `max_content_width` and centered when
    /// the panel is wider than that value, for optimal readability.
    ///
    /// Default: true
    pub limit_content_width: Option<bool>,
    /// Maximum content width in pixels for the agent panel. Content will be
    /// centered when the panel is wider than this value.
    ///
    /// Default: 850
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub max_content_width: Option<f32>,
    /// The default model to use when creating new chats and for other features when a specific model is not specified.
    pub default_model: Option<LanguageModelSelection>,
    /// The model to use for subagents spawned via the `spawn_agent` tool. Defaults to the parent agent's model when not specified.
    pub subagent_model: Option<LanguageModelSelection>,
    /// Favorite models to show at the top of the model selector.
    #[serde(default)]
    pub favorite_models: Vec<LanguageModelSelection>,
    /// Model to use for the inline assistant. Defaults to default_model when not specified.
    pub inline_assistant_model: Option<LanguageModelSelection>,
    /// Model to use for the inline assistant when streaming tools are enabled.
    ///
    /// Default: true
    pub inline_assistant_use_streaming_tools: Option<bool>,
    /// Model to use for generating git commit messages. Defaults to default_model when not specified.
    pub commit_message_model: Option<LanguageModelSelection>,
    /// Custom instructions to include in the prompt when generating git commit messages.
    /// Applied in addition to any project rules files (such as `.rules` or `AGENTS.md`).
    pub commit_message_instructions: Option<String>,
    /// Model to use for generating thread summaries. Defaults to default_model when not specified.
    pub thread_summary_model: Option<LanguageModelSelection>,
    /// Additional models with which to generate alternatives when performing inline assists.
    pub inline_alternatives: Option<Vec<LanguageModelSelection>>,
    /// The default profile to use in the Agent.
    ///
    /// Default: write
    pub default_profile: Option<Arc<str>>,
    /// Which system prompt to send. `auto` uses a compact prompt for local
    /// models and the full prompt for the hybrid network orchestrator.
    ///
    /// Default: auto
    pub prompt_style: Option<AgentPromptStyle>,
    /// The available agent profiles.
    pub profiles: Option<IndexMap<Arc<str>, AgentProfileContent>>,
    /// Where to show a popup notification when the agent is waiting for user input.
    ///
    /// Default: "primary_screen"
    pub notify_when_agent_waiting: Option<NotifyWhenAgentWaiting>,
    /// When to play a sound when the agent has either completed its response, or needs user input.
    ///
    /// Default: never
    pub play_sound_when_agent_done: Option<PlaySoundWhenAgentDone>,
    /// Whether to display agent edits in single-file editors in addition to the review multibuffer pane.
    ///
    /// Default: false
    pub single_file_review: Option<bool>,
    /// Additional parameters for language model requests. When making a request
    /// to a model, parameters will be taken from the last entry in this list
    /// that matches the model's provider and name. In each entry, both provider
    /// and model are optional, so that you can specify parameters for either
    /// one.
    ///
    /// Default: []
    #[serde(default)]
    pub model_parameters: Vec<LanguageModelParameters>,
    /// Whether to show thumb buttons for feedback in the agent panel.
    ///
    /// Default: true
    pub enable_feedback: Option<bool>,
    /// Whether to have edit cards in the agent panel expanded, showing a preview of the full diff.
    ///
    /// Default: true
    pub expand_edit_card: Option<bool>,
    /// Whether to have terminal cards in the agent panel expanded, showing the whole command output.
    ///
    /// Default: true
    pub expand_terminal_card: Option<bool>,
    /// How thinking blocks should be displayed by default in the agent panel.
    ///
    /// Default: automatic
    pub thinking_display: Option<ThinkingBlockDisplay>,
    /// Whether clicking the stop button on a running terminal tool should also cancel the agent's generation.
    /// Note that this only applies to the stop button, not to ctrl+c inside the terminal.
    ///
    /// Default: true
    pub cancel_generation_on_terminal_stop: Option<bool>,
    /// Whether to always use cmd-enter (or ctrl-enter on Linux or Windows) to send messages in the agent panel.
    ///
    /// Default: false
    pub use_modifier_to_send: Option<bool>,
    /// Minimum number of lines of height the agent message editor should have.
    ///
    /// Default: 4
    pub message_editor_min_lines: Option<usize>,
    /// Whether to show turn statistics (elapsed time during generation, final turn duration).
    ///
    /// Default: false
    pub show_turn_stats: Option<bool>,
    /// Whether to show the merge conflict indicator in the status bar
    /// that offers to resolve conflicts using the agent.
    ///
    /// Default: true
    pub show_merge_conflict_indicator: Option<bool>,
    /// Per-tool permission rules for granular control over which tool actions
    /// require confirmation.
    ///
    /// The global `default` applies when no tool-specific rules match.
    /// For external agent servers (e.g. Claude Agent) that define their own
    /// permission modes, "deny" and "confirm" still take precedence — the
    /// external agent's permission system is only used when LEAD would allow
    /// the action. Per-tool regex patterns (`always_allow`, `always_deny`,
    /// `always_confirm`) match against the tool's text input (command, path,
    /// URL, etc.).
    pub tool_permissions: Option<ToolPermissionsContent>,

    /// Persistent sandbox permission grants for agent-run terminal commands.
    /// These are populated when choosing "Allow always" from a sandbox
    /// escalation prompt.
    pub sandbox_permissions: Option<SandboxPermissionsContent>,

    /// Guard-railed whole-PC access for native agent tools.
    ///
    /// Disabled by default. When enabled, operations outside project
    /// worktrees still require explicit user authorization unless their
    /// target is under an allowed root.
    pub full_access: Option<FullAccessSettingsContent>,

    /// Optional "Network Agent" configuration. When enabled, a model served
    /// from a network endpoint drives the main agent (planning and tool calls)
    /// while the local `default_model` handles delegated subagent work.
    pub network_agent: Option<NetworkAgentSettingsContent>,

    /// Automatically roll a too-long conversation into a fresh thread, seeded
    /// with a robust hand-off summary, to keep the model performing well.
    pub auto_thread_rollover: Option<AutoThreadRolloverContent>,

    /// Detect and recover from repetitive local-model output.
    pub anti_loop: Option<AntiLoopSettingsContent>,

    /// Local web research (HTML search discovery + hardened fetch).
    pub web_research: Option<WebResearchSettingsContent>,
}

/// Configuration for local-model repetition detection and recovery.
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct AntiLoopSettingsContent {
    /// Whether repetition detection is enabled.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Number of automatic recovery attempts before stopping the turn.
    ///
    /// Default: 1
    pub max_recoveries_per_turn: Option<usize>,
    /// Minimum repeated block length in characters.
    ///
    /// Default: 64
    pub min_block_chars: Option<usize>,
    /// Maximum repeated block length in characters.
    ///
    /// Default: 1024
    pub max_block_chars: Option<usize>,
    /// Total contiguous occurrences required to detect a loop.
    ///
    /// Default: 3
    pub min_repeats: Option<usize>,
    /// Minimum generated text length before detection can trigger.
    ///
    /// Default: 256
    pub min_text_chars_before_trip: Option<usize>,
    /// Whether plaintext LM Studio reasoning streams are monitored.
    ///
    /// Default: true
    pub watch_thinking: Option<bool>,
    /// Minimum repeated reasoning block length in characters.
    ///
    /// Default: 128
    pub thinking_min_block_chars: Option<usize>,
    /// Maximum repeated reasoning block length in characters.
    ///
    /// Default: 1024
    pub thinking_max_block_chars: Option<usize>,
    /// Total contiguous reasoning-block occurrences required to detect a loop.
    ///
    /// Default: 4
    pub thinking_min_repeats: Option<usize>,
    /// Minimum reasoning text length before detection can trigger.
    ///
    /// Default: 512
    pub thinking_min_text_chars_before_trip: Option<usize>,
}

/// Local web research: HTML SERP discovery and hardened page fetch.
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct WebResearchSettingsContent {
    /// Whether local web research is enabled.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Preferred discovery provider: `local`, `cloud`, or `auto`.
    ///
    /// Default: local
    pub preferred_provider: Option<String>,
    /// Maximum search hits returned to the model.
    ///
    /// Default: 5
    pub max_search_results: Option<usize>,
    /// Maximum characters per search snippet.
    ///
    /// Default: 300
    pub max_snippet_chars: Option<usize>,
    /// Maximum characters of fetched page markdown kept for the model.
    ///
    /// Default: 12000
    pub max_fetch_chars: Option<usize>,
    /// Maximum raw response bytes to download.
    ///
    /// Default: 1048576
    pub max_response_bytes: Option<u64>,
    /// Maximum redirects to follow while re-checking SSRF on each hop.
    ///
    /// Default: 5
    pub max_redirects: Option<u32>,
    /// HTTP request timeout in milliseconds.
    ///
    /// Default: 15000
    pub request_timeout_ms: Option<u64>,
    /// Maximum concurrent research fetches.
    ///
    /// Default: 2
    pub max_concurrency: Option<usize>,
    /// Minimum delay between requests to the same host, in milliseconds.
    ///
    /// Default: 1000
    pub per_host_delay_ms: Option<u64>,
    /// Whether isolated browser fallback is enabled (system Edge/Chrome).
    ///
    /// Default: false
    pub browser_fallback_enabled: Option<bool>,
    /// Timeout for a single isolated browser page render, in seconds.
    ///
    /// Default: 45
    pub browser_timeout_secs: Option<u64>,
    /// Maximum pages fetched per `research_web` session.
    ///
    /// Default: 6
    pub max_pages: Option<usize>,
    /// Maximum link-follow depth from each SERP seed in `research_web`.
    ///
    /// Default: 1
    pub max_depth: Option<usize>,
    /// Wall-clock budget in seconds for a `research_web` session.
    ///
    /// Default: 90
    pub research_wall_clock_secs: Option<u64>,
    /// SERP HTML cache TTL in seconds (app-data disk cache).
    ///
    /// Default: 3600
    pub cache_ttl_serp_secs: Option<u64>,
    /// Fetched page cache TTL in seconds (app-data disk cache).
    ///
    /// Default: 604800 (7 days)
    pub cache_ttl_page_secs: Option<u64>,
}

/// Configuration for automatic thread rollover and hand-off.
///
/// When a thread's context fills past `context_fraction` of the model's
/// context window, LEAD generates a hand-off summary, opens a brand-new thread
/// seeded with that summary (carrying any active goal), and switches to it.
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct AutoThreadRolloverContent {
    /// Whether automatic thread rollover is enabled.
    ///
    /// Default: true
    pub enabled: Option<bool>,
    /// Fraction (0.0 - 1.0) of the model's context window at which a thread
    /// rolls over into a fresh one. Treated as a floor: LEAD may raise it so
    /// rollover stays above auto-compaction for the active model.
    ///
    /// Default: 0.5
    pub context_fraction: Option<f32>,
}

/// Configuration for the optional "Network Agent" mode.
///
/// When enabled, a model served from a network endpoint drives the main agent
/// (planning and tool calls) while the local `default_model` handles delegated
/// subagent work. The network endpoint itself is configured as an
/// OpenAI-compatible provider under the reserved id `network-agent`.
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct NetworkAgentWorkerEndpointContent {
    /// Stable id for logs and sticky subagent sessions.
    pub id: Option<String>,
    /// Provider key under `language_models.openai_compatible` (or another registered provider).
    pub provider: Option<String>,
    /// Model id on that provider.
    pub model: Option<String>,
    /// Maximum simultaneous subagent turns on this worker.
    ///
    /// Default: 1
    pub max_concurrent: Option<u32>,
    /// Whether this worker participates in the pool.
    ///
    /// Default: true
    pub enabled: Option<bool>,
}

/// Hybrid worker pool: multiple networked LM Studio / Ollama endpoints share
/// subagent load while one orchestrator plans.
#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct NetworkAgentWorkersContent {
    /// How subagent work is assigned across workers: `pool`, `round_robin`, or
    /// `least_busy`.
    ///
    /// Default: least_busy
    pub scheduling: Option<String>,
    /// Maximum number of worker endpoints to use (1–10).
    ///
    /// Default: 10
    pub max_workers: Option<u32>,
    /// Include `agent.default_model` as a pool member.
    ///
    /// Default: true
    pub include_default_model: Option<bool>,
    pub endpoints: Option<Vec<NetworkAgentWorkerEndpointContent>>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq, Default)]
pub struct NetworkAgentSettingsContent {
    /// Whether the Network Agent is currently enabled. When disabled, the
    /// agent uses the local `default_model` for everything.
    ///
    /// Default: false
    pub enabled: Option<bool>,
    /// The id of the model (served from the configured network endpoint) to
    /// use as the orchestrating agent model.
    pub model: Option<String>,
    /// How aggressively the orchestrator delegates heavy work to the local
    /// worker via `spawn_agent`. `"balanced"` (default) hides execution tools
    /// from the main thread; `"manual"` leaves tool access unchanged.
    pub delegation_mode: Option<String>,
    /// Optional worker pool for hybrid subagent load sharing.
    pub workers: Option<NetworkAgentWorkersContent>,
}

impl AgentSettingsContent {
    pub fn set_dock(&mut self, dock: DockPosition) {
        self.dock = Some(dock);
    }

    pub fn set_sidebar_side(&mut self, position: SidebarDockPosition) {
        self.sidebar_side = Some(position);
    }

    pub fn set_flexible_size(&mut self, flexible: bool) {
        self.flexible = Some(flexible);
    }

    pub fn set_model(&mut self, language_model: LanguageModelSelection) {
        self.default_model = Some(language_model)
    }

    pub fn set_inline_assistant_model(&mut self, provider: String, model: String) {
        self.inline_assistant_model = Some(LanguageModelSelection {
            provider: provider.into(),
            model,
            enable_thinking: false,
            effort: None,
            speed: None,
        });
    }

    pub fn set_profile(&mut self, profile_id: Arc<str>) {
        self.default_profile = Some(profile_id);
    }

    pub fn add_favorite_model(&mut self, model: LanguageModelSelection) {
        // Note: this is intentional to not compare using `PartialEq`here.
        // Full equality would treat entries that differ just in thinking/effort/speed
        // as distinct and silently produce duplicates.
        if !self
            .favorite_models
            .iter()
            .any(|m| m.provider == model.provider && m.model == model.model)
        {
            self.favorite_models.push(model);
        }
    }

    pub fn remove_favorite_model(&mut self, model: &LanguageModelSelection) {
        self.favorite_models
            .retain(|m| !(m.provider == model.provider && m.model == model.model));
    }

    pub fn update_favorite_model<F>(&mut self, provider: &str, model: &str, f: F)
    where
        F: FnOnce(&mut LanguageModelSelection),
    {
        if let Some(entry) = self
            .favorite_models
            .iter_mut()
            .find(|m| m.provider.0 == provider && m.model == model)
        {
            f(entry);
        }
    }

    pub fn set_tool_default_permission(&mut self, tool_id: &str, mode: ToolPermissionMode) {
        let tool_permissions = self.tool_permissions.get_or_insert_default();
        let tool_rules = tool_permissions
            .tools
            .entry(Arc::from(tool_id))
            .or_default();
        tool_rules.default = Some(mode);
    }

    pub fn add_tool_allow_pattern(&mut self, tool_name: &str, pattern: String) {
        let tool_permissions = self.tool_permissions.get_or_insert_default();
        let tool_rules = tool_permissions
            .tools
            .entry(Arc::from(tool_name))
            .or_default();
        let always_allow = tool_rules.always_allow.get_or_insert_default();
        if !always_allow.0.iter().any(|r| r.pattern == pattern) {
            always_allow.0.push(ToolRegexRule {
                pattern,
                case_sensitive: None,
            });
        }
    }

    pub fn add_tool_deny_pattern(&mut self, tool_name: &str, pattern: String) {
        let tool_permissions = self.tool_permissions.get_or_insert_default();
        let tool_rules = tool_permissions
            .tools
            .entry(Arc::from(tool_name))
            .or_default();
        let always_deny = tool_rules.always_deny.get_or_insert_default();
        if !always_deny.0.iter().any(|r| r.pattern == pattern) {
            always_deny.0.push(ToolRegexRule {
                pattern,
                case_sensitive: None,
            });
        }
    }

    pub fn allow_sandbox_network(&mut self) {
        self.sandbox_permissions
            .get_or_insert_default()
            .allow_network = Some(true);
    }

    pub fn allow_sandbox_fs_write_all(&mut self) {
        self.sandbox_permissions
            .get_or_insert_default()
            .allow_fs_write_all = Some(true);
    }

    pub fn allow_sandbox_unsandboxed(&mut self) {
        self.sandbox_permissions
            .get_or_insert_default()
            .allow_unsandboxed = Some(true);
    }

    pub fn add_sandbox_write_path(&mut self, path: PathBuf) {
        let write_paths = &mut self
            .sandbox_permissions
            .get_or_insert_default()
            .write_paths
            .get_or_insert_default()
            .0;

        util::paths::insert_subtree(write_paths, path);
    }
}

#[with_fallible_options]
#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct AgentProfileContent {
    pub name: Arc<str>,
    #[serde(default)]
    pub tools: IndexMap<Arc<str>, bool>,
    /// Whether all context servers are enabled by default.
    pub enable_all_context_servers: Option<bool>,
    #[serde(default)]
    pub context_servers: IndexMap<Arc<str>, ContextServerPresetContent>,
    /// The default language model selected when using this profile.
    pub default_model: Option<LanguageModelSelection>,
}

#[with_fallible_options]
#[derive(Debug, PartialEq, Clone, Default, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ContextServerPresetContent {
    pub tools: IndexMap<Arc<str>, bool>,
}

#[derive(
    Copy,
    Clone,
    Default,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum NotifyWhenAgentWaiting {
    #[default]
    PrimaryScreen,
    AllScreens,
    Never,
}

#[derive(
    Copy,
    Clone,
    Default,
    Debug,
    Serialize,
    Deserialize,
    JsonSchema,
    MergeFrom,
    PartialEq,
    strum::VariantArray,
    strum::VariantNames,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaySoundWhenAgentDone {
    #[default]
    Never,
    WhenHidden,
    Always,
}

impl PlaySoundWhenAgentDone {
    pub fn should_play(&self, visible: bool) -> bool {
        match self {
            PlaySoundWhenAgentDone::Never => false,
            PlaySoundWhenAgentDone::WhenHidden => !visible,
            PlaySoundWhenAgentDone::Always => true,
        }
    }
}

#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq)]
pub struct LanguageModelSelection {
    pub provider: LanguageModelProviderSetting,
    pub model: String,
    #[serde(default)]
    pub enable_thinking: bool,
    pub effort: Option<String>,
    pub speed: Option<language_model_core::Speed>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, MergeFrom, PartialEq)]
pub struct LanguageModelParameters {
    pub provider: Option<LanguageModelProviderSetting>,
    pub model: Option<String>,
    #[serde(serialize_with = "crate::serialize_optional_f32_with_two_decimal_places")]
    pub temperature: Option<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, MergeFrom)]
pub struct LanguageModelProviderSetting(pub String);

impl JsonSchema for LanguageModelProviderSetting {
    fn schema_name() -> Cow<'static, str> {
        "LanguageModelProviderSetting".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        // list the builtin providers as a subset so that we still auto complete them in the settings
        json_schema!({
            "anyOf": [
                {
                    "type": "string",
                    "enum": [
                        "amazon-bedrock",
                        "anthropic",
                        "copilot_chat",
                        "deepseek",
                        "google",
                        "lmstudio",
                        "mistral",
                        "ollama",
                        "openai",
                        "opencode",
                        "openrouter",
                        "vercel_ai_gateway",
                        "x_ai",
                        "zed.dev"
                    ]
                },
                {
                    "type": "string",
                }
            ]
        })
    }
}

impl From<String> for LanguageModelProviderSetting {
    fn from(provider: String) -> Self {
        Self(provider)
    }
}

impl From<&str> for LanguageModelProviderSetting {
    fn from(provider: &str) -> Self {
        Self(provider.to_string())
    }
}

#[with_fallible_options]
#[derive(Default, PartialEq, Deserialize, Serialize, Clone, JsonSchema, MergeFrom, Debug)]
#[serde(transparent)]
pub struct AllAgentServersSettings(pub HashMap<String, CustomAgentServerSettings>);

impl std::ops::Deref for AllAgentServersSettings {
    type Target = HashMap<String, CustomAgentServerSettings>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for AllAgentServersSettings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[with_fallible_options]
#[derive(Deserialize, Serialize, Clone, JsonSchema, MergeFrom, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CustomAgentServerSettings {
    Custom {
        #[serde(rename = "command")]
        path: PathBuf,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// The default mode to use for this agent.
        ///
        /// Note: Not only all agents support modes.
        ///
        /// Default: None
        default_mode: Option<String>,
        /// Default values for session config options.
        ///
        /// This is a map from config option ID to value ID.
        ///
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        default_config_options: HashMap<String, String>,
        /// Favorited values for session config options.
        ///
        /// This is a map from config option ID to a list of favorited value IDs.
        ///
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        favorite_config_option_values: HashMap<String, Vec<String>>,
    },
    // Used for the ACP extension migration
    #[serde(alias = "extension")]
    Registry {
        /// Additional environment variables to pass to the agent.
        ///
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        env: HashMap<String, String>,
        /// The default mode to use for this agent.
        ///
        /// Note: Not only all agents support modes.
        ///
        /// Default: None
        default_mode: Option<String>,
        /// Default values for session config options.
        ///
        /// This is a map from config option ID to value ID.
        ///
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        default_config_options: HashMap<String, String>,
        /// Favorited values for session config options.
        ///
        /// This is a map from config option ID to a list of favorited value IDs.
        ///
        /// Default: {}
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        favorite_config_option_values: HashMap<String, Vec<String>>,
    },
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct SandboxPermissionsContent {
    /// Whether sandboxed terminal commands may always use outbound network
    /// access without prompting.
    /// Default: false
    pub allow_network: Option<bool>,

    /// Whether sandboxed terminal commands may always write anywhere on the
    /// filesystem without prompting.
    /// Default: false
    pub allow_fs_write_all: Option<bool>,

    /// Whether terminal commands may always run outside the sandbox without
    /// prompting when they request `unsandboxed: true`.
    /// Default: false
    pub allow_unsandboxed: Option<bool>,

    /// Directory subtrees that sandboxed terminal commands may always write
    /// to without prompting. Paths written by LEAD are absolute.
    /// Default: []
    pub write_paths: Option<ExtendingVec<PathBuf>>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct FullAccessSettingsContent {
    /// Master switch for native whole-PC tools.
    /// Default: false
    pub enabled: Option<bool>,

    /// Additional absolute directory subtrees treated as trusted scope.
    /// Operations below these roots do not require an escape prompt.
    /// Default: []
    pub allowed_roots: Option<ExtendingVec<PathBuf>>,

    /// Absolute directory subtrees that native whole-PC tools may never touch.
    /// These augment LEAD's built-in catastrophic-path denylist.
    /// Default: []
    pub denied_roots: Option<ExtendingVec<PathBuf>>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ToolPermissionsContent {
    /// Global default permission when no tool-specific rules match.
    /// Individual tools can override this with their own default.
    /// Default: confirm
    #[serde(alias = "default_mode")]
    pub default: Option<ToolPermissionMode>,

    /// Per-tool permission rules.
    /// Keys are tool names (e.g. terminal, edit_file, fetch) including MCP
    /// tools (e.g. mcp:server_name:tool_name). Any tool name is accepted;
    /// even tools without meaningful text input can have a `default` set.
    #[serde(default)]
    pub tools: HashMap<Arc<str>, ToolRulesContent>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ToolRulesContent {
    /// Default mode when no regex rules match.
    /// When unset, inherits from the global `tool_permissions.default`.
    #[serde(alias = "default_mode")]
    pub default: Option<ToolPermissionMode>,

    /// Regexes for inputs to auto-approve.
    /// For terminal: matches command. For file tools: matches path. For fetch: matches URL.
    /// For `copy_path` and `move_path`, patterns are matched independently against each
    /// path (source and destination).
    /// Patterns accumulate across settings layers (user, project, profile) and cannot be
    /// removed by a higher-priority layer—only new patterns can be added.
    /// Default: []
    pub always_allow: Option<ExtendingVec<ToolRegexRule>>,

    /// Regexes for inputs to auto-reject.
    /// **SECURITY**: These take precedence over ALL other rules, across ALL settings layers.
    /// For `copy_path` and `move_path`, patterns are matched independently against each
    /// path (source and destination).
    /// Patterns accumulate across settings layers (user, project, profile) and cannot be
    /// removed by a higher-priority layer—only new patterns can be added.
    /// Default: []
    pub always_deny: Option<ExtendingVec<ToolRegexRule>>,

    /// Regexes for inputs that must always prompt.
    /// Takes precedence over always_allow but not always_deny.
    /// For `copy_path` and `move_path`, patterns are matched independently against each
    /// path (source and destination).
    /// Patterns accumulate across settings layers (user, project, profile) and cannot be
    /// removed by a higher-priority layer—only new patterns can be added.
    /// Default: []
    pub always_confirm: Option<ExtendingVec<ToolRegexRule>>,
}

#[with_fallible_options]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema, MergeFrom)]
pub struct ToolRegexRule {
    /// The regex pattern to match.
    #[serde(default)]
    pub pattern: String,

    /// Whether the regex is case-sensitive.
    /// Default: false (case-insensitive)
    pub case_sensitive: Option<bool>,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, MergeFrom,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermissionMode {
    /// Auto-approve without prompting.
    Allow,
    /// Auto-reject with an error.
    Deny,
    /// Always prompt for confirmation (default behavior).
    #[default]
    Confirm,
}

impl std::fmt::Display for ToolPermissionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ToolPermissionMode::Allow => write!(f, "Allow"),
            ToolPermissionMode::Deny => write!(f, "Deny"),
            ToolPermissionMode::Confirm => write!(f, "Confirm"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_tool_default_permission_creates_structure() {
        let mut settings = AgentSettingsContent::default();
        assert!(settings.tool_permissions.is_none());

        settings.set_tool_default_permission("terminal", ToolPermissionMode::Allow);

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal_rules.default, Some(ToolPermissionMode::Allow));
    }

    #[test]
    fn test_set_tool_default_permission_updates_existing() {
        let mut settings = AgentSettingsContent::default();

        settings.set_tool_default_permission("terminal", ToolPermissionMode::Confirm);
        settings.set_tool_default_permission("terminal", ToolPermissionMode::Allow);

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        assert_eq!(terminal_rules.default, Some(ToolPermissionMode::Allow));
    }

    #[test]
    fn test_set_tool_default_permission_for_mcp_tool() {
        let mut settings = AgentSettingsContent::default();

        settings.set_tool_default_permission("mcp:github:create_issue", ToolPermissionMode::Allow);

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let mcp_rules = tool_permissions
            .tools
            .get("mcp:github:create_issue")
            .unwrap();
        assert_eq!(mcp_rules.default, Some(ToolPermissionMode::Allow));
    }

    #[test]
    fn test_add_tool_allow_pattern_creates_structure() {
        let mut settings = AgentSettingsContent::default();
        assert!(settings.tool_permissions.is_none());

        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_allow = terminal_rules.always_allow.as_ref().unwrap();
        assert_eq!(always_allow.0.len(), 1);
        assert_eq!(always_allow.0[0].pattern, "^cargo\\s");
    }

    #[test]
    fn test_add_tool_allow_pattern_appends_to_existing() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());
        settings.add_tool_allow_pattern("terminal", "^npm\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_allow = terminal_rules.always_allow.as_ref().unwrap();
        assert_eq!(always_allow.0.len(), 2);
        assert_eq!(always_allow.0[0].pattern, "^cargo\\s");
        assert_eq!(always_allow.0[1].pattern, "^npm\\s");
    }

    #[test]
    fn test_add_tool_allow_pattern_does_not_duplicate() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());
        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());
        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_allow = terminal_rules.always_allow.as_ref().unwrap();
        assert_eq!(
            always_allow.0.len(),
            1,
            "Duplicate patterns should not be added"
        );
    }

    #[test]
    fn test_add_tool_allow_pattern_for_different_tools() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());
        settings.add_tool_allow_pattern("fetch", "^https?://github\\.com".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();

        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        assert_eq!(
            terminal_rules.always_allow.as_ref().unwrap().0[0].pattern,
            "^cargo\\s"
        );

        let fetch_rules = tool_permissions.tools.get("fetch").unwrap();
        assert_eq!(
            fetch_rules.always_allow.as_ref().unwrap().0[0].pattern,
            "^https?://github\\.com"
        );
    }

    #[test]
    fn test_add_tool_deny_pattern_creates_structure() {
        let mut settings = AgentSettingsContent::default();
        assert!(settings.tool_permissions.is_none());

        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_deny = terminal_rules.always_deny.as_ref().unwrap();
        assert_eq!(always_deny.0.len(), 1);
        assert_eq!(always_deny.0[0].pattern, "^rm\\s");
    }

    #[test]
    fn test_add_tool_deny_pattern_appends_to_existing() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());
        settings.add_tool_deny_pattern("terminal", "^sudo\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_deny = terminal_rules.always_deny.as_ref().unwrap();
        assert_eq!(always_deny.0.len(), 2);
        assert_eq!(always_deny.0[0].pattern, "^rm\\s");
        assert_eq!(always_deny.0[1].pattern, "^sudo\\s");
    }

    #[test]
    fn test_add_tool_deny_pattern_does_not_duplicate() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());
        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());
        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();
        let always_deny = terminal_rules.always_deny.as_ref().unwrap();
        assert_eq!(
            always_deny.0.len(),
            1,
            "Duplicate patterns should not be added"
        );
    }

    #[test]
    fn test_add_tool_deny_and_allow_patterns_separate() {
        let mut settings = AgentSettingsContent::default();

        settings.add_tool_allow_pattern("terminal", "^cargo\\s".to_string());
        settings.add_tool_deny_pattern("terminal", "^rm\\s".to_string());

        let tool_permissions = settings.tool_permissions.as_ref().unwrap();
        let terminal_rules = tool_permissions.tools.get("terminal").unwrap();

        let always_allow = terminal_rules.always_allow.as_ref().unwrap();
        assert_eq!(always_allow.0.len(), 1);
        assert_eq!(always_allow.0[0].pattern, "^cargo\\s");

        let always_deny = terminal_rules.always_deny.as_ref().unwrap();
        assert_eq!(always_deny.0.len(), 1);
        assert_eq!(always_deny.0[0].pattern, "^rm\\s");
    }

    #[test]
    fn test_allow_sandbox_permissions_create_structure() {
        let mut settings = AgentSettingsContent::default();
        assert!(settings.sandbox_permissions.is_none());

        settings.allow_sandbox_network();
        settings.allow_sandbox_fs_write_all();
        settings.allow_sandbox_unsandboxed();
        settings.add_sandbox_write_path(PathBuf::from("/tmp/build"));

        let sandbox_permissions = settings.sandbox_permissions.as_ref().unwrap();
        assert_eq!(sandbox_permissions.allow_network, Some(true));
        assert_eq!(sandbox_permissions.allow_fs_write_all, Some(true));
        assert_eq!(sandbox_permissions.allow_unsandboxed, Some(true));
        assert_eq!(
            sandbox_permissions
                .write_paths
                .as_ref()
                .unwrap()
                .0
                .as_slice(),
            &[PathBuf::from("/tmp/build")]
        );
    }

    #[test]
    fn test_add_sandbox_write_path_prunes_redundant_paths() {
        let mut settings = AgentSettingsContent::default();

        settings.add_sandbox_write_path(PathBuf::from("/tmp/build/cache"));
        settings.add_sandbox_write_path(PathBuf::from("/tmp/build"));
        settings.add_sandbox_write_path(PathBuf::from("/tmp/build/output"));

        let write_paths = settings
            .sandbox_permissions
            .as_ref()
            .unwrap()
            .write_paths
            .as_ref()
            .unwrap()
            .0
            .as_slice();
        assert_eq!(write_paths, &[PathBuf::from("/tmp/build")]);
    }
}
