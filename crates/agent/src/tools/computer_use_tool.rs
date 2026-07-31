use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

use agent_client_protocol::schema as acp;
use base64::Engine as _;
#[cfg(not(windows))]
use enigo::Coordinate;
use enigo::{Button, Direction, Enigo, Key, Keyboard as _, Mouse as _, Settings as EnigoSettings};
use gpui::{App, AppContext as _, Task};
use image::imageops::FilterType;
use language_model::{ImageSize, LanguageModelImage, LanguageModelToolResultContent};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ui::SharedString;
use xcap::Monitor;

use crate::{AgentTool, IntoToolLlmOutput, ToolCallEventStream, ToolInput, ToolPermissionContext};

/// Longest edge for screenshots sent to the model.
/// Matches Anthropic's practical vision limit while staying affordable for local contexts.
const SCREENSHOT_MAX_EDGE: u32 = 1568;
const SCREENSHOT_JPEG_QUALITY: u8 = 85;

/// Captures the screen or controls the local mouse and keyboard.
///
/// Coordinates for `mouse_move` (and `click` when `x`/`y` are provided) are in
/// **image pixels** from the most recent screenshot in this session. LEAD maps
/// them onto the selected monitor's virtual-desktop coordinates. Always call
/// `screenshot` again after `click`/`type`/`key` before the next targeted move.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct ComputerUseToolInput {
    /// `screenshot`, `mouse_move`, `click`, `type`, or `key`.
    pub action: String,
    /// Image-space X for `mouse_move`, or optional pre-click move for `click`.
    pub x: Option<i32>,
    /// Image-space Y for `mouse_move`, or optional pre-click move for `click`.
    pub y: Option<i32>,
    /// Text for `type`, or key name for `key`.
    pub text: Option<String>,
    /// Mouse button for `click`: `left`, `right`, or `middle`.
    pub button: Option<String>,
    /// Monitor selector for `screenshot`: `primary` (default), a 0-based index
    /// like `0`, or a case-insensitive substring of the monitor name.
    pub monitor: Option<String>,
}

/// Maps screenshot image pixels into absolute virtual-desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ScreenshotScale {
    image_w: i32,
    image_h: i32,
    capture_w: i32,
    capture_h: i32,
    /// Monitor top-left in virtual desktop coordinates.
    origin_x: i32,
    origin_y: i32,
}

impl ScreenshotScale {
    fn map_image_to_virtual(&self, x: i32, y: i32) -> (i32, i32) {
        let local_x = map_axis(x, self.image_w, self.capture_w);
        let local_y = map_axis(y, self.image_h, self.capture_h);
        (self.origin_x + local_x, self.origin_y + local_y)
    }
}

/// Map `pos` from a source axis of `src_size` pixels onto `dst_size` pixels.
/// Uses inclusive endpoints `[0, size-1]` with rounding.
fn map_axis(pos: i32, src_size: i32, dst_size: i32) -> i32 {
    if dst_size <= 1 {
        return 0;
    }
    if src_size <= 1 {
        return pos.clamp(0, dst_size - 1);
    }
    let src_max = (src_size - 1) as i64;
    let dst_max = (dst_size - 1) as i64;
    let mapped = (pos as i64 * dst_max + src_max / 2) / src_max;
    mapped.clamp(0, dst_max) as i32
}

#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum ComputerUseToolOutput {
    Parts(Vec<LanguageModelToolResultContent>),
    Text(String),
}

impl<'de> Deserialize<'de> for ComputerUseToolOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        // New multipart form.
        if let Ok(parts) =
            serde_json::from_value::<Vec<LanguageModelToolResultContent>>(value.clone())
        {
            return Ok(Self::Parts(parts));
        }
        // Plain text success/error strings (current + legacy).
        if let Ok(text) = serde_json::from_value::<String>(value.clone()) {
            return Ok(Self::Text(text));
        }
        // Legacy single LanguageModelToolResultContent (e.g. a lone image).
        if let Ok(part) = serde_json::from_value::<LanguageModelToolResultContent>(value) {
            return Ok(Self::Parts(vec![part]));
        }
        Err(serde::de::Error::custom(
            "unsupported computer_use tool output shape",
        ))
    }
}

impl IntoToolLlmOutput for ComputerUseToolOutput {
    fn into_tool_llm_output(self) -> Vec<LanguageModelToolResultContent> {
        match self {
            Self::Parts(parts) => parts,
            Self::Text(text) => vec![text.into()],
        }
    }
}

pub struct ComputerUseTool {
    last_scale: Arc<Mutex<Option<ScreenshotScale>>>,
}

impl Default for ComputerUseTool {
    fn default() -> Self {
        Self {
            last_scale: Arc::new(Mutex::new(None)),
        }
    }
}

impl AgentTool for ComputerUseTool {
    type Input = ComputerUseToolInput;
    type Output = ComputerUseToolOutput;

    const NAME: &'static str = "computer_use";

    fn kind() -> acp::ToolKind {
        acp::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        input
            .map(|input| format!("Computer use: {}", input.action).into())
            .unwrap_or_else(|_| "Use computer".into())
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input.recv().await.map_err(|error| tool_error_output(error))?;
            if !cx.update(|cx| crate::full_access_enabled(cx)) {
                return Err(tool_error_output(
                    "Unleashed is disabled. Enable it in Settings → AI → Unleashed (or set agent.full_access.enabled).",
                ));
            }

            let action = input.action.to_ascii_lowercase();
            let summary = serde_json::to_string(&input).map_err(|error| tool_error_output(error))?;
            let authorize = cx.update(|cx| {
                event_stream.authorize_always_prompt_unless_denied(
                    format!("Computer use: {action}"),
                    ToolPermissionContext::new(Self::NAME, vec![summary]),
                    cx,
                )
            });
            authorize.await.map_err(|error| tool_error_output(error))?;

            let scale = self.last_scale.clone();
            let output = cx
                .background_spawn(async move { perform_computer_action(input, scale) })
                .await
                .map_err(|error| tool_error_output(error))?;

            if let ComputerUseToolOutput::Parts(parts) = &output {
                let mut content = Vec::new();
                for part in parts {
                    match part {
                        LanguageModelToolResultContent::Text(text) => {
                            content.push(acp::ToolCallContent::Content(acp::Content::new(
                                acp::ContentBlock::Text(acp::TextContent::new(text.to_string())),
                            )));
                        }
                        LanguageModelToolResultContent::Image(image) => {
                            content.push(acp::ToolCallContent::Content(acp::Content::new(
                                acp::ContentBlock::Image(acp::ImageContent::new(
                                    image.source.clone(),
                                    image.mime_type.to_string(),
                                )),
                            )));
                        }
                    }
                }
                if !content.is_empty() {
                    event_stream
                        .update_fields(acp::ToolCallUpdateFields::new().content(content));
                }
            }
            Ok(output)
        })
    }
}

fn perform_computer_action(
    input: ComputerUseToolInput,
    last_scale: Arc<Mutex<Option<ScreenshotScale>>>,
) -> Result<ComputerUseToolOutput, String> {
    match input.action.to_ascii_lowercase().as_str() {
        "screenshot" => {
            let (image, metadata, scale) = capture_screenshot(input.monitor.as_deref())?;
            *last_scale
                .lock()
                .map_err(|_| "Screenshot scale lock poisoned.".to_string())? = Some(scale);
            Ok(ComputerUseToolOutput::Parts(vec![
                LanguageModelToolResultContent::Text(Arc::from(metadata)),
                LanguageModelToolResultContent::Image(image),
            ]))
        }
        "mouse_move" => {
            let x = input.x.ok_or_else(|| "mouse_move requires x".to_string())?;
            let y = input.y.ok_or_else(|| "mouse_move requires y".to_string())?;
            let (screen_x, screen_y) = map_coords(x, y, &last_scale)?;
            move_pointer(screen_x, screen_y)?;
            Ok(ComputerUseToolOutput::Text(format!(
                "Mouse moved to image=({x}, {y}) screen=({screen_x}, {screen_y})."
            )))
        }
        "click" => {
            if let (Some(x), Some(y)) = (input.x, input.y) {
                let (screen_x, screen_y) = map_coords(x, y, &last_scale)?;
                move_pointer(screen_x, screen_y)?;
            }
            let button = match input
                .button
                .as_deref()
                .unwrap_or("left")
                .to_ascii_lowercase()
                .as_str()
            {
                "left" => Button::Left,
                "right" => Button::Right,
                "middle" => Button::Middle,
                _ => return Err("Unsupported button. Use left, right, or middle.".into()),
            };
            let mut enigo = new_enigo()?;
            enigo
                .button(button, Direction::Click)
                .map_err(|error| error.to_string())?;
            clear_scale(&last_scale)?;
            Ok(ComputerUseToolOutput::Text(
                "Mouse click sent. Take a new screenshot before the next mouse_move/click with coordinates."
                    .into(),
            ))
        }
        "type" => {
            let text = input
                .text
                .as_deref()
                .ok_or_else(|| "type requires text".to_string())?;
            let mut enigo = new_enigo()?;
            enigo.text(text).map_err(|error| error.to_string())?;
            clear_scale(&last_scale)?;
            Ok(ComputerUseToolOutput::Text(format!(
                "Typed {} characters. Take a new screenshot before the next mouse_move/click with coordinates.",
                text.chars().count()
            )))
        }
        "key" => {
            let name = input
                .text
                .as_deref()
                .ok_or_else(|| "key requires a key name in text".to_string())?;
            let key = parse_key(name)?;
            let mut enigo = new_enigo()?;
            enigo
                .key(key, Direction::Click)
                .map_err(|error| error.to_string())?;
            clear_scale(&last_scale)?;
            Ok(ComputerUseToolOutput::Text(format!(
                "Key {name} sent. Take a new screenshot before the next mouse_move/click with coordinates."
            )))
        }
        _ => Err("Unsupported action. Use screenshot, mouse_move, click, type, or key.".into()),
    }
}

fn new_enigo() -> Result<Enigo, String> {
    Enigo::new(&EnigoSettings::default()).map_err(|error| error.to_string())
}

fn move_pointer(x: i32, y: i32) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        unsafe { SetCursorPos(x, y) }.map_err(|error| error.to_string())
    }
    #[cfg(not(windows))]
    {
        let mut enigo = new_enigo()?;
        enigo
            .move_mouse(x, y, Coordinate::Abs)
            .map_err(|error| error.to_string())
    }
}

fn clear_scale(last_scale: &Arc<Mutex<Option<ScreenshotScale>>>) -> Result<(), String> {
    *last_scale
        .lock()
        .map_err(|_| "Screenshot scale lock poisoned.".to_string())? = None;
    Ok(())
}

fn map_coords(
    x: i32,
    y: i32,
    last_scale: &Arc<Mutex<Option<ScreenshotScale>>>,
) -> Result<(i32, i32), String> {
    let scale = last_scale
        .lock()
        .map_err(|_| "Screenshot scale lock poisoned.".to_string())?;
    let Some(scale) = *scale else {
        return Err(
            "No fresh screenshot scale is available. Call action=screenshot first in this session, then pass image-pixel coordinates."
                .into(),
        );
    };
    Ok(scale.map_image_to_virtual(x, y))
}

fn capture_screenshot(
    monitor_selector: Option<&str>,
) -> Result<(LanguageModelImage, String, ScreenshotScale), String> {
    let monitors = Monitor::all().map_err(|error| error.to_string())?;
    if monitors.is_empty() {
        return Err("No monitors were found.".into());
    }
    let monitor_catalog = format_monitor_catalog(&monitors);
    let monitor = select_monitor(&monitors, monitor_selector)?;
    let origin_x = monitor.x().unwrap_or(0);
    let origin_y = monitor.y().unwrap_or(0);
    let name = monitor.name().unwrap_or_else(|_| "unknown".to_string());
    let rgba = monitor.capture_image().map_err(|error| error.to_string())?;
    let capture_w = rgba.width() as i32;
    let capture_h = rgba.height() as i32;

    let (image, image_w, image_h) =
        encode_screenshot_for_model(image::DynamicImage::ImageRgba8(rgba))?;
    let scale = ScreenshotScale {
        image_w,
        image_h,
        capture_w,
        capture_h,
        origin_x,
        origin_y,
    };
    let scale_x = capture_w as f32 / image_w.max(1) as f32;
    let scale_y = capture_h as f32 / image_h.max(1) as f32;
    let metadata = format!(
        "screenshot: monitor={name:?} origin=({origin_x},{origin_y}) image={image_w}x{image_h} capture={capture_w}x{capture_h} scale_x={scale_x:.4} scale_y={scale_y:.4}\n\
         Coordinates in mouse_move/click must be in IMAGE pixels; LEAD maps them onto this monitor's virtual-desktop position.\n\
         After click/type/key, take a new screenshot before the next targeted mouse action.\n\
         Available monitors:\n{monitor_catalog}"
    );
    Ok((image, metadata, scale))
}

fn format_monitor_catalog(monitors: &[Monitor]) -> String {
    monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            let name = monitor.name().unwrap_or_else(|_| "unknown".into());
            let primary = monitor.is_primary().unwrap_or(false);
            let x = monitor.x().unwrap_or(0);
            let y = monitor.y().unwrap_or(0);
            let w = monitor.width().unwrap_or(0);
            let h = monitor.height().unwrap_or(0);
            format!(
                "- [{index}] {name}{} origin=({x},{y}) size={w}x{h}",
                if primary { " (primary)" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn select_monitor<'a>(
    monitors: &'a [Monitor],
    selector: Option<&str>,
) -> Result<&'a Monitor, String> {
    let Some(selector) = selector.map(str::trim).filter(|value| !value.is_empty()) else {
        return monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .or_else(|| monitors.first())
            .ok_or_else(|| "No monitors were found.".to_string());
    };

    if selector.eq_ignore_ascii_case("primary") {
        return monitors
            .iter()
            .find(|monitor| monitor.is_primary().unwrap_or(false))
            .ok_or_else(|| "No primary monitor was found.".to_string());
    }

    if let Ok(index) = selector.parse::<usize>() {
        return monitors.get(index).ok_or_else(|| {
            format!(
                "Monitor index {index} is out of range (0..{}).",
                monitors.len().saturating_sub(1)
            )
        });
    }

    let needle = selector.to_ascii_lowercase();
    let matches: Vec<_> = monitors
        .iter()
        .filter(|monitor| {
            monitor
                .name()
                .map(|name| name.to_ascii_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .collect();
    match matches.as_slice() {
        [monitor] => Ok(*monitor),
        [] => Err(format!(
            "No monitor name contains {selector:?}. Available:\n{}",
            format_monitor_catalog(monitors)
        )),
        _ => Err(format!(
            "Multiple monitors match {selector:?}. Use an index instead. Available:\n{}",
            format_monitor_catalog(monitors)
        )),
    }
}

fn encode_screenshot_for_model(
    image: image::DynamicImage,
) -> Result<(LanguageModelImage, i32, i32), String> {
    let (orig_w, orig_h) = (image.width(), image.height());
    let longest = orig_w.max(orig_h).max(1);
    let scaled = if longest > SCREENSHOT_MAX_EDGE {
        let scale = SCREENSHOT_MAX_EDGE as f32 / longest as f32;
        let new_w = ((orig_w as f32) * scale).round().max(1.0) as u32;
        let new_h = ((orig_h as f32) * scale).round().max(1.0) as u32;
        image.resize(new_w, new_h, FilterType::Lanczos3)
    } else {
        image
    };
    let (image_w, image_h) = (scaled.width() as i32, scaled.height() as i32);

    match encode_jpeg_base64(&scaled) {
        Ok(source) => Ok((
            LanguageModelImage {
                source: source.into(),
                size: ImageSize {
                    width: image_w,
                    height: image_h,
                },
                mime_type: "image/jpeg".into(),
            },
            image_w,
            image_h,
        )),
        Err(_) => {
            let source = encode_png_base64(&scaled)?;
            Ok((
                LanguageModelImage {
                    source: source.into(),
                    size: ImageSize {
                        width: image_w,
                        height: image_h,
                    },
                    mime_type: "image/png".into(),
                },
                image_w,
                image_h,
            ))
        }
    }
}

fn encode_jpeg_base64(image: &image::DynamicImage) -> Result<String, String> {
    use image::ImageEncoder as _;

    let rgb = image.to_rgb8();
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, SCREENSHOT_JPEG_QUALITY)
        .write_image(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|error| error.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn encode_png_base64(image: &image::DynamicImage) -> Result<String, String> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .map_err(|error| error.to_string())?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

fn parse_key(name: &str) -> Result<Key, String> {
    match name.to_ascii_lowercase().as_str() {
        "enter" | "return" => Ok(Key::Return),
        "escape" | "esc" => Ok(Key::Escape),
        "tab" => Ok(Key::Tab),
        "space" => Ok(Key::Space),
        "backspace" => Ok(Key::Backspace),
        "delete" => Ok(Key::Delete),
        "up" => Ok(Key::UpArrow),
        "down" => Ok(Key::DownArrow),
        "left" => Ok(Key::LeftArrow),
        "right" => Ok(Key::RightArrow),
        value if value.chars().count() == 1 => Ok(Key::Unicode(value.chars().next().unwrap())),
        _ => Err("Unsupported key name.".into()),
    }
}

fn tool_error_output(error: impl ToString) -> ComputerUseToolOutput {
    ComputerUseToolOutput::Text(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_map_image_to_virtual_includes_monitor_origin() {
        let scale = ScreenshotScale {
            image_w: 1280,
            image_h: 720,
            capture_w: 2560,
            capture_h: 1440,
            origin_x: 1920,
            origin_y: 0,
        };
        assert_eq!(scale.map_image_to_virtual(0, 0), (1920, 0));
        assert_eq!(scale.map_image_to_virtual(100, 50), (2120, 100));
        assert_eq!(scale.map_image_to_virtual(1279, 719), (4479, 1439));
    }

    #[test]
    fn test_encode_screenshot_downscales_and_uses_jpeg() {
        let image = image::DynamicImage::new_rgb8(3840, 2160);
        let (encoded, image_w, image_h) = encode_screenshot_for_model(image).unwrap();
        assert!(image_w <= SCREENSHOT_MAX_EDGE as i32);
        assert!(image_h <= SCREENSHOT_MAX_EDGE as i32);
        assert_eq!(encoded.size.width, image_w);
        assert_eq!(encoded.size.height, image_h);
        assert_eq!(encoded.mime_type.as_ref(), "image/jpeg");
        assert_eq!(image_w, 1568);
        assert_eq!(image_h, 882);
        assert_eq!(encoded.estimate_tokens(), 1_445);
    }

    #[test]
    fn test_computer_use_output_deserializes_legacy_shapes() {
        let text: ComputerUseToolOutput =
            serde_json::from_value(serde_json::json!("Mouse moved")).unwrap();
        assert!(matches!(text, ComputerUseToolOutput::Text(_)));

        let image: ComputerUseToolOutput = serde_json::from_value(serde_json::json!({
            "Image": { "source": "AAAA", "size": { "width": 1, "height": 1 } }
        }))
        .unwrap();
        assert!(matches!(image, ComputerUseToolOutput::Parts(parts) if parts.len() == 1));

        let parts: ComputerUseToolOutput = serde_json::from_value(serde_json::json!([
            "meta",
            { "Image": { "source": "BBBB" } }
        ]))
        .unwrap();
        assert!(matches!(parts, ComputerUseToolOutput::Parts(parts) if parts.len() == 2));
    }
}
