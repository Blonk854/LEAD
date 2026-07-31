use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::role::Role;
use crate::{LanguageModelToolUse, LanguageModelToolUseId, SharedString};

/// Dimensions of a `LanguageModelImage`
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ImageSize {
    pub width: i32,
    pub height: i32,
}

fn default_image_mime_type() -> SharedString {
    SharedString::from("image/png")
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub struct LanguageModelImage {
    /// Base64-encoded image bytes (without the `data:` prefix).
    pub source: SharedString,
    /// Pixel dimensions of the encoded image. Zero means unknown (legacy data).
    #[serde(default)]
    pub size: ImageSize,
    /// MIME type of the encoded bytes, e.g. `image/png` or `image/jpeg`.
    #[serde(default = "default_image_mime_type")]
    pub mime_type: SharedString,
}

impl LanguageModelImage {
    pub fn len(&self) -> usize {
        self.source.len()
    }

    pub fn is_empty(&self) -> bool {
        self.source.is_empty()
    }

    pub fn empty() -> Self {
        Self {
            source: "".into(),
            size: ImageSize::default(),
            mime_type: default_image_mime_type(),
        }
    }

    pub fn from_source(source: impl Into<SharedString>) -> Self {
        Self {
            source: source.into(),
            size: ImageSize::default(),
            mime_type: default_image_mime_type(),
        }
    }

    /// OpenAI-style vision token estimate from image dimensions.
    ///
    /// `tokens ≈ 85 + 170 * ceil(w/512) * ceil(h/512)`. Falls back to a
    /// conservative constant when dimensions are unknown so base64 byte length
    /// is never mistaken for text tokens.
    pub fn estimate_tokens(&self) -> u64 {
        estimate_image_tokens(self.size)
    }

    /// Parse Self from a JSON object with case-insensitive field names
    pub fn from_json(obj: &serde_json::Map<String, serde_json::Value>) -> Option<Self> {
        let mut source = None;
        let mut width = None;
        let mut height = None;
        let mut mime_type = None;

        for (k, v) in obj.iter() {
            match k.to_lowercase().as_str() {
                "source" => source = v.as_str(),
                "mime_type" | "mimetype" => mime_type = v.as_str(),
                "size" => {
                    if let Some(size_obj) = v.as_object() {
                        for (sk, sv) in size_obj.iter() {
                            match sk.to_lowercase().as_str() {
                                "width" => width = sv.as_i64().map(|n| n as i32),
                                "height" => height = sv.as_i64().map(|n| n as i32),
                                _ => {}
                            }
                        }
                    }
                }
                "width" => width = v.as_i64().map(|n| n as i32),
                "height" => height = v.as_i64().map(|n| n as i32),
                _ => {}
            }
        }

        let source = source?;
        Some(Self {
            source: SharedString::from(source.to_string()),
            size: ImageSize {
                width: width.unwrap_or(0),
                height: height.unwrap_or(0),
            },
            mime_type: mime_type
                .map(|value| SharedString::from(value.to_string()))
                .unwrap_or_else(default_image_mime_type),
        })
    }

    pub fn to_base64_url(&self) -> String {
        format!("data:{};base64,{}", self.mime_type, self.source)
    }
}

/// Estimate vision tokens for an image using an OpenAI-style 512px tile formula.
pub fn estimate_image_tokens(size: ImageSize) -> u64 {
    if size.width <= 0 || size.height <= 0 {
        return 1_100;
    }
    let tiles_x = (size.width as u64).div_ceil(512);
    let tiles_y = (size.height as u64).div_ceil(512);
    85 + 170 * tiles_x * tiles_y
}

impl std::fmt::Debug for LanguageModelImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageModelImage")
            .field("source", &format!("<{} bytes>", self.source.len()))
            .field("size", &self.size)
            .field("mime_type", &self.mime_type)
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub struct LanguageModelToolResult {
    pub tool_use_id: LanguageModelToolUseId,
    pub tool_name: Arc<str>,
    pub is_error: bool,
    #[serde(with = "tool_result_content_vec")]
    pub content: Vec<LanguageModelToolResultContent>,
    /// The raw tool output, if available, often for debugging or extra state for replay
    pub output: Option<serde_json::Value>,
}

impl LanguageModelToolResult {
    /// Concatenates all `Text` parts of the content, ignoring non-text parts.
    pub fn text_contents(&self) -> String {
        let mut buffer = String::new();
        for part in &self.content {
            if let LanguageModelToolResultContent::Text(text) = part {
                buffer.push_str(text);
            }
        }
        buffer
    }

    /// Returns true when there are no content parts, or every part is empty.
    pub fn is_content_empty(&self) -> bool {
        self.content.iter().all(|part| part.is_empty())
    }
}

/// Serde helper that accepts both the legacy single-value shape and the new
/// array shape for `LanguageModelToolResult::content`, and normalizes both to
/// `Vec<LanguageModelToolResultContent>`.
mod tool_result_content_vec {
    use super::LanguageModelToolResultContent;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(
        value: &Vec<LanguageModelToolResultContent>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<Vec<LanguageModelToolResultContent>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match value {
            serde_json::Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(
                        serde_json::from_value::<LanguageModelToolResultContent>(item)
                            .map_err(serde::de::Error::custom)?,
                    );
                }
                Ok(out)
            }
            other => {
                let single = serde_json::from_value::<LanguageModelToolResultContent>(other)
                    .map_err(serde::de::Error::custom)?;
                Ok(vec![single])
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Eq, PartialEq, Hash)]
pub enum LanguageModelToolResultContent {
    Text(Arc<str>),
    Image(LanguageModelImage),
}

impl<'de> Deserialize<'de> for LanguageModelToolResultContent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        let value = serde_json::Value::deserialize(deserializer)?;

        // 1. Try as plain string
        if let Ok(text) = serde_json::from_value::<String>(value.clone()) {
            return Ok(Self::Text(Arc::from(text)));
        }

        // 2. Try as object
        if let Some(obj) = value.as_object() {
            fn get_field<'a>(
                obj: &'a serde_json::Map<String, serde_json::Value>,
                field: &str,
            ) -> Option<&'a serde_json::Value> {
                obj.iter()
                    .find(|(k, _)| k.to_lowercase() == field.to_lowercase())
                    .map(|(_, v)| v)
            }

            // Accept wrapped text format: { "type": "text", "text": "..." }
            if let (Some(type_value), Some(text_value)) =
                (get_field(obj, "type"), get_field(obj, "text"))
                && let Some(type_str) = type_value.as_str()
                && type_str.to_lowercase() == "text"
                && let Some(text) = text_value.as_str()
            {
                return Ok(Self::Text(Arc::from(text)));
            }

            // Check for wrapped Text variant: { "text": "..." }
            if let Some((_key, value)) = obj.iter().find(|(k, _)| k.to_lowercase() == "text")
                && obj.len() == 1
            {
                if let Some(text) = value.as_str() {
                    return Ok(Self::Text(Arc::from(text)));
                }
            }

            // Check for wrapped Image variant: { "image": { "source": "...", "size": ... } }
            if let Some((_key, value)) = obj.iter().find(|(k, _)| k.to_lowercase() == "image")
                && obj.len() == 1
            {
                if let Some(image_obj) = value.as_object()
                    && let Some(image) = LanguageModelImage::from_json(image_obj)
                {
                    return Ok(Self::Image(image));
                }
            }

            // Try as direct Image
            if let Some(image) = LanguageModelImage::from_json(obj) {
                return Ok(Self::Image(image));
            }
        }

        Err(D::Error::custom(format!(
            "data did not match any variant of LanguageModelToolResultContent. Expected either a string, \
             an object with 'type': 'text', a wrapped variant like {{\"Text\": \"...\"}}, or an image object. Got: {}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string())
        )))
    }
}

impl LanguageModelToolResultContent {
    pub fn to_str(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Image(_) => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Text(text) => text.chars().all(|c| c.is_whitespace()),
            Self::Image(_) => false,
        }
    }
}

impl From<&str> for LanguageModelToolResultContent {
    fn from(value: &str) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<String> for LanguageModelToolResultContent {
    fn from(value: String) -> Self {
        Self::Text(Arc::from(value))
    }
}

impl From<anyhow::Error> for LanguageModelToolResultContent {
    fn from(error: anyhow::Error) -> Self {
        Self::Text(Arc::from(error.to_string()))
    }
}

impl From<LanguageModelImage> for LanguageModelToolResultContent {
    fn from(image: LanguageModelImage) -> Self {
        Self::Image(image)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, Hash)]
pub enum MessageContent {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    RedactedThinking(String),
    Image(LanguageModelImage),
    ToolUse(LanguageModelToolUse),
    ToolResult(LanguageModelToolResult),
}

impl MessageContent {
    pub fn is_empty(&self) -> bool {
        match self {
            MessageContent::Text(text) => text.chars().all(|c| c.is_whitespace()),
            MessageContent::Thinking { text, .. } => text.chars().all(|c| c.is_whitespace()),
            MessageContent::ToolResult(tool_result) => tool_result.is_content_empty(),
            MessageContent::RedactedThinking(_)
            | MessageContent::ToolUse(_)
            | MessageContent::Image(_) => false,
        }
    }
}

impl From<String> for MessageContent {
    fn from(value: String) -> Self {
        MessageContent::Text(value)
    }
}

impl From<&str> for MessageContent {
    fn from(value: &str) -> Self {
        MessageContent::Text(value.to_string())
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Hash)]
pub struct LanguageModelRequestMessage {
    pub role: Role,
    pub content: Vec<MessageContent>,
    pub cache: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Arc<serde_json::Value>>,
}

impl LanguageModelRequestMessage {
    pub fn string_contents(&self) -> String {
        let mut buffer = String::new();
        for content in &self.content {
            match content {
                MessageContent::Text(text) => {
                    buffer.push_str(text);
                }
                MessageContent::Thinking { text, .. } => {
                    buffer.push_str(text);
                }
                MessageContent::ToolResult(tool_result) => {
                    for part in &tool_result.content {
                        if let LanguageModelToolResultContent::Text(text) = part {
                            buffer.push_str(text);
                        }
                    }
                }
                MessageContent::RedactedThinking(_)
                | MessageContent::ToolUse(_)
                | MessageContent::Image(_) => {}
            }
        }
        buffer
    }

    pub fn contents_empty(&self) -> bool {
        self.content.iter().all(|content| content.is_empty())
    }
}

#[derive(Debug, PartialEq, Hash, Clone, Serialize, Deserialize)]
pub struct LanguageModelRequestTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    pub use_input_streaming: bool,
}

#[derive(Debug, PartialEq, Hash, Clone, Serialize, Deserialize)]
pub enum LanguageModelToolChoice {
    Auto,
    Any,
    None,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionIntent {
    UserPrompt,
    Subagent,
    ToolResults,
    ThreadSummarization,
    ThreadContextSummarization,
    CreateFile,
    EditFile,
    InlineAssist,
    TerminalInlineAssist,
    GenerateGitCommitMessage,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct LanguageModelRequest {
    pub thread_id: Option<String>,
    pub prompt_id: Option<String>,
    pub intent: Option<CompletionIntent>,
    pub messages: Vec<LanguageModelRequestMessage>,
    pub tools: Vec<LanguageModelRequestTool>,
    pub tool_choice: Option<LanguageModelToolChoice>,
    pub stop: Vec<String>,
    pub temperature: Option<f32>,
    pub thinking_allowed: bool,
    pub thinking_effort: Option<String>,
    pub speed: Option<Speed>,
}

#[derive(
    Clone, Copy, Default, Debug, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    #[default]
    Standard,
    Fast,
}

impl Speed {
    pub fn toggle(self) -> Self {
        match self {
            Speed::Standard => Speed::Fast,
            Speed::Fast => Speed::Standard,
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Eq, PartialEq)]
pub struct LanguageModelResponseMessage {
    pub role: Option<Role>,
    pub content: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_language_model_tool_result_content_deserialization() {
        // Test plain string
        let json = serde_json::json!("hello world");
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        assert_eq!(
            content,
            LanguageModelToolResultContent::Text(Arc::from("hello world"))
        );

        // Test wrapped text format: { "type": "text", "text": "..." }
        let json = serde_json::json!({"type": "text", "text": "hello"});
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        assert_eq!(
            content,
            LanguageModelToolResultContent::Text(Arc::from("hello"))
        );

        // Test single-field text object: { "text": "..." }
        let json = serde_json::json!({"text": "hello"});
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        assert_eq!(
            content,
            LanguageModelToolResultContent::Text(Arc::from("hello"))
        );

        // Test case-insensitive type field
        let json = serde_json::json!({"Type": "Text", "Text": "hello"});
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        assert_eq!(
            content,
            LanguageModelToolResultContent::Text(Arc::from("hello"))
        );

        // Test image object
        let json = serde_json::json!({
            "source": "base64encodedimagedata",
        });
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        match content {
            LanguageModelToolResultContent::Image(image) => {
                assert_eq!(image.source.as_ref(), "base64encodedimagedata");
            }
            _ => panic!("Expected Image variant"),
        }

        // Test wrapped image: { "image": { "source": "...", "size": ... } }
        let json = serde_json::json!({
            "image": {
                "source": "wrappedimagedata",
            }
        });
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        match content {
            LanguageModelToolResultContent::Image(image) => {
                assert_eq!(image.source.as_ref(), "wrappedimagedata");
            }
            _ => panic!("Expected Image variant"),
        }

        // Test case insensitive
        let json = serde_json::json!({
            "Source": "caseinsensitive",
        });
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        match content {
            LanguageModelToolResultContent::Image(image) => {
                assert_eq!(image.source.as_ref(), "caseinsensitive");
            }
            _ => panic!("Expected Image variant"),
        }

        // Test direct image object
        let json = serde_json::json!({
            "source": "directimage",
        });
        let content: LanguageModelToolResultContent = serde_json::from_value(json).unwrap();
        match content {
            LanguageModelToolResultContent::Image(image) => {
                assert_eq!(image.source.as_ref(), "directimage");
            }
            _ => panic!("Expected Image variant"),
        }
    }

    #[test]
    fn test_language_model_tool_result_content_vec_deserialization() {
        // Legacy single-value shape is normalized to a Vec.
        let json = serde_json::json!({
            "tool_use_id": "abc",
            "tool_name": "echo",
            "is_error": false,
            "content": "hello",
            "output": null,
        });
        let result: LanguageModelToolResult = serde_json::from_value(json).unwrap();
        assert_eq!(
            result.content,
            vec![LanguageModelToolResultContent::Text(Arc::from("hello"))]
        );

        // Legacy wrapped single-value shape also works.
        let json = serde_json::json!({
            "tool_use_id": "abc",
            "tool_name": "echo",
            "is_error": false,
            "content": {"type": "text", "text": "hello"},
            "output": null,
        });
        let result: LanguageModelToolResult = serde_json::from_value(json).unwrap();
        assert_eq!(
            result.content,
            vec![LanguageModelToolResultContent::Text(Arc::from("hello"))]
        );

        // New array shape with text + image deserializes into a Vec.
        let json = serde_json::json!({
            "tool_use_id": "abc",
            "tool_name": "echo",
            "is_error": false,
            "content": [
                {"type": "text", "text": "foo"},
                {"source": "data", "size": {"width": 1, "height": 2}}
            ],
            "output": null,
        });
        let result: LanguageModelToolResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.content.len(), 2);
        assert_eq!(
            result.content[0],
            LanguageModelToolResultContent::Text(Arc::from("foo"))
        );
        match &result.content[1] {
            LanguageModelToolResultContent::Image(image) => {
                assert_eq!(image.source.as_ref(), "data");
                assert_eq!(image.size.width, 1);
                assert_eq!(image.size.height, 2);
                assert_eq!(image.mime_type.as_ref(), "image/png");
            }
            _ => panic!("Expected Image variant"),
        }

        // Round-tripping preserves multi-part content.
        let roundtripped: LanguageModelToolResult =
            serde_json::from_value(serde_json::to_value(&result).unwrap()).unwrap();
        assert_eq!(roundtripped, result);
    }

    #[test]
    fn test_estimate_image_tokens_uses_tile_formula() {
        assert_eq!(
            estimate_image_tokens(ImageSize {
                width: 1280,
                height: 720
            }),
            1_105
        );
        assert_eq!(
            estimate_image_tokens(ImageSize {
                width: 512,
                height: 512
            }),
            255
        );
        assert_eq!(estimate_image_tokens(ImageSize::default()), 1_100);
        assert_eq!(
            LanguageModelImage {
                source: "AAAA".into(),
                size: ImageSize {
                    width: 1280,
                    height: 720
                },
                mime_type: "image/jpeg".into(),
            }
            .estimate_tokens(),
            1_105
        );
        assert_eq!(
            LanguageModelImage::from_source("AAAA").to_base64_url(),
            "data:image/png;base64,AAAA"
        );
        assert_eq!(
            LanguageModelImage {
                source: "AAAA".into(),
                size: ImageSize::default(),
                mime_type: "image/jpeg".into(),
            }
            .to_base64_url(),
            "data:image/jpeg;base64,AAAA"
        );
    }

    #[test]
    fn test_string_contents_includes_all_tool_result_text_parts() {
        let tool_result = LanguageModelToolResult {
            tool_use_id: LanguageModelToolUseId::from("id".to_string()),
            tool_name: Arc::from("tool"),
            is_error: false,
            content: vec![
                LanguageModelToolResultContent::Text(Arc::from("first ")),
                LanguageModelToolResultContent::Image(LanguageModelImage::empty()),
                LanguageModelToolResultContent::Text(Arc::from("second")),
            ],
            output: None,
        };
        let message = LanguageModelRequestMessage {
            role: Role::User,
            content: vec![
                MessageContent::Text("prefix ".to_string()),
                MessageContent::ToolResult(tool_result),
                MessageContent::Text(" suffix".to_string()),
            ],
            cache: false,
            reasoning_details: None,
        };
        assert_eq!(message.string_contents(), "prefix first second suffix");
    }
}
