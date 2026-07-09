//! Disk-backed NAR cache with moka-managed metadata
//!
//! Compressed NAR files are stored on disk under a configurable directory.
//! `moka::future::Cache` manages in-memory metadata (file path, compressed
//! hash, compressed size) with time-to-idle and weighted-size eviction.
//! An async eviction listener deletes the backing file when moka evicts
//! an entry.

use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use async_compression::tokio::bufread::ZstdEncoder;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use moka::future::Cache;
use sha2::Digest;
use sha2::Sha256;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;

use crate::config::CacheConfig;

/// Metadata for a cached, zstd-compressed NAR file on disk.
pub struct CachedNar {
    /// Guarded path to the `.nar.zst` file on disk.
    ///
    /// Set to `None` by the eviction listener just before deleting the file.
    /// Readers acquire the read lock and open the file before releasing it,
    /// so the fd keeps the inode alive even after unlink.
    path: tokio::sync::RwLock<Option<PathBuf>>,

    /// SHA-256 of the compressed file content, in SRI format (`sha256-BASE64==`).
    pub file_hash: String,

    /// Size of the compressed file in bytes.
    pub file_size: u64,
}

impl CachedNar {
    /// Open the cached NAR file for reading.
    ///
    /// Uses `try_read` rather than `read` because the write lock is only
    /// ever taken by the eviction listener, which sets the path to `None`
    /// and never releases it. If the write lock is held, the path is
    /// already gone and there is no point waiting.
    ///
    /// Returns `None` if the entry is being evicted or has already been evicted.
    async fn open(&self) -> Result<Option<tokio::fs::File>> {
        let guard = match self.path.try_read() {
            Ok(g) => g,
            Err(_) => return Ok(None),
        };
        let Some(path) = guard.as_ref() else {
            return Ok(None);
        };
        let file = tokio::fs::File::open(path)
            .await
            .with_context(|| format!("Failed to open cached NAR: {}", path.display()))?;
        Ok(Some(file))
    }
}

/// Disk-backed NAR cache with moka-managed eviction.
pub struct NarCache {
    cache: Cache<String, Arc<CachedNar>>,
    dir: PathBuf,
}

static TMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl NarCache {
    /// Create a new NAR cache from configuration.
    ///
    /// Creates the cache directory if it does not exist and builds the
    /// moka cache with the configured eviction policies.
    pub async fn new(config: &CacheConfig) -> Result<Self> {
        tokio::fs::create_dir_all(&config.dir)
            .await
            .with_context(|| format!("Failed to create cache dir: {}", config.dir.display()))?;

        let mut builder = Cache::builder()
            .weigher(|_key: &String, value: &Arc<CachedNar>| -> u32 {
                value.file_size.min(u32::MAX as u64) as u32
            })
            .async_eviction_listener(|_key, value: Arc<CachedNar>, cause| {
                Box::pin(async move {
                    // Take the path under the write lock so no new readers
                    // can open the file after this point.
                    let path = value.path.write().await.take();
                    if let Some(path) = path {
                        tracing::debug!(
                            "Evicting cached NAR {} (cause: {:?})",
                            path.display(),
                            cause,
                        );
                        if let Err(e) = tokio::fs::remove_file(&path).await {
                            tracing::warn!(
                                "Failed to delete evicted NAR {}: {}",
                                path.display(),
                                e,
                            );
                        }
                    }
                })
            });

        if config.max_size_bytes > 0 {
            builder = builder.max_capacity(config.max_size_bytes);
        }

        if config.time_to_idle_secs > 0 {
            builder = builder.time_to_idle(Duration::from_secs(config.time_to_idle_secs));
        }

        let cache = builder.build();

        Ok(Self {
            cache,
            dir: config.dir.clone(),
        })
    }

    /// Ensure a NAR is cached, generating it if absent. Returns metadata only.
    ///
    /// Uses moka's `try_get_with` to coalesce concurrent requests for the
    /// same uncached NAR into a single generation.
    pub async fn ensure(
        &self,
        store_hash: &str,
        store_path: &Path,
    ) -> Result<Arc<CachedNar>, Arc<anyhow::Error>> {
        let dir = self.dir.clone();
        let store_path = store_path.to_path_buf();

        self.cache
            .try_get_with(store_hash.to_string(), async move {
                generate_cached_nar(&dir, &store_path).await
            })
            .await
    }

    /// Ensure a NAR is cached and open the file for reading.
    ///
    /// Like [`ensure`](Self::ensure), but also opens the backing file under
    /// the `RwLock` before returning so the fd survives eviction.
    ///
    /// The file handle is `None` if the entry was evicted between the cache
    /// lookup and the open call.
    pub async fn ensure_and_open(
        &self,
        store_hash: &str,
        store_path: &Path,
    ) -> Result<(Arc<CachedNar>, Option<tokio::fs::File>), Arc<anyhow::Error>> {
        let cached = self.ensure(store_hash, store_path).await?;

        let file = cached.open().await.map_err(Arc::new)?;

        Ok((cached, file))
    }

    /// Cache NAR data fetched from a peer.
    ///
    /// Writes already-compressed bytes to disk and inserts the metadata into
    /// the moka cache. Validates that the SHA-256 of the data matches the
    /// expected `nar_hash` parameter — mismatched data is rejected.
    ///
    /// If the moka cache already has an entry, it is returned directly
    /// (idempotent). If the file already exists on disk but is not in
    /// the cache, it is hashed and inserted.
    pub async fn cache_fetched_nar(
        &self,
        store_hash: &str,
        store_name: &str,
        nar_hash: &str,
        data: &[u8],
    ) -> Result<Arc<CachedNar>, Arc<anyhow::Error>> {
        if let Some(cached) = self.cache.get(store_hash).await {
            return Ok(cached);
        }

        let final_path = self.dir.join(format!("{store_name}.nar.zst"));
        let tmp_path = self.dir.join(format!(
            "{store_name}.nar.zst.tmp.{}",
            TMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));

        if let Ok(meta) = tokio::fs::metadata(&final_path).await {
            let file_size = meta.len();
            let file_hash = hash_file(&final_path).await?;
            let cached = Arc::new(CachedNar {
                path: tokio::sync::RwLock::new(Some(final_path)),
                file_hash,
                file_size,
            });
            self.cache
                .insert(store_hash.to_string(), cached.clone())
                .await;
            return Ok(cached);
        }

        let mut hasher = Sha256::new();
        hasher.update(data);
        let digest = hasher.finalize();
        let computed_hash = format!("sha256-{}", STANDARD.encode(digest));

        if computed_hash != nar_hash {
            return Err(Arc::new(anyhow::anyhow!(
                "NAR hash mismatch for {}: expected {}, computed {}",
                store_name,
                nar_hash,
                computed_hash,
            )));
        }

        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .with_context(|| format!("Failed to create temp NAR file: {}", tmp_path.display()))?;
        file.write_all(data)
            .await
            .with_context(|| format!("Failed to write NAR data: {}", tmp_path.display()))?;
        file.flush()
            .await
            .with_context(|| format!("Failed to flush NAR data: {}", tmp_path.display()))?;
        drop(file);

        tokio::fs::rename(&tmp_path, &final_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to rename {} -> {}",
                    tmp_path.display(),
                    final_path.display()
                )
            })?;

        // After writing, check the cache again — a concurrent caller may have
        // already inserted an entry for this key. If so, return it to avoid
        // evicting the existing entry (which would trigger file deletion).
        if let Some(cached) = self.cache.get(store_hash).await {
            return Ok(cached);
        }

        let file_size = data.len() as u64;
        let cached = Arc::new(CachedNar {
            path: tokio::sync::RwLock::new(Some(final_path)),
            file_hash: computed_hash,
            file_size,
        });

        self.cache
            .insert(store_hash.to_string(), cached.clone())
            .await;

        tracing::info!(
            "Cached fetched NAR {} ({} bytes compressed)",
            store_name,
            file_size,
        );

        Ok(cached)
    }

    /// Scan the cache directory and re-populate the moka cache from existing files.
    ///
    /// This allows the cache to survive daemon restarts. For each `.nar.zst`
    /// file found, we stat it, compute its SHA-256, and insert the metadata
    /// into the moka cache.
    pub async fn recover(&self) -> Result<usize> {
        let mut count = 0;
        let mut read_dir = tokio::fs::read_dir(&self.dir).await?;

        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let name = match entry.file_name().to_str().map(String::from) {
                Some(n) => n,
                None => continue,
            };

            // Delete leftover temp files from interrupted generations
            if name.ends_with(".nar.zst.tmp") {
                let path = entry.path();
                tracing::info!("Deleting leftover temp file: {}", path.display());
                if let Err(e) = tokio::fs::remove_file(&path).await {
                    tracing::warn!("Failed to delete temp file {}: {}", path.display(), e);
                }
                continue;
            }

            if !name.ends_with(".nar.zst") {
                continue;
            }

            // Extract store hash: first 32 chars of the filename
            let store_hash = match name.get(..32) {
                Some(h) if h.len() == 32 && h.chars().all(|c| c.is_ascii_alphanumeric()) => h,
                _ => continue,
            };

            let path = entry.path();
            let meta = match tokio::fs::metadata(&path).await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("Failed to stat cached NAR {}: {}", path.display(), e);
                    continue;
                }
            };

            let file_size = meta.len();
            let file_hash = match hash_file(&path).await {
                Ok(h) => h,
                Err(e) => {
                    tracing::warn!("Failed to hash cached NAR {}: {}", path.display(), e);
                    continue;
                }
            };

            let cached = Arc::new(CachedNar {
                path: tokio::sync::RwLock::new(Some(path)),
                file_hash,
                file_size,
            });

            self.cache.insert(store_hash.to_string(), cached).await;
            count += 1;
        }

        tracing::info!("Recovered {} cached NARs from disk", count);
        Ok(count)
    }
}

/// Generate a zstd-compressed NAR file, write it to disk, and return metadata.
///
/// 1. Spawns `nix-store --dump <store_path>` piped through `ZstdEncoder`.
/// 2. Writes compressed output to `{store_name}.nar.zst.tmp`.
/// 3. Computes SHA-256 of the compressed content while writing.
/// 4. Renames to `{store_name}.nar.zst`.
/// 5. Returns `CachedNar` with the file path, hash, and size.
async fn generate_cached_nar(cache_dir: &Path, store_path: &Path) -> Result<Arc<CachedNar>> {
    let store_name = store_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("Invalid store path: {}", store_path.display()))?;

    let final_path = cache_dir.join(format!("{store_name}.nar.zst"));
    let tmp_path = cache_dir.join(format!("{store_name}.nar.zst.tmp"));

    // If the final file already exists on disk (e.g. recovery raced with
    // generation), use it directly.
    if let Ok(meta) = tokio::fs::metadata(&final_path).await {
        let file_size = meta.len();
        let file_hash = hash_file(&final_path).await?;
        return Ok(Arc::new(CachedNar {
            path: tokio::sync::RwLock::new(Some(final_path)),
            file_hash,
            file_size,
        }));
    }

    let mut child = Command::new("nix-store")
        .arg("--dump")
        .arg(store_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn nix-store --dump for {}",
                store_path.display()
            )
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("Failed to capture stdout"))?;

    let buf_reader = BufReader::new(stdout);
    let mut encoder = ZstdEncoder::new(buf_reader);

    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .with_context(|| format!("Failed to create temp NAR file: {}", tmp_path.display()))?;

    let mut hasher = Sha256::new();
    let mut file_size: u64 = 0;
    let mut buf = vec![0u8; 8192];

    loop {
        let n = encoder.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).await?;
        file_size += n as u64;
    }
    file.flush().await?;
    drop(file);

    let status = child.wait().await?;
    if !status.success() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(anyhow::anyhow!(
            "nix-store --dump failed for {}",
            store_path.display()
        ));
    }

    let digest = hasher.finalize();
    let file_hash = format!("sha256-{}", STANDARD.encode(digest));

    tokio::fs::rename(&tmp_path, &final_path)
        .await
        .with_context(|| {
            format!(
                "Failed to rename {} -> {}",
                tmp_path.display(),
                final_path.display()
            )
        })?;

    tracing::info!(
        "Cached NAR {} ({} bytes compressed)",
        final_path.display(),
        file_size,
    );

    Ok(Arc::new(CachedNar {
        path: tokio::sync::RwLock::new(Some(final_path)),
        file_hash,
        file_size,
    }))
}

/// Compute the SHA-256 hash of a file in SRI format.
async fn hash_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let digest = hasher.finalize();
    Ok(format!("sha256-{}", STANDARD.encode(digest)))
}

/// Compute the SRI-format SHA-256 hash of arbitrary bytes.
#[cfg(test)]
fn hash_bytes(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    format!("sha256-{}", STANDARD.encode(digest))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cache_config(dir: &Path) -> CacheConfig {
        CacheConfig {
            dir: dir.to_path_buf(),
            time_to_idle_secs: 0,
            max_size_bytes: 0,
        }
    }

    #[tokio::test]
    async fn test_cache_fetched_nar_success() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_cache_config(tmp.path());
        let cache = NarCache::new(&config).await.unwrap();

        let data = b"hello world nar data";
        let nar_hash = hash_bytes(data);
        let cached = cache
            .cache_fetched_nar("abc123", "abc123-mypackage", &nar_hash, data)
            .await
            .unwrap();

        assert_eq!(cached.file_size, data.len() as u64);
        assert_eq!(cached.file_hash, nar_hash);

        let path_guard = cached.path.read().await;
        let path = path_guard.as_ref().unwrap();
        assert!(path.exists());

        let on_disk = tokio::fs::read(path).await.unwrap();
        assert_eq!(on_disk, data);

        let final_path = tmp.path().join("abc123-mypackage.nar.zst");
        assert!(final_path.exists());
        let entries = std::fs::read_dir(tmp.path()).unwrap();
        assert!(
            entries
                .filter_map(|e| e.ok())
                .all(|e| { !e.file_name().to_string_lossy().contains(".tmp") }),
            "temp files should be cleaned up after rename"
        );
    }

    #[tokio::test]
    async fn test_cache_fetched_nar_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_cache_config(tmp.path());
        let cache = NarCache::new(&config).await.unwrap();

        let data = b"some nar content";
        let wrong_hash = "sha256-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let result = cache
            .cache_fetched_nar("def456", "def456-myackage", wrong_hash, data)
            .await;
        match result {
            Err(e) => {
                let err_msg = e.to_string();
                assert!(
                    err_msg.contains("hash mismatch"),
                    "expected hash mismatch error, got: {err_msg}",
                );
            }
            Ok(_) => panic!("expected hash mismatch error"),
        }

        let final_path = tmp.path().join("def456-myackage.nar.zst");
        assert!(
            !final_path.exists(),
            "file should not be written on hash mismatch"
        );
    }

    #[tokio::test]
    async fn test_cache_fetched_nar_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_cache_config(tmp.path());
        let cache = NarCache::new(&config).await.unwrap();

        let data = b"nar content for idempotent test";
        let nar_hash = hash_bytes(data);

        let cached1 = cache
            .cache_fetched_nar("ghi789", "ghi789-pkg", &nar_hash, data)
            .await
            .unwrap();

        let cached2 = cache
            .cache_fetched_nar("ghi789", "ghi789-pkg", &nar_hash, data)
            .await
            .unwrap();

        assert_eq!(cached1.file_size, cached2.file_size);
        assert_eq!(cached1.file_hash, cached2.file_hash);

        let final_path = tmp.path().join("ghi789-pkg.nar.zst");
        assert!(final_path.exists());
    }

    #[tokio::test]
    async fn test_cache_fetched_nar_preexisting_file() {
        let tmp = tempfile::tempdir().unwrap();

        let store_name = "jkl012-preexist-pkg";
        let final_path = tmp.path().join(format!("{store_name}.nar.zst"));

        let existing_data = b"existing file on disk";
        let existing_hash = hash_bytes(existing_data);

        tokio::fs::write(&final_path, existing_data).await.unwrap();

        let config = make_cache_config(tmp.path());
        let cache = NarCache::new(&config).await.unwrap();

        let new_data = b"different data that should be ignored";
        let new_hash = hash_bytes(new_data);

        let cached = cache
            .cache_fetched_nar("jkl012", store_name, &new_hash, new_data)
            .await
            .unwrap();

        assert_eq!(cached.file_hash, existing_hash);
        assert_eq!(cached.file_size, existing_data.len() as u64);

        let on_disk = tokio::fs::read(&final_path).await.unwrap();
        assert_eq!(on_disk, existing_data);
    }

    #[tokio::test]
    async fn test_cache_fetched_nar_concurrent_same_key() {
        let tmp = tempfile::tempdir().unwrap();
        let config = make_cache_config(tmp.path());
        let cache = NarCache::new(&config).await.unwrap();

        let data = b"concurrent nar data";
        let nar_hash = hash_bytes(data);

        let h1 = cache.cache_fetched_nar("mno345", "mno345-concurrent", &nar_hash, data);
        let h2 = cache.cache_fetched_nar("mno345", "mno345-concurrent", &nar_hash, data);

        let (r1, r2) = tokio::join!(h1, h2);
        let cached1 = r1.unwrap();
        let cached2 = r2.unwrap();

        assert_eq!(cached1.file_hash, cached2.file_hash);
        assert_eq!(cached1.file_size, cached2.file_size);

        let final_path = tmp.path().join("mno345-concurrent.nar.zst");
        assert!(final_path.exists());
    }
}
