use agent_client_protocol::schema as acp;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PromptInputMode {
    #[default]
    Chat,
    Goal,
}

impl PromptInputMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Goal => "Goal",
        }
    }
}

pub fn apply_goal_mode_to_contents(contents: Vec<acp::ContentBlock>) -> Vec<acp::ContentBlock> {
    let prefixed = match contents.first() {
        Some(acp::ContentBlock::Text(text)) => {
            let trimmed = text.text.trim();
            if trimmed.starts_with("/goal") {
                return contents;
            }
            Some(format!("/goal {trimmed}"))
        }
        _ => None,
    };

    let Some(prefixed) = prefixed else {
        return contents;
    };

    let mut blocks = contents;
    blocks[0] = acp::ContentBlock::Text(acp::TextContent::new(prefixed));
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_goal_mode_prefixes_plain_text() {
        let contents = vec![acp::ContentBlock::Text(acp::TextContent::new(
            "read README".to_string(),
        ))];
        let result = apply_goal_mode_to_contents(contents);
        let acp::ContentBlock::Text(text) = &result[0] else {
            panic!("expected text block");
        };
        assert_eq!(text.text, "/goal read README");
    }

    #[test]
    fn apply_goal_mode_skips_existing_goal_command() {
        let contents = vec![acp::ContentBlock::Text(acp::TextContent::new(
            "/goal pause".to_string(),
        ))];
        let result = apply_goal_mode_to_contents(contents);
        let acp::ContentBlock::Text(text) = &result[0] else {
            panic!("expected text block");
        };
        assert_eq!(text.text, "/goal pause");
    }
}
