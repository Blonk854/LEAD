//! Isolated system-browser fallback for JS-heavy pages.
//!
//! Uses the installed Edge/Chrome binary with a LEAD-owned `--user-data-dir`
//! (no personal cookies/sessions). Content is taken via Chromium headless
//! `--dump-dom` after JS runs. The child process is killed on cancel, timeout,
//! or Drop. Downloads are pointed at a quarantine directory and are never
//! executed by this module.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BrowserError {
    #[error("no system Chrome/Edge binary found")]
    BrowserNotFound,
    #[error("browser_unavailable: {0}")]
    Unavailable(String),
    #[error("browser_cancelled")]
    Cancelled,
    #[error("browser_timeout")]
    Timeout,
    #[error("browser_failed: {0}")]
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct BrowserFetchRequest {
    pub url: String,
    pub profile_dir: PathBuf,
    pub download_dir: PathBuf,
    pub timeout: Duration,
    pub cancel: Option<Arc<AtomicBool>>,
}

#[derive(Clone, Debug)]
pub struct BrowserFetchResult {
    pub final_url: String,
    pub html: Vec<u8>,
    pub browser_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredBrowser {
    pub id: String,
    pub path: PathBuf,
}

/// Locate a system Chrome or Edge binary (no bundling).
pub fn discover_system_browser() -> Option<DiscoveredBrowser> {
    for candidate in browser_candidates() {
        if candidate.path.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn browser_candidates() -> Vec<DiscoveredBrowser> {
    let mut out = Vec::new();

    #[cfg(target_os = "windows")]
    {
        let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let program_files_x86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
        let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);

        for root in [program_files_x86, program_files, local_app_data]
            .into_iter()
            .flatten()
        {
            out.push(DiscoveredBrowser {
                id: "msedge".into(),
                path: root
                    .join("Microsoft")
                    .join("Edge")
                    .join("Application")
                    .join("msedge.exe"),
            });
            out.push(DiscoveredBrowser {
                id: "chrome".into(),
                path: root
                    .join("Google")
                    .join("Chrome")
                    .join("Application")
                    .join("chrome.exe"),
            });
        }
    }

    #[cfg(target_os = "macos")]
    {
        out.push(DiscoveredBrowser {
            id: "chrome".into(),
            path: PathBuf::from(
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            ),
        });
        out.push(DiscoveredBrowser {
            id: "msedge".into(),
            path: PathBuf::from(
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            ),
        });
        out.push(DiscoveredBrowser {
            id: "chromium".into(),
            path: PathBuf::from("/Applications/Chromium.app/Contents/MacOS/Chromium"),
        });
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        for (id, name) in [
            ("chrome", "google-chrome"),
            ("chrome", "google-chrome-stable"),
            ("msedge", "microsoft-edge"),
            ("msedge", "microsoft-edge-stable"),
            ("chromium", "chromium"),
            ("chromium", "chromium-browser"),
        ] {
            if let Ok(path) = which::which(name) {
                out.push(DiscoveredBrowser {
                    id: id.into(),
                    path,
                });
            }
        }
    }

    // PATH fallback for all platforms.
    for (id, name) in [
        ("msedge", "msedge"),
        ("chrome", "chrome"),
        ("chrome", "google-chrome"),
        ("chromium", "chromium"),
        ("chromium", "chromium-browser"),
    ] {
        if let Ok(path) = which::which(name) {
            if !out.iter().any(|existing| existing.path == path) {
                out.push(DiscoveredBrowser {
                    id: id.into(),
                    path,
                });
            }
        }
    }

    out
}

/// Prepare isolated profile + download quarantine dirs and Chromium prefs.
pub fn prepare_browser_dirs(profile_dir: &Path, download_dir: &Path) -> Result<(), BrowserError> {
    fs::create_dir_all(profile_dir).map_err(|error| {
        BrowserError::Unavailable(format!("create profile dir: {error}"))
    })?;
    fs::create_dir_all(download_dir).map_err(|error| {
        BrowserError::Unavailable(format!("create download quarantine: {error}"))
    })?;

    // Default profile prefs: downloads only go to quarantine; never auto-open.
    let default_dir = profile_dir.join("Default");
    fs::create_dir_all(&default_dir).map_err(|error| {
        BrowserError::Unavailable(format!("create Default profile: {error}"))
    })?;
    let prefs_path = default_dir.join("Preferences");
    if !prefs_path.exists() {
        let download = download_dir.to_string_lossy().replace('\\', "/");
        let prefs = serde_json::json!({
            "download": {
                "default_directory": download,
                "directory_upgrade": true,
                "prompt_for_download": false
            },
            "profile": {
                "default_content_settings": {
                    "automatic_downloads": 2
                },
                "exit_type": "Normal"
            },
            "browser": {
                "check_default_browser": false
            }
        });
        fs::write(&prefs_path, prefs.to_string()).map_err(|error| {
            BrowserError::Unavailable(format!("write Preferences: {error}"))
        })?;
    }
    Ok(())
}

/// Fetch rendered HTML via system Chrome/Edge headless dump-dom.
pub fn fetch_rendered_html(request: &BrowserFetchRequest) -> Result<BrowserFetchResult, BrowserError> {
    if request
        .cancel
        .as_ref()
        .is_some_and(|flag| flag.load(Ordering::SeqCst))
    {
        return Err(BrowserError::Cancelled);
    }

    let browser = discover_system_browser().ok_or(BrowserError::BrowserNotFound)?;
    prepare_browser_dirs(&request.profile_dir, &request.download_dir)?;

    let args = dump_dom_args(
        &request.url,
        &request.profile_dir,
        &request.download_dir,
    );

    let child = Command::new(&browser.path)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| BrowserError::Unavailable(format!("spawn {}: {error}", browser.id)))?;

    let mut guard = KillOnDrop::new(child);
    let started = Instant::now();
    let stdout = guard
        .child
        .stdout
        .take()
        .ok_or_else(|| BrowserError::Failed("missing stdout".into()))?;

    // Read stdout on a helper thread so we can poll cancel/timeout.
    let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, String>>();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut reader = stdout;
        let result = reader
            .read_to_end(&mut buf)
            .map(|_| buf)
            .map_err(|error| error.to_string());
        let _ = tx.send(result);
    });

    loop {
        if request
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
        {
            return Err(BrowserError::Cancelled);
        }
        if started.elapsed() >= request.timeout {
            return Err(BrowserError::Timeout);
        }
        match rx.try_recv() {
            Ok(Ok(html)) => {
                let status = guard
                    .child
                    .wait()
                    .map_err(|error| BrowserError::Failed(error.to_string()))?;
                // dump-dom may still exit non-zero on some soft failures; accept HTML if present.
                if html.len() < 16 && !status.success() {
                    let stderr = read_stderr(&mut guard.child);
                    return Err(BrowserError::Failed(format!(
                        "exit={status:?} stderr={}",
                        truncate(&stderr, 400)
                    )));
                }
                guard.disarm();
                return Ok(BrowserFetchResult {
                    final_url: request.url.clone(),
                    html,
                    browser_id: browser.id,
                });
            }
            Ok(Err(error)) => return Err(BrowserError::Failed(error)),
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                return Err(BrowserError::Failed("stdout reader exited early".into()));
            }
        }
    }
}

pub fn dump_dom_args(url: &str, profile_dir: &Path, download_dir: &Path) -> Vec<String> {
    vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        "--disable-popup-blocking".into(),
        "--disable-background-networking".into(),
        "--disable-sync".into(),
        "--disable-translate".into(),
        "--metrics-recording-only".into(),
        "--safebrowsing-disable-auto-update".into(),
        "--disable-features=TranslateUI,DownloadBubble".into(),
        format!("--user-data-dir={}", profile_dir.display()),
        format!("--download-default-directory={}", download_dir.display()),
        // Give JS a bounded budget before dumping the DOM.
        "--virtual-time-budget=8000".into(),
        "--dump-dom".into(),
        url.to_string(),
    ]
}

struct KillOnDrop {
    child: Child,
    armed: bool,
}

impl KillOnDrop {
    fn new(child: Child) -> Self {
        Self { child, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn read_stderr(child: &mut Child) -> String {
    let mut out = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut out);
    }
    out
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dump_dom_args_include_isolation_and_url() {
        let args = dump_dom_args(
            "https://example.com/page",
            Path::new("/tmp/lead-profile"),
            Path::new("/tmp/lead-downloads"),
        );
        assert!(args.iter().any(|arg| arg == "--dump-dom"));
        assert!(args.iter().any(|arg| arg.contains("lead-profile")));
        assert!(args.iter().any(|arg| arg.contains("lead-downloads")));
        assert_eq!(args.last().map(String::as_str), Some("https://example.com/page"));
    }

    #[test]
    fn prepare_browser_dirs_writes_prefs() {
        let root = tempfile::tempdir().unwrap();
        let profile = root.path().join("profile");
        let downloads = root.path().join("downloads");
        prepare_browser_dirs(&profile, &downloads).unwrap();
        let prefs = fs::read_to_string(profile.join("Default").join("Preferences")).unwrap();
        assert!(prefs.contains("default_directory"));
        assert!(downloads.is_dir());
    }

    #[test]
    fn cancelled_before_start_short_circuits() {
        let root = tempfile::tempdir().unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        let err = fetch_rendered_html(&BrowserFetchRequest {
            url: "https://example.com".into(),
            profile_dir: root.path().join("profile"),
            download_dir: root.path().join("downloads"),
            timeout: Duration::from_secs(5),
            cancel: Some(cancel),
        })
        .unwrap_err();
        assert_eq!(err, BrowserError::Cancelled);
    }
}
