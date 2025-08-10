use crate::{config::get_config, debrid::Debrid};
use anyhow::Result;
use chunk::{Chunk, ChunkPriority};
use entry::CacheEntry;
use ratelimiter::Ratelimiter;
use sqlx::SqlitePool;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock, atomic::Ordering},
    time::Duration,
};

mod chunk;
mod downloader;
mod entry;
mod ratelimiter;
mod reader;

pub struct CacheFile {
    pub id: i64,
    pub size: i64,
    pub path: String,
    pub file_debrid_id: i64,
    pub torrent_debrid_id: i64,
}

pub struct Cache {
    pool: SqlitePool,
    ratelimiter: Arc<Ratelimiter>,
    debrid: Arc<Debrid>,
    entries: RwLock<HashMap<i64, Arc<CacheEntry>>>,
}

impl Cache {
    pub async fn load(pool: &SqlitePool, debrid: Arc<Debrid>) -> Result<Arc<Self>> {
        let ratelimiter = Arc::new(Ratelimiter::new());

        let cache_dir = get_config().cache_dir.as_ref().unwrap();
        let mut files = tokio::fs::read_dir(&cache_dir).await?;
        let mut entries = HashMap::new();
        while let Some(entry) = files.next_entry().await? {
            // {file_id}.bin
            let file_name = entry.file_name();
            let file_name = file_name.to_string_lossy();
            if !file_name.ends_with(".bin") {
                continue;
            }

            let Some(file_id) = file_name.strip_suffix(".bin").and_then(|s| s.parse::<i64>().ok()) else {
                tracing::warn!("Cache file {} does not have a valid ID, skipping it.", file_name);
                continue;
            };

            let result = sqlx::query_as!(
                CacheFile,
                r#"SELECT tf.id, tf.size, tf.path, tf.debrid_id as file_debrid_id, t.debrid_id as "torrent_debrid_id!"
                FROM torrent_files tf
                JOIN torrents t ON t.id = tf.torrent_id
                WHERE tf.id = ? AND t.debrid_id IS NOT NULL"#,
                file_id
            )
            .fetch_optional(pool)
            .await?;

            let Some(file) = result else {
                tracing::warn!(
                    "Cache file {} does not have a corresponding torrent file entry, removing it.",
                    file_name
                );
                tokio::fs::remove_file(entry.path()).await?;
                let cache_meta_path = entry.path().with_extension("cachemeta");
                if cache_meta_path.exists() {
                    tokio::fs::remove_file(cache_meta_path).await?;
                }
                continue;
            };

            let file_id = file.id;
            let entry = CacheEntry::load(file, debrid.clone(), ratelimiter.clone());
            entries.insert(file_id, Arc::new(entry));
        }

        let entries = RwLock::new(entries);
        Ok(Arc::new(Cache {
            pool: pool.clone(),
            ratelimiter,
            entries,
            debrid,
        }))
    }

    pub fn upsert_entry(&self, file: CacheFile) -> Arc<CacheEntry> {
        let entries = self.entries.read().unwrap();
        if let Some(entry) = entries.get(&file.id) {
            return entry.clone();
        }

        drop(entries);

        let mut entries = self.entries.write().unwrap();
        if let Some(entry) = entries.get(&file.id) {
            return entry.clone();
        }

        let file_id = file.id;
        let entry = CacheEntry::load(file, self.debrid.clone(), self.ratelimiter.clone());
        let entry = Arc::new(entry);
        entries.insert(file_id, entry.clone());
        entry
    }

    pub async fn start_sweeper(&self) -> Result<()> {
        let config = get_config();
        let sweep_duration = Duration::from_secs(config.cache_sweep_interval_secs);

        // todo: total_size_bytes does not account for sections that were partially written
        // but failed half way through, leaving bytes that aren't marked as cached
        // but still exist on disk. over time this could lead the cache size to grow
        // beyond what we expect. we should probably check the physical size of the
        // files on disk and use that as a base, or maybe have some kind of "repair" that punches
        // holes in uncached sections to ensure they're gone.
        loop {
            tokio::time::sleep(sweep_duration).await;
            tracing::info!("starting cache sweep");

            let mut all_chunks: Vec<(Arc<CacheEntry>, Arc<Chunk>, ChunkPriority)> = Vec::new();
            let mut total_size_bytes = 0;

            let entries = {
                let entries = self.entries.read().unwrap();
                entries.values().cloned().collect::<Vec<_>>()
            };

            for entry in entries.into_iter() {
                let file_id = entry.get_file().id;
                let file = sqlx::query!(r#"SELECT id FROM torrent_files WHERE id = ?"#, file_id)
                    .fetch_optional(&self.pool)
                    .await?;

                if file.is_none() {
                    // this cache entry no longer has a reference that is using it.
                    // we can delete its data.
                    let was_removed = entry.try_remove().await?;
                    if was_removed {
                        tracing::info!("removing cached file {}, torrent was removed", file_id);
                        {
                            let mut entries = self.entries.write().unwrap();
                            entries.remove(&file_id);
                        }
                        continue;
                    } else {
                        tracing::info!("cached file {} was not removed, but torrent was removed", file_id);
                    }
                }

                for chunk in entry.get_chunks() {
                    let is_cached = chunk.cached.load(Ordering::Relaxed);
                    if !is_cached {
                        continue;
                    }

                    let file = entry.get_file();
                    let priority = chunk.get_priority(file.size as u64);
                    all_chunks.push((entry.clone(), chunk.clone(), priority));
                    total_size_bytes += chunk.size;
                }
            }

            if total_size_bytes < config.cache_max_size {
                let total_size_mb = total_size_bytes / (1024 * 1024);
                tracing::info!(
                    "cache sweep finished, size is below threshold, total size is {} MB",
                    total_size_mb
                );
                continue;
            }

            all_chunks.sort_by(|a, b| {
                // sort by priority first (lower is higher priority)
                // then sort by last accessed time (higher is higher priority)
                let a_priority = a.2 as u64;
                let b_priority = b.2 as u64;
                if a_priority != b_priority {
                    return a_priority.cmp(&b_priority);
                }

                let a_accessed = a.1.accessed_at_secs.load(Ordering::Relaxed);
                let b_accessed = b.1.accessed_at_secs.load(Ordering::Relaxed);
                a_accessed.cmp(&b_accessed)
            });

            let mut total_removed_bytes = 0;
            for (entry, chunk, priority) in all_chunks.into_iter() {
                let file_id = entry.get_file().id;
                tracing::info!(
                    "removing chunk {} for file {}, priority {:?}",
                    chunk.index,
                    file_id,
                    priority
                );

                let removed = chunk.try_remove(entry).await?;
                if removed {
                    total_removed_bytes += chunk.size;
                    total_size_bytes -= chunk.size;
                    if total_size_bytes < config.cache_target_size {
                        break;
                    }
                }
            }

            let removed_mb = total_removed_bytes / (1024 * 1024);
            let total_mb = total_size_bytes / (1024 * 1024);
            tracing::info!(
                "cache sweep finished, removed {} MB, total size is {} MB after sweep",
                removed_mb,
                total_mb
            );
        }
    }
}
