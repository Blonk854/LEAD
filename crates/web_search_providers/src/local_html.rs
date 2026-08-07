use web_research::{
    DiscoveryAdapter, DiscoveryHealth, DiscoveryHit, DiscoveryOutcome, extract_result_links,
    looks_like_challenge,
};

/// DuckDuckGo HTML SERP parser (`html.duckduckgo.com`-style result pages).
pub struct DuckDuckGoHtmlAdapter {
    pub max_results: usize,
}

impl Default for DuckDuckGoHtmlAdapter {
    fn default() -> Self {
        Self { max_results: 5 }
    }
}

impl DiscoveryAdapter for DuckDuckGoHtmlAdapter {
    fn id(&self) -> &str {
        "duckduckgo_html"
    }

    fn search(&self, _query: &str, html: &str) -> DiscoveryOutcome {
        if looks_like_challenge(html) {
            return DiscoveryOutcome {
                adapter_id: self.id().into(),
                health: DiscoveryHealth::Challenge,
                hits: Vec::new(),
                detail: Some("challenge or captcha page".into()),
            };
        }

        // Prefer result anchors; skip duckduckgo chrome links.
        let mut hits = extract_ddg_results(html, self.max_results);
        if hits.is_empty() {
            hits = extract_result_links(
                html,
                &[
                    "duckduckgo.com",
                    "javascript:",
                    "mailto:",
                    "duck.co",
                ],
                self.max_results,
            );
        }

        let health = if hits.is_empty() {
            if html.len() < 500 {
                DiscoveryHealth::SoftBlock
            } else {
                DiscoveryHealth::Empty
            }
        } else {
            DiscoveryHealth::Ok
        };

        DiscoveryOutcome {
            adapter_id: self.id().into(),
            health,
            hits,
            detail: None,
        }
    }
}

/// Bing HTML SERP parser (fallback).
pub struct BingHtmlAdapter {
    pub max_results: usize,
}

impl Default for BingHtmlAdapter {
    fn default() -> Self {
        Self { max_results: 5 }
    }
}

impl DiscoveryAdapter for BingHtmlAdapter {
    fn id(&self) -> &str {
        "bing_html"
    }

    fn search(&self, _query: &str, html: &str) -> DiscoveryOutcome {
        if looks_like_challenge(html) {
            return DiscoveryOutcome {
                adapter_id: self.id().into(),
                health: DiscoveryHealth::Challenge,
                hits: Vec::new(),
                detail: Some("challenge or captcha page".into()),
            };
        }

        let hits = extract_result_links(
            html,
            &[
                "bing.com",
                "microsoft.com",
                "javascript:",
                "mailto:",
                "go.microsoft.com",
            ],
            self.max_results,
        );

        let health = if hits.is_empty() {
            if html.to_ascii_lowercase().contains("captcha") {
                DiscoveryHealth::Challenge
            } else if html.len() < 500 {
                DiscoveryHealth::SoftBlock
            } else {
                DiscoveryHealth::Empty
            }
        } else {
            DiscoveryHealth::Ok
        };

        DiscoveryOutcome {
            adapter_id: self.id().into(),
            health,
            hits,
            detail: None,
        }
    }
}

/// Startpage HTML SERP parser (privacy proxy over Google results; failover).
pub struct StartpageHtmlAdapter {
    pub max_results: usize,
}

impl Default for StartpageHtmlAdapter {
    fn default() -> Self {
        Self { max_results: 5 }
    }
}

impl DiscoveryAdapter for StartpageHtmlAdapter {
    fn id(&self) -> &str {
        "startpage_html"
    }

    fn search(&self, _query: &str, html: &str) -> DiscoveryOutcome {
        if looks_like_challenge(html) {
            return DiscoveryOutcome {
                adapter_id: self.id().into(),
                health: DiscoveryHealth::Challenge,
                hits: Vec::new(),
                detail: Some("challenge or captcha page".into()),
            };
        }

        // Startpage search results frequently use relative `href` values like:
        //   href="/sp/click?...". We convert those to absolute URLs so the
        // web fetch tool can follow redirects to the final destination.
        let hits = extract_startpage_result_links(html, self.max_results);

        let health = if hits.is_empty() {
            if html.to_ascii_lowercase().contains("captcha") {
                DiscoveryHealth::Challenge
            } else if html.len() < 500 {
                DiscoveryHealth::SoftBlock
            } else {
                DiscoveryHealth::Empty
            }
        } else {
            DiscoveryHealth::Ok
        };

        DiscoveryOutcome {
            adapter_id: self.id().into(),
            health,
            hits,
            detail: None,
        }
    }
}

fn extract_ddg_results(html: &str, max_results: usize) -> Vec<DiscoveryHit> {
    // DuckDuckGo HTML results often look like:
    // <a rel="nofollow" class="result__a" href="https://...">Title</a>
    // or (current html.duckduckgo.com):
    // <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2F...">Title</a>
    // <a class="result__snippet" ...>snippet</a> or <td class="result__snippet">
    let mut hits = Vec::new();
    let mut search_from = 0usize;
    while hits.len() < max_results {
        let Some(class_rel) = html[search_from..].find("result__a") else {
            break;
        };
        let class_at = search_from + class_rel;
        // Walk backward to the opening <a
        let window_start = class_at.saturating_sub(120);
        let Some(anchor_rel) = html[window_start..class_at].rfind("<a ") else {
            search_from = class_at + 9;
            continue;
        };
        let anchor_start = window_start + anchor_rel;
        let Some(href_rel) = html[anchor_start..].find("href=\"") else {
            search_from = class_at + 9;
            continue;
        };
        let href_start = anchor_start + href_rel + 6;
        let Some(href_end_rel) = html[href_start..].find('"') else {
            break;
        };
        let href_end = href_start + href_end_rel;
        let href_raw = html[href_start..href_end].to_string();
        let Some(gt) = html[href_end..].find('>') else {
            search_from = href_end;
            continue;
        };
        let text_start = href_end + gt + 1;
        let Some(text_end_rel) = html[text_start..].find("</a>") else {
            search_from = text_start;
            continue;
        };
        let text_end = text_start + text_end_rel;
        let title = strip_tags_simple(&html[text_start..text_end]);
        search_from = text_end + 4;

        let Some(url) = resolve_ddg_result_url(&href_raw) else {
            continue;
        };
        // Skip DDG chrome / about links that are not redirect wrappers.
        if is_duckduckgo_host_url(&url) {
            continue;
        }
        if hits.iter().any(|hit: &DiscoveryHit| hit.url == url) {
            continue;
        }

        let snippet_window = &html[search_from..(search_from + 500).min(html.len())];
        let snippet = if let Some(snippet_at) = snippet_window.find("result__snippet") {
            let after = &snippet_window[snippet_at..];
            if let Some(gt) = after.find('>') {
                let start = gt + 1;
                let end = after[start..]
                    .find('<')
                    .map(|index| start + index)
                    .unwrap_or(after.len());
                strip_tags_simple(&after[start..end])
            } else {
                String::new()
            }
        } else {
            String::new()
        };

        hits.push(DiscoveryHit {
            title: if title.is_empty() {
                url.clone()
            } else {
                title
            },
            url,
            snippet,
        });
    }
    hits
}

/// Normalize a DuckDuckGo result href into a fetchable destination URL.
///
/// Live `html.duckduckgo.com` pages use protocol-relative redirect wrappers:
/// `//duckduckgo.com/l/?uddg=<url-encoded-destination>&rut=...`
fn resolve_ddg_result_url(href: &str) -> Option<String> {
    let href = href.trim();
    if href.is_empty() {
        return None;
    }

    // Absolute destination already.
    if href.starts_with("http://") || href.starts_with("https://") {
        if is_duckduckgo_redirect_wrapper(href) {
            return extract_ddg_uddg_destination(href);
        }
        return Some(href.to_string());
    }

    // Protocol-relative wrapper or destination.
    if let Some(rest) = href.strip_prefix("//") {
        let absolute = format!("https://{rest}");
        if is_duckduckgo_redirect_wrapper(&absolute) {
            return extract_ddg_uddg_destination(&absolute);
        }
        return Some(absolute);
    }

    None
}

fn is_duckduckgo_host_url(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    let host = parsed.host_str().unwrap_or("").to_ascii_lowercase();
    host == "duckduckgo.com" || host.ends_with(".duckduckgo.com")
}

fn is_duckduckgo_redirect_wrapper(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    if !is_duckduckgo_host_url(url) {
        return false;
    }
    parsed.path().starts_with("/l/")
}

fn extract_ddg_uddg_destination(wrapper: &str) -> Option<String> {
    // HTML entities: href may contain `&amp;` between query params.
    let normalized = wrapper.replace("&amp;", "&");
    let parsed = url::Url::parse(&normalized).ok()?;
    for (key, value) in parsed.query_pairs() {
        if key == "uddg" {
            let dest = value.trim();
            if dest.starts_with("http://") || dest.starts_with("https://") {
                return Some(dest.to_string());
            }
        }
    }
    None
}

fn strip_tags_simple(input: &str) -> String {
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
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn extract_startpage_result_links(html: &str, max_results: usize) -> Vec<DiscoveryHit> {
    let mut hits = Vec::new();
    let mut search_from = 0usize;

    // Match Startpage click routing.
    //
    // Startpage often returns search results via a click wrapper like:
    //   /sp/click?url=<target_url_encoded>
    // where the actual destination can be recovered from the `url` query param.
    // We extract the destination directly so `search_web` returns fetchable URLs.
    let link_contains = ["startpage.com/sp/click", "support.startpage.com"];

    while hits.len() < max_results {
        let Some(href_rel) = html[search_from..].find("href=\"") else {
            break;
        };
        let href_start = search_from + href_rel + 6;
        let Some(href_end_rel) = html[href_start..].find('"') else {
            break;
        };
        let href_end = href_start + href_end_rel;
        let href_raw = &html[href_start..href_end];
        search_from = href_end + 1;

        // Normalize Startpage-relative click URLs to absolute.
        let href = if href_raw.starts_with('/') {
            format!("https://www.startpage.com{href_raw}")
        } else if href_raw.starts_with("sp/") || href_raw.starts_with("sp/click") {
            format!("https://www.startpage.com/{href_raw}")
        } else {
            href_raw.to_string()
        };

        if !href.starts_with("http://") && !href.starts_with("https://") {
            continue;
        }

        let href_lc = href.to_ascii_lowercase();
        if href_lc.starts_with("javascript:") || href_lc.starts_with("mailto:") {
            continue;
        }

        if !link_contains.iter().any(|needle| href_lc.contains(needle)) {
            continue;
        }

        // Only treat Startpage click routes as candidates.
        if !href_lc.contains("/sp/click") {
            continue;
        }

        let destination = extract_startpage_click_destination(&href);
        let final_url = destination.unwrap_or_else(|| href.clone());

        if hits.iter().any(|hit: &DiscoveryHit| hit.url == final_url) {
            continue;
        }

        // Prefer nearby text as title/snippet.
        let window_end = (href_end + 400).min(html.len());
        let window = &html[href_end..window_end];
        let cleaned = strip_tags_simple(window);
        let title = cleaned
            .split_whitespace()
            .take(12)
            .collect::<Vec<_>>()
            .join(" ");
        let snippet = cleaned
            .split_whitespace()
            .take(40)
            .collect::<Vec<_>>()
            .join(" ");

        hits.push(DiscoveryHit {
            title: if title.is_empty() {
                final_url.clone()
            } else {
                title
            },
            url: final_url,
            snippet,
        });
    }

    hits
}

fn extract_startpage_click_destination(click_url: &str) -> Option<String> {
    let parsed = url::Url::parse(click_url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    if !(host == "startpage.com" || host == "www.startpage.com") {
        return None;
    }

    // Prefer the explicit `url=` destination param when present.
    let mut fallback = None;
    for (key, value) in parsed.query_pairs() {
        let v = value.trim();
        if !(v.starts_with("http://") || v.starts_with("https://")) {
            continue;
        }
        if key == "url" {
            return Some(v.to_string());
        }
        if fallback.is_none() {
            fallback = Some(v.to_string());
        }
    }
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddg_fixture() {
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result__a" href="https://docs.rs/gpui">gpui - Rust</a>
          <a class="result__snippet" href="https://docs.rs/gpui">GPU UI framework docs</a>
          <a rel="nofollow" class="result__a" href="https://duckduckgo.com/about">skip chrome</a>
        </body></html>
        "#;
        let adapter = DuckDuckGoHtmlAdapter::default();
        let outcome = adapter.search("gpui", html);
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits.len(), 1);
        assert_eq!(outcome.hits[0].url, "https://docs.rs/gpui");
        assert!(outcome.hits[0].snippet.contains("GPU UI"));
    }

    #[test]
    fn keeps_non_ddg_hosts_that_mention_duckduckgo_in_path() {
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result__a" href="https://example.com/posts/duckduckgo.com-review">review</a>
        </body></html>
        "#;
        let adapter = DuckDuckGoHtmlAdapter::default();
        let outcome = adapter.search("review", html);
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits[0].url, "https://example.com/posts/duckduckgo.com-review");
    }

    #[test]
    fn parses_ddg_uddg_redirect_wrappers() {
        // Live html.duckduckgo.com uses protocol-relative redirect links.
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fgpui%2Flatest%2Fgpui%2F&amp;rut=abc">gpui - Rust - Docs.rs</a>
          <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fgpui%2Flatest%2Fgpui%2F&amp;rut=abc">API documentation for the Rust gpui crate.</a>
          <a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fdocs.rs%2Fcrate%2Fgpui%2Flatest&amp;rut=def">gpui 0.2.2 - Docs.rs</a>
        </body></html>
        "#;
        let adapter = DuckDuckGoHtmlAdapter { max_results: 5 };
        let outcome = adapter.search("gpui", html);
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits.len(), 2);
        assert_eq!(outcome.hits[0].url, "https://docs.rs/gpui/latest/gpui/");
        assert_eq!(outcome.hits[1].url, "https://docs.rs/crate/gpui/latest");
        assert!(outcome.hits[0].snippet.contains("API documentation"));
    }

    #[test]
    fn challenge_detected() {
        let adapter = DuckDuckGoHtmlAdapter::default();
        let outcome = adapter.search(
            "x",
            r#"<html><div class="g-recaptcha"></div>captcha required</html>"#,
        );
        assert_eq!(outcome.health, DiscoveryHealth::Challenge);
    }

    #[test]
    fn snippet_mentioning_captcha_is_not_a_challenge() {
        let adapter = DuckDuckGoHtmlAdapter::default();
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result__a" href="https://docs.rs/gpui">how captcha works</a>
          <a class="result__snippet" href="https://docs.rs/gpui">article about captcha UX</a>
        </body></html>
        "#;
        let outcome = adapter.search("captcha", html);
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits[0].url, "https://docs.rs/gpui");
    }

    #[test]
    fn startpage_anubis_challenge_detected() {
        let adapter = StartpageHtmlAdapter::default();
        let html = r#"
        <html><body>
          <script id="anubis_challenge" type="application/json">{"difficulty":4}</script>
          <div class="sp-message">Verifying your request...</div>
        </body></html>
        "#;
        let outcome = adapter.search("gpui", html);
        assert_eq!(outcome.health, DiscoveryHealth::Challenge);
    }

    #[test]
    fn parses_startpage_relative_links() {
        let adapter = StartpageHtmlAdapter { max_results: 5 };
        let html = r#"
        <html><body>
          <a href="/sp/click?url=https%3A%2F%2Fdocs.rs%2Fgpui">gpui docs - Rust</a>
          <a href="https://startpage.com/sp/click?url=https%3A%2F%2Fdocs.rs%2Fgpui&amp;other=https%3A%2F%2Fevil.example">dup same dest</a>
          <a href="https://www.startpage.com/sp/click?url=https%3A%2F%2Fdocs.rs%2Fzed">gpui api</a>
        </body></html>
        "#;

        let outcome = adapter.search("gpui", html);
        assert_eq!(outcome.health, DiscoveryHealth::Ok);
        assert_eq!(outcome.hits.len(), 2);
        assert_eq!(outcome.hits[0].url, "https://docs.rs/gpui");
        assert_eq!(outcome.hits[1].url, "https://docs.rs/zed");
    }
}
