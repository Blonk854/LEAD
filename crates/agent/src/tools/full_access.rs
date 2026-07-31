use std::path::{Component, Path, PathBuf};

use agent_settings::AgentSettings;
use gpui::{App, Entity};
use project::Project;
use settings::Settings;

use crate::ToolPermissionContext;

const FULL_ACCESS_DISABLED: &str = "Unleashed is disabled. Enable it in Settings → AI → Unleashed (or set agent.full_access.enabled).";

pub fn full_access_enabled(cx: &App) -> bool {
    AgentSettings::get_global(cx).full_access_enabled()
}

/// Canonicalizes the deepest existing ancestor and reattaches any missing
/// suffix, preventing a symlinked parent from bypassing path policy.
pub async fn canonicalize_for_access(path: &Path, fs: &dyn fs::Fs) -> Result<PathBuf, String> {
    let mut ancestor = Some(path);
    let mut suffix = Vec::new();
    while let Some(current) = ancestor {
        match fs.canonicalize(current).await {
            Ok(mut canonical) => {
                for component in suffix.into_iter().rev() {
                    canonical.push(component);
                }
                return util::paths::normalize_lexically(&canonical)
                    .map_err(|error| error.to_string());
            }
            Err(_) => {
                if let Some(name) = current.file_name() {
                    suffix.push(name.to_os_string());
                }
                ancestor = current.parent();
            }
        }
    }
    Err(format!("Unable to resolve path {}", path.display()))
}

/// Returns true when `path` is equal to or below `root`.
///
/// Windows path components are compared case-insensitively because the
/// platform's normal filesystems are case-insensitive while `Path::starts_with`
/// is not guaranteed to model that policy.
pub fn path_is_within(path: &Path, root: &Path) -> bool {
    let path_components = normalized_components(path);
    let root_components = normalized_components(root);
    path_components.len() >= root_components.len()
        && path_components
            .iter()
            .zip(root_components.iter())
            .all(|(path, root)| component_eq(path, root))
}

fn normalized_components(path: &Path) -> Vec<String> {
    let mut result = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if result.last().is_some_and(|component| component != "..") {
                    result.pop();
                } else {
                    result.push("..".into());
                }
            }
            Component::Prefix(prefix) => {
                result.push(prefix.as_os_str().to_string_lossy().into_owned())
            }
            Component::RootDir => result.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::Normal(component) => {
                result.push(component.to_string_lossy().into_owned());
            }
        }
    }
    result
}

fn component_eq(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        let left = left.strip_prefix(r"\\?\").unwrap_or(left);
        let right = right.strip_prefix(r"\\?\").unwrap_or(right);
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

/// Built-in paths that an agent may never target through full-access tools.
pub fn is_catastrophic_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }

    let components = normalized_components(path);
    if components.len() <= 2 {
        // POSIX root is one component; a Windows drive root is two.
        return true;
    }

    #[cfg(windows)]
    {
        let protected = [
            Path::new(r"C:\Windows"),
            Path::new(r"C:\Program Files"),
            Path::new(r"C:\Program Files (x86)"),
            Path::new(r"C:\ProgramData"),
        ];
        if protected.iter().any(|root| path_is_within(path, root)) {
            return true;
        }
    }

    #[cfg(not(windows))]
    {
        let protected = [
            Path::new("/System"),
            Path::new("/bin"),
            Path::new("/etc"),
            Path::new("/sbin"),
            Path::new("/usr"),
            Path::new("/var"),
        ];
        if protected.iter().any(|root| path_is_within(path, root)) {
            return true;
        }
    }

    false
}

pub fn is_inside_project(project: &Entity<Project>, path: &Path, cx: &App) -> bool {
    let settings = AgentSettings::get_global(cx);
    project
        .read(cx)
        .worktrees(cx)
        .any(|worktree| path_is_within(path, &worktree.read(cx).abs_path()))
        || settings
            .full_access
            .allowed_roots
            .iter()
            .any(|root| path_is_within(path, root))
}

/// Applies the shared guard rail for paths outside open project worktrees.
///
/// `Ok(None)` means the operation is already in trusted scope. `Ok(Some(_))`
/// means the caller must authorize the returned context before proceeding.
pub fn escape_gate(
    project: &Entity<Project>,
    paths: &[PathBuf],
    tool_name: &str,
    cx: &App,
) -> Result<Option<ToolPermissionContext>, String> {
    let settings = AgentSettings::get_global(cx);
    let mut escaped = Vec::new();
    for path in paths {
        if !path.is_absolute() {
            return Err(format!(
                "Full-access path must be absolute: {}",
                path.display()
            ));
        }

        let path = util::paths::normalize_lexically(path)
            .map_err(|error| format!("Invalid full-access path {}: {error}", path.display()))?;

        if settings.full_access.enabled
            && (is_catastrophic_path(&path)
                || settings
                    .full_access
                    .denied_roots
                    .iter()
                    .any(|root| path_is_within(&path, root)))
        {
            return Err(format!(
                "Blocked by LEAD's unbypassable full-access denylist: {}",
                path.display()
            ));
        }

        if !is_inside_project(project, &path, cx) {
            if !settings.full_access.enabled {
                return Err(FULL_ACCESS_DISABLED.into());
            }
            escaped.push(path.to_string_lossy().into_owned());
        }
    }

    if escaped.is_empty() {
        Ok(None)
    } else {
        Ok(Some(ToolPermissionContext::new(tool_name, escaped)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_containment_is_component_aware() {
        if cfg!(windows) {
            assert!(path_is_within(
                Path::new(r"C:\Users\person\project\src"),
                Path::new(r"C:\Users\person\project")
            ));
            assert!(!path_is_within(
                Path::new(r"C:\Users\person\project-other"),
                Path::new(r"C:\Users\person\project")
            ));
        } else {
            assert!(path_is_within(
                Path::new("/home/person/project/src"),
                Path::new("/home/person/project")
            ));
            assert!(!path_is_within(
                Path::new("/home/person/project-other"),
                Path::new("/home/person/project")
            ));
        }
    }

    #[test]
    fn catastrophic_roots_are_blocked() {
        #[cfg(windows)]
        {
            assert!(is_catastrophic_path(Path::new(r"C:\")));
            assert!(is_catastrophic_path(Path::new(r"C:\Windows\System32")));
            assert!(!is_catastrophic_path(Path::new(
                r"C:\Users\person\Documents"
            )));
        }
        #[cfg(not(windows))]
        {
            assert!(is_catastrophic_path(Path::new("/")));
            assert!(is_catastrophic_path(Path::new("/usr/bin")));
            assert!(!is_catastrophic_path(Path::new("/home/person/Documents")));
        }
    }
}
