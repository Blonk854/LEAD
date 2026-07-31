//! Local vector RAG index stored per worktree under `.local_agent/rag.db`.

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient};
use parking_lot::Mutex;
use serde::Deserialize;
use sqlez::connection::Connection;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::project_memory::state_dir_for_worktree;

pub const DEFAULT_EMBEDDING_MODEL: &str = "text-embedding-nomic-embed-text-v1.5";
const EMBED_BATCH_SIZE: usize = 64;
const DEFAULT_CHUNK_SIZE: usize = 1000;
const DEFAULT_CHUNK_OVERLAP: usize = 200;
const DEFAULT_TOP_K: usize = 5;

const INCLUDE_EXTENSIONS: &[&str] = &[
    ".txt",
    ".md",
    ".markdown",
    ".py",
    ".rs",
    ".rst",
    ".json",
    ".js",
    ".ts",
    ".tsx",
    ".jsx",
    ".go",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".yaml",
    ".yml",
    ".toml",
    ".sql",
];

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub score: f32,
    pub source: String,
    pub chunk_index: i32,
    pub text: String,
}

pub fn rag_db_path(worktree_root: &Path) -> PathBuf {
    state_dir_for_worktree(worktree_root).join("rag.db")
}

pub fn embeddings_api_base(chat_api_url: &str) -> String {
    if let Some(prefix) = chat_api_url.strip_suffix("/api/v0") {
        format!("{prefix}/v1")
    } else if chat_api_url.ends_with("/v1") {
        chat_api_url.trim_end_matches('/').to_string()
    } else {
        format!("{}/v1", chat_api_url.trim_end_matches('/'))
    }
}

pub fn chunk_text(text: &str, chunk_size: usize, overlap: usize) -> Vec<String> {
    let text = text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    if chunk_size == 0 {
        return vec![text.to_string()];
    }

    let step = chunk_size.saturating_sub(overlap);
    let step = step.max(1);
    let mut chunks = Vec::new();
    let mut start = 0usize;
    let n = text.len();

    while start < n {
        let mut end = (start + chunk_size).min(n);
        if end < n {
            let window_start = start + step / 2;
            if let Some(space) = text[window_start..end].rfind(' ') {
                end = window_start + space;
            }
        }
        let chunk = text[start..end].trim();
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }
        if end >= n {
            break;
        }
        start = if end > overlap { end - overlap } else { end };
    }

    chunks
}

#[derive(Deserialize)]
struct EmbeddingsResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

pub async fn embed_texts(
    http_client: Arc<dyn HttpClient>,
    api_base: &str,
    model: &str,
    texts: &[String],
) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }

    let url = format!("{}/embeddings", api_base.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "input": texts,
    });
    let body = serde_json::to_vec(&body)?;
    let mut response = http_client
        .post_json(&url, AsyncBody::from(body))
        .await
        .with_context(|| format!("failed to request embeddings from {url}"))?;

    let mut response_body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut response_body)
        .await
        .context("failed to read embeddings response")?;

    if !response.status().is_success() {
        let text = String::from_utf8_lossy(&response_body);
        bail!(
            "embeddings request failed with status {}: {text}",
            response.status().as_u16()
        );
    }

    let parsed: EmbeddingsResponse =
        serde_json::from_slice(&response_body).context("failed to parse embeddings response")?;
    let mut ordered = vec![Vec::new(); texts.len()];
    for item in parsed.data {
        if item.index >= ordered.len() {
            bail!("embeddings response index out of range");
        }
        ordered[item.index] = item.embedding;
    }
    if ordered.iter().any(Vec::is_empty) {
        bail!("embeddings response missing one or more vectors");
    }
    Ok(ordered)
}

pub struct RagIndex {
    connection: Mutex<Connection>,
}

impl RagIndex {
    pub fn open(worktree_root: &Path) -> Result<Self> {
        let db_path = rag_db_path(worktree_root);
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open_file(&db_path.to_string_lossy());
        connection.exec(
            "CREATE TABLE IF NOT EXISTS chunks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source TEXT NOT NULL,
                chunk_index INTEGER NOT NULL,
                text TEXT NOT NULL,
                dim INTEGER NOT NULL,
                embedding TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_chunks_source ON chunks(source);",
        )?()?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn count(&self) -> Result<usize> {
        let connection = self.connection.lock();
        let counts: Vec<i64> = connection.select("SELECT COUNT(*) FROM chunks")?()?;
        Ok(counts.into_iter().next().unwrap_or(0).max(0) as usize)
    }

    fn delete_source(&self, source: &str) -> Result<()> {
        let connection = self.connection.lock();
        connection.exec_bound("DELETE FROM chunks WHERE source = ?")?(source.to_string())?;
        Ok(())
    }

    fn insert_chunk(
        &self,
        source: &str,
        chunk_index: i32,
        text: &str,
        embedding: &[f32],
    ) -> Result<()> {
        let embedding_json = serde_json::to_string(embedding)?;
        let dim = embedding.len() as i32;
        let connection = self.connection.lock();
        connection.exec_bound(
            "INSERT INTO chunks (source, chunk_index, text, dim, embedding)
             VALUES (?, ?, ?, ?, ?)",
        )?((
            source.to_string(),
            chunk_index,
            text.to_string(),
            dim,
            embedding_json,
        ))?;
        Ok(())
    }

    fn read_text(path: &Path) -> Result<String> {
        std::fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))
    }

    pub async fn ingest_file(
        &self,
        path: &Path,
        http_client: Arc<dyn HttpClient>,
        api_base: &str,
        model: &str,
    ) -> Result<usize> {
        let text = Self::read_text(path)?;
        let source = path
            .canonicalize()
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned();
        self.ingest_text(&source, &text, http_client, api_base, model)
            .await
    }

    /// Ingest arbitrary text under an explicit source key (e.g. `web:https://…`).
    pub async fn ingest_text(
        &self,
        source: &str,
        text: &str,
        http_client: Arc<dyn HttpClient>,
        api_base: &str,
        model: &str,
    ) -> Result<usize> {
        let chunks = chunk_text(text, DEFAULT_CHUNK_SIZE, DEFAULT_CHUNK_OVERLAP);
        if chunks.is_empty() {
            return Ok(0);
        }

        self.delete_source(source)?;

        let mut inserted = 0usize;
        for batch_start in (0..chunks.len()).step_by(EMBED_BATCH_SIZE) {
            let batch_end = (batch_start + EMBED_BATCH_SIZE).min(chunks.len());
            let batch: Vec<String> = chunks[batch_start..batch_end].iter().cloned().collect();
            let vectors = embed_texts(http_client.clone(), api_base, model, &batch).await?;
            for (offset, (chunk, vector)) in batch.into_iter().zip(vectors).enumerate() {
                self.insert_chunk(source, (batch_start + offset) as i32, &chunk, &vector)?;
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    pub async fn ingest_path(
        &self,
        path: &Path,
        http_client: Arc<dyn HttpClient>,
        api_base: &str,
        model: &str,
    ) -> Result<(usize, usize)> {
        let files = if path.is_dir() {
            walkdir_files(path)?
        } else if path.is_file() {
            vec![path.to_path_buf()]
        } else {
            bail!("Path not found: {}", path.display());
        };

        let mut total_chunks = 0usize;
        let mut ingested_files = 0usize;
        for file in files {
            match self
                .ingest_file(&file, http_client.clone(), api_base, model)
                .await
            {
                Ok(chunks) if chunks > 0 => {
                    total_chunks += chunks;
                    ingested_files += 1;
                }
                Ok(_) => {}
                Err(error) => {
                    log::warn!("failed to ingest {}: {error:#}", file.display());
                }
            }
        }
        Ok((ingested_files, total_chunks))
    }

    pub async fn search(
        &self,
        query: &str,
        k: usize,
        http_client: Arc<dyn HttpClient>,
        api_base: &str,
        model: &str,
    ) -> Result<Vec<SearchHit>> {
        let query_vector = embed_texts(http_client, api_base, model, &[query.to_string()])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("missing query embedding"))?;

        let rows = self.load_rows()?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }

        let query_norm = vector_norm(&query_vector).max(f32::MIN_POSITIVE);
        let query_vector: Vec<f32> = query_vector.iter().map(|v| v / query_norm).collect();

        let mut scored = rows
            .into_iter()
            .filter_map(|row| {
                if row.embedding.len() != query_vector.len() {
                    return None;
                }
                let denom = vector_norm(&row.embedding).max(f32::MIN_POSITIVE);
                let score = dot(&query_vector, &row.embedding) / denom;
                Some(SearchHit {
                    score,
                    source: row.source,
                    chunk_index: row.chunk_index,
                    text: row.text,
                })
            })
            .collect::<Vec<_>>();

        scored.sort_by(|left, right| {
            right
                .score
                .partial_cmp(&left.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        Ok(scored)
    }

    fn load_rows(&self) -> Result<Vec<StoredChunk>> {
        let connection = self.connection.lock();
        let rows: Vec<(String, i32, String, i32, String)> =
            connection.select("SELECT source, chunk_index, text, dim, embedding FROM chunks")?()?;
        Ok(rows
            .into_iter()
            .filter_map(|(source, chunk_index, text, dim, embedding_json)| {
                let embedding: Vec<f32> = serde_json::from_str(&embedding_json).ok()?;
                if embedding.len() != dim as usize {
                    return None;
                }
                Some(StoredChunk {
                    source,
                    chunk_index,
                    text,
                    embedding,
                })
            })
            .collect())
    }
}

struct StoredChunk {
    source: String,
    chunk_index: i32,
    text: String,
    embedding: Vec<f32>,
}

fn walkdir_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("failed to read directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    let dotted = format!(".{ext}");
                    INCLUDE_EXTENSIONS.contains(&dotted.as_str())
                })
            {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

fn vector_norm(values: &[f32]) -> f32 {
    values.iter().map(|value| value * value).sum::<f32>().sqrt()
}

fn dot(left: &[f32], right: &[f32]) -> f32 {
    left.iter().zip(right).map(|(a, b)| a * b).sum()
}

pub struct RagIndexCache {
    indexes: Mutex<HashMap<PathBuf, Arc<RagIndex>>>,
}

impl RagIndexCache {
    pub fn new() -> Self {
        Self {
            indexes: Mutex::new(HashMap::new()),
        }
    }

    pub fn for_worktree(&self, worktree_root: &Path) -> Result<Arc<RagIndex>> {
        let key = worktree_root
            .canonicalize()
            .unwrap_or_else(|_| worktree_root.to_path_buf());
        let mut indexes = self.indexes.lock();
        if let Some(index) = indexes.get(&key) {
            return Ok(index.clone());
        }
        let index = Arc::new(RagIndex::open(&key)?);
        indexes.insert(key, index.clone());
        Ok(index)
    }
}

pub fn default_top_k() -> usize {
    DEFAULT_TOP_K
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_text_overlap() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let chunks = chunk_text(text, 20, 5);
        assert!(!chunks.is_empty());
        assert!(chunks.len() > 1);
    }

    #[test]
    fn test_embeddings_api_base() {
        assert_eq!(
            embeddings_api_base("http://localhost:1234/api/v0"),
            "http://localhost:1234/v1"
        );
    }
}
