//! Persist research digests + page bodies under LEAD app data for resume / explicit RAG ingest.
//!
//! Sessions are never written into the project tree. RAG ingest remains opt-in.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};

use crate::session::{ResearchDigest, ResearchPageBody};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoredResearchSession {
    pub id: String,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub digest: ResearchDigest,
    /// Full page markdown retained for resume / explicit RAG ingest (not shown to the model by default).
    #[serde(default)]
    pub pages: Vec<ResearchPageBody>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub query: String,
    pub pages_fetched: usize,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub stopped_reason: String,
}

pub fn sessions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("web_research").join("sessions")
}

pub fn new_session_id(query: &str) -> String {
    let ms = now_ms();
    let digest = {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(format!("{ms}:{query}").as_bytes());
        format!("{hash:x}")
    };
    format!("{ms}-{}", &digest[..10.min(digest.len())])
}

pub fn save_session(data_dir: &Path, session: &StoredResearchSession) -> Result<PathBuf> {
    let dir = sessions_dir(data_dir);
    fs::create_dir_all(&dir)
        .with_context(|| format!("create sessions dir {}", dir.display()))?;
    let path = dir.join(format!("{}.json", session.id));
    let bytes = serde_json::to_vec_pretty(session).context("serialize research session")?;
    fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
    let _ = prune_sessions(data_dir, 50, Duration::from_secs(14 * 24 * 60 * 60));
    Ok(path)
}

/// Delete oldest / expired session files. Keeps at most `max_sessions` and drops
/// entries older than `max_age` based on `updated_at_unix_ms`.
pub fn prune_sessions(data_dir: &Path, max_sessions: usize, max_age: Duration) -> Result<usize> {
    let dir = sessions_dir(data_dir);
    if !dir.exists() {
        return Ok(0);
    }
    let now = now_ms();
    let max_age_ms = max_age.as_millis() as u64;
    let mut entries: Vec<(PathBuf, u64)> = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_slice::<StoredResearchSession>(&bytes) else {
            continue;
        };
        if now.saturating_sub(session.updated_at_unix_ms) > max_age_ms {
            let _ = fs::remove_file(&path);
            continue;
        }
        entries.push((path, session.updated_at_unix_ms));
    }
    entries.sort_by(|left, right| right.1.cmp(&left.1));
    let mut removed = 0usize;
    for (path, _) in entries.into_iter().skip(max_sessions.max(1)) {
        if fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn load_session(data_dir: &Path, session_id: &str) -> Result<StoredResearchSession> {
    let id = sanitize_session_id(session_id)?;
    let path = sessions_dir(data_dir).join(format!("{id}.json"));
    if !path.exists() {
        bail!("research session not found: {id}");
    }
    let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_slice(&bytes).context("parse research session")
}

pub fn list_sessions(data_dir: &Path, limit: usize) -> Result<Vec<SessionSummary>> {
    let dir = sessions_dir(data_dir);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(session) = serde_json::from_slice::<StoredResearchSession>(&bytes) else {
            continue;
        };
        sessions.push(SessionSummary {
            id: session.id,
            query: session.digest.query,
            pages_fetched: session.digest.pages_fetched,
            created_at_unix_ms: session.created_at_unix_ms,
            updated_at_unix_ms: session.updated_at_unix_ms,
            stopped_reason: session.digest.stopped_reason,
        });
    }
    sessions.sort_by(|left, right| right.updated_at_unix_ms.cmp(&left.updated_at_unix_ms));
    sessions.truncate(limit.max(1));
    Ok(sessions)
}

pub fn render_session_list(summaries: &[SessionSummary]) -> String {
    if summaries.is_empty() {
        return "No persisted research sessions.".into();
    }
    let mut out = String::from("# Research sessions\n");
    for summary in summaries {
        out.push_str(&format!(
            "- `{}` — {} ({} page(s), {})\n",
            summary.id, summary.query, summary.pages_fetched, summary.stopped_reason
        ));
    }
    out.push_str(
        "\nResume with research_web session_id=<id>. Explicitly set ingest_to_rag=true to index pages.\n",
    );
    out
}

/// Build a RAG source key for a web page (`web:` + URL or content hash).
pub fn web_rag_source(url: &str, source_id: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        format!("web:{url}")
    } else if source_id.starts_with("web:") {
        source_id.to_string()
    } else {
        format!("web:{source_id}")
    }
}

fn sanitize_session_id(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || !trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        bail!("invalid research session id");
    }
    Ok(trimmed.to_string())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{ResearchClaim, ResearchCitation, ResearchDigest};

    fn sample_session(id: &str) -> StoredResearchSession {
        let now = now_ms();
        StoredResearchSession {
            id: id.into(),
            created_at_unix_ms: now,
            updated_at_unix_ms: now,
            digest: ResearchDigest {
                query: "gpui".into(),
                claims: vec![ResearchClaim {
                    text: "GPUI is a UI framework".into(),
                    source_indexes: vec![1],
                    confidence: "fetched".into(),
                }],
                sources: vec![ResearchCitation {
                    index: 1,
                    title: "docs".into(),
                    url: "https://docs.rs/gpui".into(),
                    source_id: "web:abc".into(),
                    quotes: vec!["GPU-accelerated".into()],
                }],
                pages_fetched: 1,
                skipped: vec![],
                stopped_reason: "completed".into(),
                domain_allowlist: vec!["docs.rs".into()],
                session_id: Some(id.into()),
            },
            pages: vec![ResearchPageBody {
                url: "https://docs.rs/gpui".into(),
                title: "docs".into(),
                source_id: "web:abc".into(),
                markdown: "# GPUI\n\nGPU UI crate.".into(),
                fetch_mode: "http".into(),
            }],
        }
    }

    #[test]
    fn save_load_list_roundtrip() {
        let root = tempfile::tempdir().unwrap();
        let session = sample_session("123-abcdef0123");
        save_session(root.path(), &session).unwrap();
        let loaded = load_session(root.path(), "123-abcdef0123").unwrap();
        assert_eq!(loaded.digest.query, "gpui");
        assert_eq!(loaded.pages.len(), 1);
        let listed = list_sessions(root.path(), 10).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "123-abcdef0123");
    }

    #[test]
    fn rejects_path_traversal_session_id() {
        assert!(load_session(Path::new("/tmp"), "../evil").is_err());
    }

    #[test]
    fn web_rag_source_prefers_url() {
        assert_eq!(
            web_rag_source("https://example.com/a", "web:hash"),
            "web:https://example.com/a"
        );
    }

    #[test]
    fn prune_sessions_caps_count() {
        let root = tempfile::tempdir().unwrap();
        let now = now_ms();
        for i in 0..5 {
            let mut session = sample_session(&format!("id-{i}"));
            session.updated_at_unix_ms = now + i as u64;
            session.created_at_unix_ms = now + i as u64;
            // Write without going through save_session's auto-prune side effect on timestamps.
            let dir = sessions_dir(root.path());
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{}.json", session.id));
            std::fs::write(&path, serde_json::to_vec_pretty(&session).unwrap()).unwrap();
        }
        let removed = prune_sessions(root.path(), 2, Duration::from_secs(14 * 24 * 60 * 60)).unwrap();
        assert_eq!(removed, 3);
        let listed = list_sessions(root.path(), 20).unwrap();
        assert_eq!(listed.len(), 2);
    }
}
