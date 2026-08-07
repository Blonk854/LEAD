use anyhow::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryHealth {
    Ok,
    Empty,
    Challenge,
    SoftBlock,
    ParseError,
}

#[derive(Debug, Clone)]
pub struct DiscoveryOutcome {
    pub adapter_id: String,
    pub health: DiscoveryHealth,
    pub hits: Vec<DiscoveryHit>,
    pub detail: Option<String>,
}

pub trait DiscoveryAdapter: Send + Sync {
    fn id(&self) -> &str;
    fn search(&self, query: &str, html: &str) -> DiscoveryOutcome;
}

/// Try adapters in order until one returns usable hits.
pub struct FailoverDiscovery {
    adapters: Vec<Box<dyn DiscoveryAdapter>>,
}

impl FailoverDiscovery {
    pub fn new(adapters: Vec<Box<dyn DiscoveryAdapter>>) -> Self {
        Self { adapters }
    }

    pub fn parse_with_failover(&self, query: &str, pages: &[(&str, &str)]) -> Result<DiscoveryOutcome> {
        let mut last = DiscoveryOutcome {
            adapter_id: "none".into(),
            health: DiscoveryHealth::Empty,
            hits: Vec::new(),
            detail: Some("no adapters configured".into()),
        };
        for (adapter_id, html) in pages {
            let Some(adapter) = self.adapters.iter().find(|adapter| adapter.id() == *adapter_id)
            else {
                continue;
            };
            let outcome = adapter.search(query, html);
            if outcome.health == DiscoveryHealth::Ok && !outcome.hits.is_empty() {
                return Ok(outcome);
            }
            last = outcome;
        }
        Ok(last)
    }

    pub fn adapters(&self) -> &[Box<dyn DiscoveryAdapter>] {
        &self.adapters
    }
}

/// Extract anchor hits from simple HTML SERP pages.
pub fn extract_result_links(
    html: &str,
    link_contains: &[&str],
    max_results: usize,
) -> Vec<DiscoveryHit> {
    let mut hits = Vec::new();
    let lower = html;
    let mut search_from = 0usize;
    while hits.len() < max_results {
        let Some(href_rel) = lower[search_from..].find("href=\"") else {
            break;
        };
        let href_start = search_from + href_rel + 6;
        let Some(href_end_rel) = lower[href_start..].find('"') else {
            break;
        };
        let href_end = href_start + href_end_rel;
        let href = &lower[href_start..href_end];
        search_from = href_end + 1;

        if !href.starts_with("http://") && !href.starts_with("https://") {
            continue;
        }
        if link_contains
            .iter()
            .any(|needle| href.to_ascii_lowercase().contains(needle))
        {
            continue;
        }
        if hits.iter().any(|hit: &DiscoveryHit| hit.url == href) {
            continue;
        }

        // Prefer nearby text as title/snippet.
        let window_end = (href_end + 400).min(lower.len());
        let window = &lower[href_end..window_end];
        let title = strip_tags(window)
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        let title = if title.is_empty() {
            href.to_string()
        } else {
            title
        };
        let snippet = strip_tags(window)
            .split_whitespace()
            .take(40)
            .collect::<Vec<_>>()
            .join(" ");
        hits.push(DiscoveryHit {
            title,
            url: href.to_string(),
            snippet,
        });
    }
    hits
}

pub fn looks_like_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    // Prefer structural markers over English copy so SERP result snippets
    // mentioning "captcha" / "verifying your request" do not discard real hits.
    lower.contains("cf-browser-verification")
        || lower.contains("anubis_challenge")
        || lower.contains("challenges.cloudflare.com/turnstile")
        || lower.contains("id=\"turnstile-widget\"")
        || lower.contains("unusual traffic")
        || lower.contains("enable javascript and cookies")
        // Keep bare "captcha" only when paired with challenge chrome, not alone
        // in a normal results page body.
        || (lower.contains("captcha")
            && (lower.contains("turnstile")
                || lower.contains("cf-")
                || lower.contains("g-recaptcha")
                || lower.contains("hcaptcha")
                || lower.contains("please solve")))
}

fn strip_tags(input: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    html_unescape_basic(&out)
}

fn html_unescape_basic(input: &str) -> String {
    input
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyAdapter;

    impl DiscoveryAdapter for DummyAdapter {
        fn id(&self) -> &str {
            "dummy"
        }

        fn search(&self, _query: &str, html: &str) -> DiscoveryOutcome {
            if looks_like_challenge(html) {
                return DiscoveryOutcome {
                    adapter_id: self.id().into(),
                    health: DiscoveryHealth::Challenge,
                    hits: Vec::new(),
                    detail: Some("challenge".into()),
                };
            }
            let hits = extract_result_links(html, &["duckduckgo.com"], 5);
            DiscoveryOutcome {
                adapter_id: self.id().into(),
                health: if hits.is_empty() {
                    DiscoveryHealth::Empty
                } else {
                    DiscoveryHealth::Ok
                },
                hits,
                detail: None,
            }
        }
    }

    #[test]
    fn failover_skips_challenge() {
        let discovery = FailoverDiscovery::new(vec![Box::new(DummyAdapter)]);
        let challenge = r#"<html><div id="turnstile-widget"></div>captcha please</html>"#;
        let ok = r#"<a href="https://docs.rs/gpui">gpui docs</a>"#;
        let outcome = discovery
            .parse_with_failover("gpui", &[("dummy", challenge), ("dummy", ok)])
            .unwrap();
        // Second call uses same adapter id; parse_with_failover iterates pages.
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits[0].url, "https://docs.rs/gpui");
    }
}
