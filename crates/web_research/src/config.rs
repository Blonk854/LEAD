use std::path::PathBuf;
use std::time::Duration;

/// Runtime configuration for local web research.
#[derive(Clone, Debug, PartialEq)]
pub struct WebResearchConfig {
    pub enabled: bool,
    pub max_search_results: usize,
    pub max_snippet_chars: usize,
    pub max_fetch_chars: usize,
    pub max_response_bytes: u64,
    pub max_redirects: u32,
    pub request_timeout: Duration,
    /// Reserved for future parallel research fetches (currently sequential).
    pub max_concurrency: usize,
    pub per_host_delay: Duration,
    pub cache_ttl_serp: Duration,
    pub cache_ttl_page: Duration,
    /// App-data web cache root. When set, successful HTTP page extracts are cached.
    pub cache_dir: Option<PathBuf>,
    pub browser_fallback_enabled: bool,
    /// Isolated Chromium profile directory (app data). Required when browser fallback runs.
    pub browser_profile_dir: Option<PathBuf>,
    /// Download quarantine directory (never executed).
    pub browser_download_dir: Option<PathBuf>,
    pub browser_timeout: Duration,
    pub max_pages: usize,
    pub max_depth: usize,
    pub research_wall_clock: Duration,
}

impl Default for WebResearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_search_results: 5,
            max_snippet_chars: 300,
            max_fetch_chars: 12_000,
            max_response_bytes: 1024 * 1024,
            max_redirects: 5,
            request_timeout: Duration::from_millis(15_000),
            max_concurrency: 2,
            per_host_delay: Duration::from_millis(1_000),
            cache_ttl_serp: Duration::from_secs(3_600),
            cache_ttl_page: Duration::from_secs(604_800),
            cache_dir: None,
            browser_fallback_enabled: false,
            browser_profile_dir: None,
            browser_download_dir: None,
            browser_timeout: Duration::from_secs(45),
            max_pages: 6,
            max_depth: 1,
            research_wall_clock: Duration::from_secs(90),
        }
    }
}
