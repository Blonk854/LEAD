use std::path::{Path, PathBuf};

use agent_settings::AgentSettings;
use gpui::{ReadGlobal, ScrollHandle, prelude::*};
use settings::{Settings as _, SettingsStore};
use ui::{Banner, Divider, Severity, Tooltip, prelude::*};
use util::ResultExt as _;

use super::tool_permissions_setup::render_unleashed_tool_permission_items;
use crate::{SettingsWindow, components::SettingsInputField};

const UNLEASHED_INTRO: &str = "Unleashed unlocks whole-PC agent tools (run code, process control, HTTP requests, and computer use) and lets trusted absolute paths skip the outside-project prompt. Pair this with the Unleashed agent profile.";

pub(crate) fn render_unleashed_setup_page(
    settings_window: &SettingsWindow,
    scroll_handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let settings = AgentSettings::get_global(cx);
    let allowed_roots = settings.full_access.allowed_roots.clone();
    let denied_roots = settings.full_access.denied_roots.clone();
    let tool_items = render_unleashed_tool_permission_items(settings_window, window, cx);
    let scroll_step = px(40.);

    v_flex()
        .id("unleashed-setup-page")
        .on_action({
            let scroll_handle = scroll_handle.clone();
            move |_: &menu::SelectNext, window, cx| {
                window.focus_next(cx);
                let current_offset = scroll_handle.offset();
                scroll_handle.set_offset(gpui::point(
                    current_offset.x,
                    current_offset.y - scroll_step,
                ));
            }
        })
        .on_action({
            let scroll_handle = scroll_handle.clone();
            move |_: &menu::SelectPrevious, window, cx| {
                window.focus_prev(cx);
                let current_offset = scroll_handle.offset();
                scroll_handle.set_offset(gpui::point(
                    current_offset.x,
                    current_offset.y + scroll_step,
                ));
            }
        })
        .min_w_0()
        .size_full()
        .pt_2p5()
        .px_8()
        .pb_16()
        .overflow_y_scroll()
        .track_scroll(scroll_handle)
        .gap_5()
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("Unleashed").size(LabelSize::Large))
                .child(
                    Label::new(UNLEASHED_INTRO)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(
            Banner::new().severity(Severity::Info).child(
                Label::new(
                    "Built-in protected paths (Windows, Program Files, ProgramData, and drive roots) cannot be bypassed. Denied roots always win over allowed roots.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            ),
        )
        .child(render_roots_section(
            "Allowed Roots",
            "Absolute directories treated as trusted. Operations under these paths skip the outside-project escape prompt.",
            "allowed",
            &allowed_roots,
            "Add absolute path…",
            cx,
        ))
        .child(Divider::horizontal())
        .child(render_roots_section(
            "Denied Roots",
            "Absolute directories Unleashed tools must never touch. These augment LEAD's built-in denylist.",
            "denied",
            &denied_roots,
            "Add absolute path…",
            cx,
        ))
        .child(Divider::horizontal())
        .child(
            v_flex()
                .gap_1()
                .child(Label::new("Unleashed Tool Permissions").size(LabelSize::Large))
                .child(
                    Label::new(
                        "Configure confirmation rules for whole-PC tools. General tools remain under Tool Permissions.",
                    )
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                ),
        )
        .children(tool_items)
        .into_any_element()
}

fn render_roots_section(
    title: &'static str,
    description: &'static str,
    kind: &'static str,
    roots: &[PathBuf],
    placeholder: &'static str,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    v_flex()
        .gap_3()
        .child(
            v_flex().gap_1().child(Label::new(title)).child(
                Label::new(description)
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
        )
        .map(|this| {
            if roots.is_empty() {
                this.child(
                    Label::new("No paths configured.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            } else {
                this.children(roots.iter().enumerate().map(|(index, root)| {
                    render_root_row(kind, index, root.display().to_string(), cx)
                }))
            }
        })
        .child(render_add_root_input(kind, placeholder, cx))
        .into_any_element()
}

fn render_root_row(
    kind: &'static str,
    index: usize,
    path: String,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let path_for_delete = path.clone();
    let path_for_update = path.clone();
    let input_id = format!("unleashed-{kind}-root-{index}");
    let delete_id = format!("unleashed-{kind}-delete-{index}");
    let settings_window = cx.entity().downgrade();

    SettingsInputField::new()
        .with_id(input_id)
        .with_initial_text(path)
        .tab_index(0)
        .with_buffer_font()
        .color(Color::Default)
        .action_slot(
            IconButton::new(delete_id, IconName::Trash)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(Tooltip::text("Remove Path"))
                .on_click(cx.listener(move |_, _, _, cx| {
                    remove_root(kind, &path_for_delete, cx);
                })),
        )
        .on_confirm(move |new_path, _window, cx| {
            if let Some(new_path) = new_path {
                let trimmed = new_path.trim().to_string();
                if trimmed.is_empty() || trimmed == path_for_update {
                    return;
                }
                match validate_absolute_path(&trimmed) {
                    Ok(normalized) => {
                        if !update_root(kind, &path_for_update, normalized, cx) {
                            settings_window
                                .update(cx, |this, cx| {
                                    this.regex_validation_error =
                                        Some("That path is already in this list.".into());
                                    cx.notify();
                                })
                                .log_err();
                        } else {
                            settings_window
                                .update(cx, |this, cx| {
                                    this.regex_validation_error = None;
                                    cx.notify();
                                })
                                .log_err();
                        }
                    }
                    Err(error) => {
                        settings_window
                            .update(cx, |this, cx| {
                                this.regex_validation_error = Some(error);
                                cx.notify();
                            })
                            .log_err();
                    }
                }
            }
        })
        .into_any_element()
}

fn render_add_root_input(
    kind: &'static str,
    placeholder: &'static str,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let input_id = format!("unleashed-{kind}-new-root");
    let settings_window = cx.entity().downgrade();

    SettingsInputField::new()
        .with_id(input_id)
        .with_placeholder(placeholder)
        .tab_index(0)
        .with_buffer_font()
        .on_confirm(move |new_path, _window, cx| {
            let Some(new_path) = new_path else {
                return;
            };
            let trimmed = new_path.trim().to_string();
            if trimmed.is_empty() {
                return;
            }
            match validate_absolute_path(&trimmed) {
                Ok(normalized) => {
                    if !add_root(kind, normalized, cx) {
                        settings_window
                            .update(cx, |this, cx| {
                                this.regex_validation_error =
                                    Some("That path is already in this list.".into());
                                cx.notify();
                            })
                            .log_err();
                    } else {
                        settings_window
                            .update(cx, |this, cx| {
                                this.regex_validation_error = None;
                                cx.notify();
                            })
                            .log_err();
                    }
                }
                Err(error) => {
                    settings_window
                        .update(cx, |this, cx| {
                            this.regex_validation_error = Some(error);
                            cx.notify();
                        })
                        .log_err();
                }
            }
        })
        .into_any_element()
}

fn validate_absolute_path(raw: &str) -> Result<PathBuf, String> {
    let path = PathBuf::from(raw);
    if !path.is_absolute() {
        return Err("Enter an absolute path (for example, C:\\Users\\you\\Scratch).".into());
    }
    util::paths::normalize_lexically(&path).map_err(|error| format!("Invalid path: {error}"))
}

fn mutate_roots(
    kind: &'static str,
    cx: &mut App,
    mutate: impl FnOnce(&mut Vec<PathBuf>) + Send + 'static,
) {
    SettingsStore::global(cx).update_settings_file(<dyn fs::Fs>::global(cx), move |settings, _| {
        let full_access = settings
            .agent
            .get_or_insert_default()
            .full_access
            .get_or_insert_default();
        let list = match kind {
            "allowed" => full_access.allowed_roots.get_or_insert_default(),
            "denied" => full_access.denied_roots.get_or_insert_default(),
            _ => return,
        };
        mutate(&mut list.0);
    });
}

fn current_roots(kind: &'static str, cx: &App) -> Vec<PathBuf> {
    let settings = AgentSettings::get_global(cx);
    match kind {
        "allowed" => settings.full_access.allowed_roots.clone(),
        "denied" => settings.full_access.denied_roots.clone(),
        _ => Vec::new(),
    }
}

fn add_root(kind: &'static str, path: PathBuf, cx: &mut App) -> bool {
    if current_roots(kind, cx)
        .iter()
        .any(|existing| path_eq(existing, &path))
    {
        return false;
    }
    mutate_roots(kind, cx, move |roots| {
        if !roots.iter().any(|existing| path_eq(existing, &path)) {
            roots.push(path);
        }
    });
    true
}

fn remove_root(kind: &'static str, path: &str, cx: &mut App) {
    let target = PathBuf::from(path);
    mutate_roots(kind, cx, move |roots| {
        roots.retain(|existing| !path_eq(existing, &target));
    });
}

fn update_root(kind: &'static str, old_path: &str, new_path: PathBuf, cx: &mut App) -> bool {
    let old = PathBuf::from(old_path);
    let roots = current_roots(kind, cx);
    if roots
        .iter()
        .any(|existing| path_eq(existing, &new_path) && !path_eq(existing, &old))
    {
        return false;
    }
    if !roots.iter().any(|existing| path_eq(existing, &old)) {
        return false;
    }
    mutate_roots(kind, cx, move |roots| {
        if roots
            .iter()
            .any(|existing| path_eq(existing, &new_path) && !path_eq(existing, &old))
        {
            return;
        }
        if let Some(entry) = roots.iter_mut().find(|existing| path_eq(existing, &old)) {
            *entry = new_path;
        }
    });
    true
}

fn path_eq(a: &Path, b: &Path) -> bool {
    #[cfg(windows)]
    {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_absolute_path_rejects_relative() {
        assert!(validate_absolute_path("relative/path").is_err());
        assert!(validate_absolute_path("./also-relative").is_err());
    }

    #[test]
    fn validate_absolute_path_accepts_absolute() {
        #[cfg(windows)]
        {
            assert!(validate_absolute_path(r"C:\Users\me\Scratch").is_ok());
        }
        #[cfg(not(windows))]
        {
            assert!(validate_absolute_path("/tmp/scratch").is_ok());
        }
    }
}
