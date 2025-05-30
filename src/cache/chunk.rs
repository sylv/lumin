use super::entry::CacheEntry;
use crate::config::get_config;
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    ops::RangeInclusive,
    os::fd::{AsFd, AsRawFd},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};
use tokio::sync::Mutex;

// This is the size of an individual chunk.
// Generally, chunks will be batched together into a single request.
// DO NOT CHANGE or it will break existing caches.
// todo: Ideally this should be serialized in cachemeta.
pub const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024; // 8MB

const PRIORITY_RANGES: [(RangeInclusive<u64>, ChunkPriority); 3] = [
    (0..=20, ChunkPriority::High),
    (21..=95, ChunkPriority::Low),
    (96..=100, ChunkPriority::Medium),
];

#[derive(Debug, Clone, Copy, PartialOrd, Ord, PartialEq, Eq)]
pub enum ChunkPriority {
    GracePeriod,
    Preloaded,
    FirstChunk,
    LastChunk,
    High,
    Medium,
    Low,
}

#[derive(Debug, Serialize)]
pub struct Chunk {
    pub index: u64,
    pub offset: u64,
    pub size: u64,
    pub accessed_at_secs: AtomicU64,
    pub cached: AtomicBool,
    #[serde(skip)]
    pub downloading: Arc<Mutex<()>>,
}

impl Chunk {
    pub fn new(index: u64, size: u64) -> Self {
        let now = chrono::Utc::now().timestamp() as u64;
        let offset = index * DEFAULT_CHUNK_SIZE;
        Self {
            index,
            offset,
            size,
            accessed_at_secs: AtomicU64::new(now),
            cached: AtomicBool::new(false),
            downloading: Arc::new(Mutex::new(())),
        }
    }

    pub fn get_priority(&self, file_size: u64) -> ChunkPriority {
        let now = chrono::Utc::now().timestamp() as u64;
        let accessed_at = self
            .accessed_at_secs
            .load(std::sync::atomic::Ordering::Relaxed);

        if now.abs_diff(accessed_at) < get_config().cache_grace_period_secs {
            return ChunkPriority::GracePeriod;
        }

        // the first and last chunks generally contain metadata that we want to keep
        // so that ffprobe/etc can read the file without requesting new data
        if self.index == 0 {
            return ChunkPriority::FirstChunk;
        }

        let total_chunks = (file_size + DEFAULT_CHUNK_SIZE - 1) / DEFAULT_CHUNK_SIZE;
        if self.index == total_chunks - 1 {
            return ChunkPriority::LastChunk;
        }

        let config = get_config();
        if let Some((preload_start, preload_end)) = config.chunk_preload {
            if self.index <= preload_start {
                return ChunkPriority::Preloaded;
            }

            let total_chunks = (file_size + DEFAULT_CHUNK_SIZE - 1) / DEFAULT_CHUNK_SIZE;
            if self.index >= total_chunks - preload_end {
                return ChunkPriority::Preloaded;
            }
        }

        // then we use percent-based ranges based on the center of the chunk
        let center = self.offset + self.size / 2;
        let percent = (center * 100) / file_size;
        for (range, priority) in PRIORITY_RANGES.iter() {
            if range.contains(&percent) {
                return *priority;
            }
        }

        panic!(
            "chunk priority wasn't found for chunk {} ({}%)",
            self.index, percent
        );
    }

    pub fn is_cached_or_downloading(&self) -> bool {
        if self.cached.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }

        let downloading = self.downloading.try_lock();
        if downloading.is_err() {
            return true;
        }

        false
    }

    pub async fn try_remove(&self, file: Arc<CacheEntry>) -> Result<bool> {
        let Ok(download_lock) = self.downloading.try_lock() else {
            // chunk is already being downloaded by something else, we can skip it
            tracing::warn!("Chunk {} is being downloaded, skipping removal", self.index);
            return Ok(false);
        };

        let is_cached = self.cached.load(Ordering::SeqCst);
        if !is_cached {
            bail!(
                "chunk {} is not cached, removing it is unnecessary",
                self.index
            );
        }

        let fd = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(file.get_cache_path())
            .await?;

        unsafe {
            libc::fallocate64(
                fd.as_fd().as_raw_fd(),
                libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                self.offset.try_into().unwrap(),
                self.size.try_into().unwrap(),
            );
        }

        self.cached.store(false, Ordering::SeqCst);
        drop(download_lock);
        drop(fd);
        file.flush_cache_meta()
            .map_err(|e| {
                tracing::error!("Failed to flush metadata: {}", e);
            })
            .ok();

        tracing::info!("Chunk {} removed", self.index);
        Ok(true)
    }
}

impl PartialEq for Chunk {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl<'de> serde::Deserialize<'de> for Chunk {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ChunkData {
            index: u64,
            size: u64,
            offset: u64,
            accessed_at_secs: AtomicU64,
            cached: AtomicBool,
        }

        let data = ChunkData::deserialize(deserializer)?;

        Ok(Chunk {
            index: data.index,
            size: data.size,
            offset: data.offset,
            accessed_at_secs: data.accessed_at_secs,
            cached: data.cached,
            downloading: Arc::new(Mutex::new(())),
        })
    }
}

pub fn get_chunk_size_from_index(index: u64, file_size: u64) -> u64 {
    let total_chunks = (file_size + DEFAULT_CHUNK_SIZE - 1) / DEFAULT_CHUNK_SIZE;
    if index == total_chunks - 1 {
        return file_size % DEFAULT_CHUNK_SIZE;
    }

    DEFAULT_CHUNK_SIZE
}

pub fn serialize_chunks(chunks: &[Arc<Chunk>], file: &mut std::fs::File) -> Result<()> {
    serde_json::to_writer(file, chunks)?;
    Ok(())
}

pub fn deserialize_chunks(file: &mut std::fs::File) -> Result<Vec<Arc<Chunk>>> {
    let chunks: Vec<Arc<Chunk>> = serde_json::from_reader(file)?;
    Ok(chunks)
}
