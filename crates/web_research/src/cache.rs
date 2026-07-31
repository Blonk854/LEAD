use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedPage {
    pub url: String,
    pub stored_at_unix_ms: u64,
    pub content_type: Option<String>,
    pub markdown: String,
    pub content_sha256: String,
}

pub fn disk_cache_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("web_cache")
}

pub struct DiskCache {
    root: PathBuf,
}

impl DiskCache {
    pub fn open(root: PathBuf) -> Result<Self> {
        fs::create_dir_all(&root)
            .with_context(|| format!("failed to create web cache dir {}", root.display()))?;
        Ok(Self { root })
    }

    pub fn get(&self, canonical_url: &str, ttl: Duration) -> Result<Option<CachedPage>> {
        let path = self.path_for(canonical_url);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
        let page: CachedPage = serde_json::from_slice(&bytes).context("parse cache entry")?;
        let now = now_ms();
        if now.saturating_sub(page.stored_at_unix_ms) > ttl.as_millis() as u64 {
            let _ = fs::remove_file(&path);
            return Ok(None);
        }
        Ok(Some(page))
    }

    pub fn put(&self, page: &CachedPage) -> Result<()> {
        let path = self.path_for(&page.url);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let bytes = serde_json::to_vec_pretty(page)?;
        fs::write(&path, bytes).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn path_for(&self, canonical_url: &str) -> PathBuf {
        let digest = {
            use sha2::{Digest, Sha256};
            let hash = Sha256::digest(canonical_url.as_bytes());
            format!("{hash:x}")
        };
        self.root.join(&digest[..2]).join(format!("{digest}.json"))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}
