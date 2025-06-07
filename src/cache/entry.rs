use super::{
    chunk::{
        Chunk, DEFAULT_CHUNK_SIZE, deserialize_chunks, get_chunk_size_from_index, serialize_chunks,
    },
    downloader::download_contiguous_chunks,
    ratelimiter::Ratelimiter,
    reader::Readers,
};
use crate::{config::get_config, debrid::Debrid, entities::torrent_files};
use anyhow::Result;
use std::{
    io::SeekFrom,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::OwnedMutexGuard,
    time::sleep,
};

struct ReadAheadTier {
    after_reading_secs: u64,
    when_secs_until_uncached: u64,
    then_buffer_secs: u64,
}

// read ahead is essentially when we are within READ_AHEAD_START_BYTES of the start of uncached chunks,
// we download enough chunks to have a buffer of READ_AHEAD_TARGET_BYTES bytes.
// these are the default values used when a duration hint is not available.
const READ_AHEAD_START_BYTES: u64 = 24 * 1024 * 1024;
const READ_AHEAD_TARGET_BYTES: u64 = 64 * 1024 * 1024;

// if we have a duration hint, we use these values instead, which are
// based on the duration hint and file size. it allows for much more accurate
// read ahead values and less wasted bandwidth.
const READ_AHEAD_TIERS: [ReadAheadTier; 2] = [
    ReadAheadTier {
        after_reading_secs: 0,
        when_secs_until_uncached: 10,
        then_buffer_secs: 60,
    },
    ReadAheadTier {
        after_reading_secs: 30,
        when_secs_until_uncached: 30,
        then_buffer_secs: 120,
    },
];

pub struct CacheEntry {
    file: torrent_files::Model,
    remote_torrent_id: i64,
    readers: Readers,
    chunks: Vec<Arc<Chunk>>,
    debrid: Arc<Debrid>,
    ratelimiter: Arc<Ratelimiter>,
    duration_hint_secs: AtomicU64,
}

impl CacheEntry {
    pub fn load(
        file: torrent_files::Model,
        remote_torrent_id: i64,
        debrid: Arc<Debrid>,
        ratelimiter: Arc<Ratelimiter>,
    ) -> Self {
        let meta_path = get_config()
            .cache_dir
            .as_ref()
            .unwrap()
            .join(format!("{}.cachemeta", file.id));

        let chunks = if meta_path.exists() {
            let mut file = std::fs::File::open(&meta_path).unwrap();
            deserialize_chunks(&mut file).expect("failed to deserialize chunk metadata")
        } else {
            let mut chunks = Vec::new();
            let total_chunks = (file.size as u64 + DEFAULT_CHUNK_SIZE - 1) / DEFAULT_CHUNK_SIZE;
            for i in 0..total_chunks {
                let chunk_size = get_chunk_size_from_index(i, file.size as u64);
                let chunk = Chunk::new(i, chunk_size);
                chunks.push(Arc::new(chunk));
            }

            chunks
        };

        let duration_hint_secs = file.duration_hint_secs.unwrap_or(0);
        let readers = Readers::new();

        Self {
            file,
            remote_torrent_id,
            debrid,
            ratelimiter,
            readers,
            chunks,
            duration_hint_secs: AtomicU64::new(duration_hint_secs as u64),
        }
    }

    pub fn get_cache_path(&self) -> PathBuf {
        get_config()
            .cache_dir
            .as_ref()
            .unwrap()
            .join(format!("{}.bin", self.file.id))
    }

    pub fn set_duration_hint_secs(&self, secs: u64) {
        self.duration_hint_secs.store(secs, Ordering::SeqCst);
    }

    pub fn get_duration_hint_secs(&self) -> Option<u64> {
        let secs = self.duration_hint_secs.load(Ordering::SeqCst);
        if secs > 0 { Some(secs) } else { None }
    }

    pub fn get_file(&self) -> &torrent_files::Model {
        &self.file
    }

    pub fn get_chunks(&self) -> &[Arc<Chunk>] {
        &self.chunks
    }

    pub fn get_remote_ids(&self) -> (i64, i64) {
        (self.remote_torrent_id, self.file.remote_id)
    }

    pub fn has_cached_chunks(&self) -> bool {
        self.chunks.iter().any(|c| c.cached.load(Ordering::SeqCst))
    }

    fn get_meta_path(&self) -> PathBuf {
        self.get_cache_path().with_extension("cachemeta")
    }

    pub fn flush_cache_meta(&self) -> Result<()> {
        let meta_path = self.get_meta_path();
        let mut file = std::fs::File::create(&meta_path).unwrap();
        serialize_chunks(&self.chunks, &mut file)?;
        Ok(())
    }

    pub async fn try_remove(&self) -> Result<bool> {
        let mut guards = Vec::new();
        for chunk in self.chunks.iter() {
            let Ok(guard) = chunk.downloading.try_lock() else {
                return Ok(false);
            };

            guards.push(guard);
        }

        tokio::fs::remove_file(self.get_cache_path()).await?;
        tokio::fs::remove_file(self.get_meta_path()).await?;

        Ok(true)
    }

    fn get_preload_chunks(&self) -> Option<Vec<Arc<Chunk>>> {
        let config = get_config();
        if let Some(preload) = config.chunk_preload {
            let total_chunks = self.chunks.len() as u64;
            if total_chunks <= preload.0 + preload.1 + 1 {
                return Some(self.chunks.clone());
            }

            let preload_end_index = total_chunks - preload.1;
            let start_chunks = &self.chunks[0..=preload.0 as usize];
            let end_chunks = &self.chunks[preload_end_index as usize..];
            return Some(
                start_chunks
                    .iter()
                    .chain(end_chunks.iter())
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }

        None
    }

    fn get_read_ahead_chunks(
        &self,
        current_chunk_idx: u64,
        reader_bytes_read: u64,
    ) -> Option<Vec<Arc<Chunk>>> {
        let duration_hint_secs = self.duration_hint_secs.load(Ordering::SeqCst);
        let (read_ahead_trigger, read_ahead_target) = if duration_hint_secs > 0 {
            let bytes_per_sec = self.file.size as u64 / duration_hint_secs as u64;
            let seconds_read = reader_bytes_read / bytes_per_sec;
            tracing::trace!("reader seconds read: {}", seconds_read);

            let tier = READ_AHEAD_TIERS
                .iter()
                .rev()
                .find(|tier| seconds_read >= tier.after_reading_secs)
                .expect("failed to find a valid read ahead tier");

            // todo: account for time-to-first-byte
            let read_ahead_start = bytes_per_sec * tier.when_secs_until_uncached;
            let read_ahead_target = bytes_per_sec * tier.then_buffer_secs;
            (read_ahead_start, read_ahead_target)
        } else {
            // if we don't have a duration hint, we have to use the default values
            (READ_AHEAD_START_BYTES, READ_AHEAD_TARGET_BYTES)
        };

        let read_ahead_trigger_chunks = (read_ahead_trigger / DEFAULT_CHUNK_SIZE).max(1);
        let read_ahead_target_chunks = (read_ahead_target / DEFAULT_CHUNK_SIZE).max(2);
        tracing::trace!("read ahead trigger: {} chunks", read_ahead_trigger_chunks);
        tracing::trace!("read ahead target: {} chunks", read_ahead_target_chunks);

        // find the next uncached chunk (that is not being downloaded
        // and that is within the read ahead start chunks)
        // let mut first_uncached_chunk: Option<Arc<Chunk>> = None;
        let mut read_ahead_triggered = false;
        let trigger_start_idx = current_chunk_idx + 1;
        let trigger_end_idx = trigger_start_idx + read_ahead_trigger_chunks;
        tracing::trace!(
            "read ahead trigger range: {}-{}",
            trigger_start_idx,
            trigger_end_idx
        );
        for i in trigger_start_idx..trigger_end_idx {
            let Some(chunk) = self.chunks.get(i as usize) else {
                break;
            };

            if !chunk.is_cached_or_downloading() {
                read_ahead_triggered = true;
                break;
            }
        }

        if read_ahead_triggered {
            // there are chunks that are not cached/downloading within the read ahead trigger range
            let target_start_idx = trigger_start_idx;
            let target_end_idx = trigger_start_idx + read_ahead_target_chunks;
            let mut read_ahead_chunks = Vec::new();
            for i in target_start_idx..target_end_idx {
                let Some(chunk) = self.chunks.get(i as usize) else {
                    break;
                };

                if chunk.is_cached_or_downloading() {
                    continue;
                }

                read_ahead_chunks.push(chunk.clone());
            }

            Some(read_ahead_chunks)
        } else {
            None
        }
    }

    pub async fn read_bytes(self: &Arc<Self>, offset: u64, size: u64) -> Result<Vec<u8>> {
        let reader = self.readers.get_reader(offset, size);

        let start_chunk_index = offset / DEFAULT_CHUNK_SIZE;
        let end_chunk_index = (offset + size - 1) / DEFAULT_CHUNK_SIZE;

        let mut chunks_to_queue = self.chunks
            [start_chunk_index as usize..=end_chunk_index as usize]
            .iter()
            .map(|c| c.clone())
            .collect::<Vec<_>>();

        tracing::trace!("Current chunks: {}-{}", start_chunk_index, end_chunk_index);
        let config = get_config();
        let mut is_in_preload = false;
        if let Some(preload) = config.chunk_preload {
            // if the requested range is within the preload values,
            // add the preload chunks to the chunk list.
            let preload_end_index = self.chunks.len() as u64 - preload.1;
            if start_chunk_index <= preload.0 || end_chunk_index >= preload_end_index {
                let mut preload_chunks = Vec::new();
                for chunk in self.get_preload_chunks().unwrap() {
                    if chunk.index >= start_chunk_index && chunk.index <= end_chunk_index {
                        // don't queue the chunk if its already in the list
                        continue;
                    }

                    is_in_preload = true;
                    preload_chunks.push(chunk.index);
                    chunks_to_queue.push(chunk);
                }

                tracing::trace!("Added preload chunks: {:#?}", preload_chunks);
            }
        }

        // this ensures that when crossing from preload chunks to normal chunks,
        // we don't freeze the stream because the user passed from preload to uncached normal chunks.
        let force_read_ahead = self
            .chunks
            .get(end_chunk_index as usize + 1)
            .map(|c| !c.is_cached_or_downloading())
            .unwrap_or(false);

        // we only use read ahead if we aren't in the preload chunks, or else
        // a tool like ffprobe would trigger read ahead and download 10 chunks instead
        // of using the preload chunks like it should.
        if !is_in_preload || force_read_ahead {
            let read_ahead_chunks = self.get_read_ahead_chunks(end_chunk_index, reader.bytes_read);
            // tracing::trace!("Read ahead chunks: {:#?}", read_ahead_chunks);
            // tracing::trace!("Consecutive reads: {}", consecutive_reads);
            if let Some(read_ahead_chunks) = read_ahead_chunks {
                tracing::trace!(
                    "Adding read ahead chunks: {:#?}",
                    read_ahead_chunks
                        .iter()
                        .map(|c| c.index)
                        .collect::<Vec<_>>()
                );
                chunks_to_queue.extend(read_ahead_chunks);
                // chunks_to_queue.sort_by_key(|c| c.index);
            } else {
                tracing::trace!("No read ahead chunks calculated");
            }
        }

        self.queue_chunks(chunks_to_queue)?;

        // we might ensure multiple chunks (for read ahead/preload), but we only need
        // probably 1-2 chunks, so we can skip waiting for the rest.
        let necessary_chunks = &self.chunks[start_chunk_index as usize..=end_chunk_index as usize];
        self.wait_for_chunks(necessary_chunks).await?;

        // let mut data = Vec::with_capacity(size as usize);
        let cache_path = self.get_cache_path();
        let mut fd = tokio::fs::OpenOptions::new()
            .read(true)
            .write(false)
            .open(&cache_path)
            .await?;

        let mut buffer = vec![0; size as usize];
        fd.seek(SeekFrom::Start(offset)).await?;
        fd.read_exact(&mut buffer).await?;
        drop(fd);

        Ok(buffer)
    }

    async fn wait_for_chunks(self: &Arc<Self>, chunks: &[Arc<Chunk>]) -> Result<()> {
        for chunk in chunks {
            loop {
                // the lock wont be released once the chunk is finished downloading because of
                // how retries/ratelimit handling works, so we have to spin until the chunk is downloaded.
                // but, if the lock is released, we know the download is completed and if its still not cached, its failed.
                let maybe_lock = chunk.downloading.try_lock();
                let is_cached = chunk.cached.load(Ordering::SeqCst);
                if is_cached {
                    break;
                }

                if maybe_lock.is_ok() {
                    // if we can acquire the lock we know the download failed.
                    anyhow::bail!(
                        "Chunk {} is not cached and is not downloading. It might have failed to download.",
                        chunk.index
                    );
                }

                // if the chunk is not cached and is downloading, we wait for it to finish
                sleep(std::time::Duration::from_millis(20)).await;
            }
        }

        Ok(())
    }

    fn queue_chunks(self: &Arc<Self>, mut chunks: Vec<Arc<Chunk>>) -> Result<()> {
        // todo: this is inefficient, we should just assert that these are true and do it as
        // we build the chunk list, but for now this is fine.
        // sort the chunks by index
        chunks.sort_by_key(|c| c.index);
        // remove duplicates
        chunks.dedup();

        let mut current_batch: Vec<(OwnedMutexGuard<()>, Arc<Chunk>)> = Vec::new();
        for chunk in chunks {
            let is_cached = chunk.cached.load(Ordering::SeqCst);
            if is_cached {
                continue;
            }

            let download_lock = chunk.downloading.clone().try_lock_owned();
            let Ok(download_lock) = download_lock else {
                // chunk is already being downloaded by something else, we can skip it
                continue;
            };

            debug_assert!(chunk.cached.load(Ordering::SeqCst) == false);

            if let Some((_, last)) = current_batch.last() {
                if last.index != chunk.index - 1 {
                    // if the previous chunk is not contiguous, we have to spawn the batch
                    // and start a new one.
                    tracing::warn!(
                        "Chunk {} is not contiguous with previous chunk {}",
                        chunk.index,
                        last.index
                    );
                    self.pinch_chunk_batch(current_batch);
                    current_batch = Vec::new();
                }
            }

            current_batch.push((download_lock, chunk.clone()));
        }

        if !current_batch.is_empty() {
            self.pinch_chunk_batch(current_batch);
        }

        Ok(())
    }

    fn pinch_chunk_batch(self: &Arc<Self>, chunks: Vec<(OwnedMutexGuard<()>, Arc<Chunk>)>) {
        tokio::spawn({
            let file = self.clone();
            let ratelimiter = self.ratelimiter.clone();
            let debrid = self.debrid.clone();
            async move {
                let result = download_contiguous_chunks(chunks, file, ratelimiter, debrid).await;
                if let Err(e) = result {
                    tracing::error!("Failed to download chunks: {}", e);
                }
            }
        });
    }
}
