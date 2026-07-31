use std::cell::RefCell;
use std::rc::Rc;

use anyhow::{Context as _, Result, bail};
use html_to_markdown::{TagHandler, convert_html_to_markdown, markdown};

#[derive(Debug, Clone)]
pub struct ExtractedPage {
    pub title: Option<String>,
    pub markdown: String,
    pub outline: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContentType {
    Html,
    Plaintext,
    Json,
}

pub fn extract_page_content(
    url: &str,
    content_type_header: Option<&str>,
    body: &[u8],
    max_chars: usize,
) -> Result<ExtractedPage> {
    let content_type = classify_content_type(content_type_header);
    let markdown = match content_type {
        ContentType::Html => html_to_markdown(url, body)?,
        ContentType::Plaintext => std::str::from_utf8(body)
            .context("response body was not valid UTF-8")?
            .to_owned(),
        ContentType::Json => {
            let json: serde_json::Value =
                serde_json::from_slice(body).context("failed to parse JSON response")?;
            format!(
                "```json\n{}\n```",
                serde_json::to_string_pretty(&json).context("failed to format JSON")?
            )
        }
    };

    if markdown.trim().is_empty() {
        bail!("no textual content found");
    }

    let outline = heading_outline(&markdown, 24);
    let (markdown, truncated) = truncate_for_model(&markdown, max_chars);
    let title = first_heading(&markdown).or_else(|| first_heading(&outline));

    Ok(ExtractedPage {
        title,
        markdown,
        outline,
        truncated,
    })
}

pub fn heading_outline(markdown: &str, max_headings: usize) -> String {
    let mut lines = Vec::new();
    for line in markdown.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            lines.push(trimmed.to_string());
            if lines.len() >= max_headings {
                break;
            }
        }
    }
    lines.join("\n")
}

pub fn truncate_for_model(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 || text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    while !truncated.is_char_boundary(truncated.len()) {
        truncated.pop();
    }
    // Prefer cutting at a paragraph boundary when possible.
    if let Some(index) = truncated.rfind("\n\n") {
        if index > max_chars / 2 {
            truncated.truncate(index);
        }
    }
    truncated.push_str("\n\n[truncated: page content exceeded model budget; use outline/headings or fetch a more specific URL]\n");
    (truncated, true)
}

fn first_heading(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix('#')
            .map(|rest| rest.trim_start_matches('#').trim().to_string())
            .filter(|title| !title.is_empty())
    })
}

fn classify_content_type(header: Option<&str>) -> ContentType {
    let Some(header) = header else {
        return ContentType::Html;
    };
    let lower = header.to_ascii_lowercase();
    if lower.starts_with("text/plain") {
        ContentType::Plaintext
    } else if lower.starts_with("application/json") {
        ContentType::Json
    } else {
        ContentType::Html
    }
}

fn html_to_markdown(url: &str, body: &[u8]) -> Result<String> {
    let mut handlers: Vec<TagHandler> = vec![
        Rc::new(RefCell::new(markdown::WebpageChromeRemover)),
        Rc::new(RefCell::new(markdown::ParagraphHandler)),
        Rc::new(RefCell::new(markdown::HeadingHandler)),
        Rc::new(RefCell::new(markdown::ListHandler)),
        Rc::new(RefCell::new(markdown::TableHandler::new())),
        Rc::new(RefCell::new(markdown::StyledTextHandler)),
    ];
    if url.contains("wikipedia.org") {
        use html_to_markdown::structure::wikipedia;

        handlers.push(Rc::new(RefCell::new(wikipedia::WikipediaChromeRemover)));
        handlers.push(Rc::new(RefCell::new(wikipedia::WikipediaInfoboxHandler)));
        handlers.push(Rc::new(
            RefCell::new(wikipedia::WikipediaCodeHandler::new()),
        ));
    } else {
        handlers.push(Rc::new(RefCell::new(markdown::CodeHandler)));
    }
    convert_html_to_markdown(&body[..], &mut handlers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_with_marker() {
        let text = "a".repeat(100);
        let (out, truncated) = truncate_for_model(&text, 20);
        assert!(truncated);
        assert!(out.contains("[truncated:"));
        assert!(!out.contains(&"a".repeat(50)));
    }

    #[test]
    fn extracts_plaintext() {
        let page = extract_page_content(
            "https://example.com",
            Some("text/plain"),
            b"hello world",
            12_000,
        )
        .unwrap();
        assert_eq!(page.markdown, "hello world");
        assert!(!page.truncated);
    }
}
