//! Local web research primitives: URL policy, HTTP fetch, extraction, envelopes,
//! caching, rate limiting, and SERP discovery adapters.
//!
//! This crate has no GPUI dependency so it can be unit-tested and reused by
//! agent tools and web search providers.

mod browser;
mod cache;
mod config;
mod discovery;
mod envelope;
mod extract;
mod fetch;
mod rate_limit;
mod session;
mod store;
mod url_policy;

pub use browser::{
    BrowserError, BrowserFetchRequest, BrowserFetchResult, DiscoveredBrowser,
    discover_system_browser, dump_dom_args, fetch_rendered_html, prepare_browser_dirs,
};
pub use cache::{DiskCache, disk_cache_dir};
pub use config::WebResearchConfig;
pub use discovery::{
    DiscoveryAdapter, DiscoveryHealth, DiscoveryHit, DiscoveryOutcome, FailoverDiscovery,
    extract_result_links, looks_like_challenge,
};
pub use envelope::{render_search_results_for_model, render_web_envelope, sanitize_web_content};
pub use extract::{ExtractedPage, extract_page_content, heading_outline, truncate_for_model};
pub use fetch::{FetchError, FetchMode, FetchOutcome, FetchedPage, PageFetcher};
pub use rate_limit::HostRateLimiter;
pub use session::{
    ResearchBudgets, ResearchClaim, ResearchCitation, ResearchDigest, ResearchPageBody,
    ResearchSeed, ResearchSession, ResearchSessionOutcome, ResearchSkip,
};
pub use store::{
    SessionSummary, StoredResearchSession, list_sessions, load_session, new_session_id,
    render_session_list, save_session, sessions_dir, web_rag_source,
};
pub use url_policy::{
    UrlPolicyError, canonicalize_url, ensure_public_http_url, is_blocked_ip, normalize_url_string,
};
