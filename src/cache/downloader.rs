use super::{chunk::Chunk, entry::CacheEntry, ratelimiter::Ratelimiter};
use crate::{
    config::get_config,
    debrid::{Debrid, TorboxError},
    helpers::get_user_agent::get_user_agent,
};
use anyhow::Result;
use futures_util::StreamExt;
use reqwest::StatusCode;
use std::{
    io::SeekFrom,
    sync::{Arc, atomic::Ordering},
};
use thiserror::Error;
use tokio::{
    io::{AsyncSeekExt, AsyncWriteExt},
    sync::OwnedMutexGuard,
};

const RESPONSE_ERROR_RETRIES: [u64; 3] = [1, 5, 30]; // errors that are returned by the server, like 500
const STREAM_ERROR_RETRIES: [u64; 2] = [5, 30]; // errors that happen while streaming the response chunks
const FETCH_ERROR_RETRIES: [u64; 1] = [5]; // errors that happen while sending the request

#[derive(Debug, Error)]
enum DownloadChunkError {
    #[error("ratelimited while trying to download chunks")]
    Ratelimited(Option<u64>), // inner is server-requested backoff in seconds, or None if not specified
    #[error("failed to start download: {0}")]
    FetchError(#[from] reqwest::Error), // retryable
    #[error("invalid status from server: {0}")]
    ResponseError(StatusCode, bool), // retryable if `bool` is true
    #[error("error while streaming chunks: {0}")]
    StreamError(reqwest::Error), // retryable
    #[error("failed to open file for writing: {0}")]
    IoError(#[from] std::io::Error), // never retried
    #[error("torbox api error: {0}")]
    TorboxError(#[from] TorboxError),
    #[error("unrecoverable error while downloading chunks: {0}")]
    GenericError(#[from] anyhow::Error), // never retried
}

impl DownloadChunkError {
    fn get_backoff(&self, attempts: usize, ratelimiter: &Arc<Ratelimiter>) -> Option<u64> {
        match self {
            DownloadChunkError::Ratelimited(Some(seconds)) => {
                ratelimiter.set_ratelimited_for(5);
                Some(*seconds)
            }
            DownloadChunkError::Ratelimited(None) => {
                ratelimiter.set_ratelimited_for(5);
                RESPONSE_ERROR_RETRIES.get(attempts - 1).copied()
            }

            DownloadChunkError::FetchError(_) => FETCH_ERROR_RETRIES.get(attempts - 1).copied(),
            DownloadChunkError::ResponseError(_, retryable) => {
                if *retryable {
                    RESPONSE_ERROR_RETRIES.get(attempts - 1).copied()
                } else {
                    None
                }
            }
            DownloadChunkError::StreamError(_) => STREAM_ERROR_RETRIES.get(attempts - 1).copied(),
            DownloadChunkError::GenericError(_)
            | DownloadChunkError::IoError(_)
            | DownloadChunkError::TorboxError(_) => None,
        }
    }
}

pub async fn download_contiguous_chunks(
    chunks: Vec<(OwnedMutexGuard<()>, Arc<Chunk>)>,
    file: Arc<CacheEntry>,
    ratelimiter: Arc<Ratelimiter>,
    debrid: Arc<Debrid>,
) -> Result<()> {
    assert!(chunks.len() > 0);
    debug_assert!(
        chunks
            .windows(2)
            .all(|window| window[1].1.index == window[0].1.index + 1),
        "Chunks must be contiguous"
    );

    let mut attempts = 0;
    loop {
        attempts += 1;
        let result = download_contiguous_chunks_inner(&chunks, &file, &ratelimiter, &debrid).await;
        match result {
            Ok(_) => {
                drop(chunks);
                return Ok(());
            }
            Err(e) => {
                if let Some(backoff) = e.get_backoff(attempts, &ratelimiter) {
                    tracing::warn!(
                        "Error downloading chunks, retrying in {} seconds (attempt {}): {}",
                        backoff,
                        attempts,
                        e
                    );
                    tokio::time::sleep(tokio::time::Duration::from_secs(backoff)).await;
                } else {
                    drop(chunks);
                    tracing::error!("Unrecoverable error downloading chunks: {}", e);
                    return Err(e.into());
                }
            }
        }
    }
}

async fn download_contiguous_chunks_inner(
    chunks: &Vec<(OwnedMutexGuard<()>, Arc<Chunk>)>,
    entry: &Arc<CacheEntry>,
    ratelimiter: &Arc<Ratelimiter>,
    debrid: &Arc<Debrid>,
) -> Result<(), DownloadChunkError> {
    // Get the first and last chunk to determine the entire range
    let first_chunk = &chunks.first().unwrap().1;
    let last_chunk = &chunks.last().unwrap().1;
    let start_offset = first_chunk.offset;
    let end_offset = last_chunk.offset + last_chunk.size - 1;

    let config = get_config();
    let file = entry.get_file();
    let (url, auth) = match (&config.torbox_username, &config.torbox_password) {
        (Some(username), Some(password)) => {
            // with a username nad password, we can use webdav instead which avoids us having to get download links
            // webdav urls should just be https://webdav.torbox.app/{file_path} with url encoding but `/` not encoded
            // and the username+pass for basic auth
            let path = file
                .path
                .split('/')
                .map(|s| urlencoding::encode(s).to_string())
                .collect::<Vec<_>>()
                .join("/");

            let url = format!("https://webdav.torbox.app/{}", path);
            (url, Some((username, password)))
        }
        (None, None) => {
            // todo: this should really be a FetchError, but the inner type is not compatible with
            // anyhow and making it return a "real" type is borderline impossible. :(
            let url = debrid
                .get_download_link(file.torrent_debrid_id, file.file_debrid_id)
                .await?;

            (url, None)
        }
        _ => {
            return Err(DownloadChunkError::GenericError(anyhow::anyhow!(
                "Torbox credentials are not set correctly, username and password must be set or both unset"
            )));
        }
    };

    let range = format!("bytes={}-{}", start_offset, end_offset);
    tracing::info!(
        "Downloading chunks {}-{} from {} (range: {})",
        first_chunk.index,
        last_chunk.index,
        url,
        range
    );

    let client = reqwest::Client::new();
    let mut builder = client
        .get(&url)
        .header("Range", range)
        .header("User-Agent", get_user_agent());

    if let Some(auth) = auth {
        let (username, password) = auth;
        builder = builder.basic_auth(username, Some(password));
    };

    let permit = ratelimiter.wait().await;
    let response = builder.send().await.map_err(DownloadChunkError::FetchError)?;

    match response.status() {
        StatusCode::PARTIAL_CONTENT => {}
        StatusCode::TOO_MANY_REQUESTS => {
            let headers = response.headers();
            let retry_after: Option<u64> = headers
                .get("x-ratelimit-after")
                .or_else(|| headers.get("Retry-After"))
                .and_then(|header| header.to_str().ok())
                .and_then(|retry_after_str| retry_after_str.parse::<u64>().ok());

            return Err(DownloadChunkError::Ratelimited(retry_after));
        }
        // todo: INTERNAL_SERVER_ERROR is not included because usually those aren't retryable,
        // but this needs some more testing before we know for sure.
        StatusCode::REQUEST_TIMEOUT
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => {
            return Err(DownloadChunkError::ResponseError(
                response.status(),
                true, // retryable
            ));
        }
        _ => {
            tracing::warn!("unexpected status code from server: {}", response.status());
            return Err(DownloadChunkError::ResponseError(
                response.status(),
                false, // not retryable, we don't know wtf it is
            ));
        }
    }

    // assert!(response.status() == StatusCode::PARTIAL_CONTENT);
    let content_length = response
        .headers()
        .get("Content-Length")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.parse::<u64>().ok())
        .unwrap_or(0);

    let expected_length = end_offset - start_offset + 1;
    assert!(content_length == expected_length);

    let mut fd = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(entry.get_cache_path())
        .await?;

    fd.seek(SeekFrom::Start(start_offset)).await?;

    let mut response_stream = response.bytes_stream();
    let mut bytes_written = 0u64;
    let mut current_chunk_index = 0;

    // Track which chunk we're currently writing to
    let mut current_chunk_end_offset = chunks[current_chunk_index].1.offset + chunks[current_chunk_index].1.size;

    while let Some(block) = response_stream.next().await {
        let block = block.map_err(DownloadChunkError::StreamError)?;

        fd.write_all(&block).await?;
        bytes_written += block.len() as u64;

        // Check if we've completed writing the current chunk
        let current_offset = start_offset + bytes_written;

        // Mark chunks as cached as soon as they're fully downloaded
        while current_offset >= current_chunk_end_offset && current_chunk_index < chunks.len() {
            // Mark this chunk as cached
            chunks[current_chunk_index].1.cached.store(true, Ordering::SeqCst);

            entry
                .flush_cache_meta()
                .map_err(|e| {
                    tracing::error!("Failed to flush metadata: {}, this will cause cache issues", e);
                })
                .ok();

            tracing::info!(
                "Chunk {} downloaded and marked as cached",
                chunks[current_chunk_index].1.index
            );

            // Move to the next chunk
            current_chunk_index += 1;
            if current_chunk_index < chunks.len() {
                current_chunk_end_offset = chunks[current_chunk_index].1.offset + chunks[current_chunk_index].1.size;
            }
        }
    }

    drop(permit);
    fd.flush().await?;
    fd.sync_all().await?;
    drop(fd);

    // Make sure all chunks were marked as cached
    #[cfg(debug_assertions)]
    for (_, chunk) in chunks {
        assert!(chunk.cached.load(Ordering::SeqCst));
    }

    Ok(())
}
