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

        let hits = extract_result_links(
            html,
            &[
                "startpage.com",
                "javascript:",
                "mailto:",
                "support.startpage.com",
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

fn extract_ddg_results(html: &str, max_results: usize) -> Vec<DiscoveryHit> {
    // DuckDuckGo HTML results often look like:
    // <a rel="nofollow" class="result__a" href="https://...">Title</a>
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
        let href = html[href_start..href_end].to_string();
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

        if !(href.starts_with("http://") || href.starts_with("https://")) {
            continue;
        }
        if href.contains("duckduckgo.com") {
            continue;
        }
        if hits.iter().any(|hit: &DiscoveryHit| hit.url == href) {
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
                href.clone()
            } else {
                title
            },
            url: href,
            snippet,
        });
    }
    hits
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ddg_fixture() {
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result__a" href="https://docs.rs/gpui">gpui - Rust</a>
          <a class="result__snippet" href="https://docs.rs/gpui">GPU UI framework docs</a>
          <a rel="nofollow" class="result__a" href="https://example.com/duckduckgo.com">skip</a>
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
    fn challenge_detected() {
        let adapter = DuckDuckGoHtmlAdapter::default();
        let outcome = adapter.search("x", "<html>captcha required</html>");
        assert_eq!(outcome.health, DiscoveryHealth::Challenge);
    }
}
