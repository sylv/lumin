use crate::{config::get_config, helpers::get_user_agent::get_user_agent};
use anyhow::Result;
use chrono::{DateTime, Utc};
use ratelimit::Ratelimiter;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use tracing::info;

const BASE_URL: &str = "https://api.torbox.app/v1/api";

#[derive(Debug, Deserialize)]
pub struct TorboxApiError {
    pub error: Option<String>,
    pub detail: String,
    pub data: serde_json::Value,
}
impl std::fmt::Display for TorboxApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TorboxError({}): {}",
            self.error.as_deref().unwrap_or("UNKNOWN"),
            self.detail
        )
    }
}

impl std::error::Error for TorboxApiError {}

#[derive(Debug, thiserror::Error)]
pub enum TorboxError {
    #[error("Torbox API error: {0}")]
    ApiError(#[from] TorboxApiError),

    #[error("Request error: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
#[allow(dead_code)]
enum TorboxResponse<T> {
    Result { data: T },
    Error(TorboxApiError),
}

#[derive(Debug, Deserialize)]
pub struct TorboxCreateTorrentData {
    pub torrent_id: u32,
}

#[derive(Debug, Deserialize)]
pub struct TorboxListTorrent {
    pub id: u64,
    pub hash: String,
    pub seeds: u32,
    pub peers: u32,
    pub name: String,
    pub ratio: f32,
    pub progress: f32,
    pub download_speed: u32,
    pub active: bool,
    pub eta: u32,
    pub size: u64,
    pub upload_speed: u32,
    pub download_state: String,
    pub download_present: bool,
    pub files: Option<Vec<TorboxTorrentFile>>,
}

#[derive(Debug, Deserialize)]
pub struct TorboxTorrentFile {
    pub id: u64,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Deserialize)]
pub struct TorboxInstantAvailability(pub HashMap<String, TorboxInstantAvailabilityData>);

#[derive(Debug, Deserialize)]
pub struct TorboxInstantAvailabilityData {
    pub name: String,
    pub size: u64,
    pub hash: String,
}

// todo: this is kinda bad, should be handled by lru for higher hit rates,
// but for now its fine w/e.
pub struct ExpiringItem<T> {
    item: T,
    expires_at: DateTime<Utc>,
}

impl<T> ExpiringItem<T> {
    pub fn new(item: T, expires_at: DateTime<Utc>) -> Self {
        ExpiringItem { item, expires_at }
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at < Utc::now()
    }
}

impl<T> From<T> for ExpiringItem<T> {
    fn from(item: T) -> Self {
        ExpiringItem {
            item,
            expires_at: Utc::now() + chrono::Duration::minutes(60),
        }
    }
}

pub struct Debrid {
    client: reqwest::Client,
    token: String,
    limiter: Ratelimiter,
    url_cache: Mutex<HashMap<String, ExpiringItem<String>>>,
    url_mutex: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl Debrid {
    pub fn new() -> Self {
        let config = get_config();
        let limiter = Ratelimiter::builder(10, Duration::from_secs(30))
            .max_tokens(10)
            .initial_available(4)
            .build()
            .expect("Failed to create rate limiter");

        Debrid {
            client: reqwest::Client::new(),
            token: config.torbox_key.clone(),
            url_cache: Mutex::new(HashMap::new()),
            url_mutex: Mutex::new(HashMap::new()),
            limiter,
        }
    }

    pub async fn create_from_magnet(
        &self,
        magnet_uri: &str,
    ) -> Result<TorboxCreateTorrentData, TorboxError> {
        info!("Creating torrent from magnet: {}", magnet_uri);
        let url = format!("{}/torrents/createtorrent", BASE_URL);
        let body = json!({ "magnet": magnet_uri, "allow_zip": false });
        self.wait().await;
        let response = self
            .add_headers(self.client.post(url), true)
            .form(&body)
            .send()
            .await?
            .json()
            .await?;

        Ok(self.parse_response::<TorboxCreateTorrentData>(response)?)
    }

    pub async fn delete_torrent(&self, torrent_id: &u64) -> Result<(), TorboxError> {
        info!("Deleting torrent: {}", torrent_id);
        let url = format!("{}/torrents/controltorrent", BASE_URL);
        self.wait().await;
        let response = self
            .add_headers(self.client.post(url), true)
            .json(&json!({
                "torrent_id": torrent_id,
                "operation": "delete"
            }))
            .send()
            .await?
            .json()
            .await?;

        self.parse_response::<serde_json::Value>(response)?;
        Ok(())
    }

    pub async fn get_torrent_info(
        &self,
        torrent_id: &u32,
    ) -> Result<TorboxListTorrent, TorboxError> {
        let url = format!(
            "{}/torrents/mylist?bypass_cache=true&id={}",
            BASE_URL, torrent_id
        );

        self.wait().await;
        let response = self
            .add_headers(self.client.get(url), true)
            .send()
            .await?
            .json()
            .await?;

        Ok(self.parse_response::<TorboxListTorrent>(response)?)
    }

    pub async fn get_torrent_list(
        &self,
        use_cache: bool,
    ) -> Result<Vec<TorboxListTorrent>, TorboxError> {
        let url = format!("{}/torrents/mylist?bypass_cache={}", BASE_URL, !use_cache);
        self.wait().await;
        let response = self
            .add_headers(self.client.get(&url), true)
            .send()
            .await?
            .json()
            .await?;

        Ok(self.parse_response::<Vec<TorboxListTorrent>>(response)?)
    }

    pub async fn check_cached(
        &self,
        hashes: &[String],
    ) -> Result<TorboxInstantAvailability, TorboxError> {
        let hash_batches = hashes
            .chunks(100)
            .map(|chunk| chunk.join(","))
            .collect::<Vec<_>>()
            .join("&hash=");

        let url = format!(
            "{}/torrents/checkcached?format=object&hash={}",
            BASE_URL, hash_batches
        );

        self.wait().await;
        let response = self
            .add_headers(self.client.get(&url), true)
            .send()
            .await?
            .json()
            .await?;

        Ok(self.parse_response::<TorboxInstantAvailability>(response)?)
    }

    pub async fn get_download_link(
        &self,
        torrent_id: i64,
        file_id: i64,
    ) -> Result<String, TorboxError> {
        let file_key = format!("{}:{}", torrent_id, file_id);
        let cached_url = self.get_cached_url(&file_key).await?;
        if let Some(cached_url) = cached_url {
            // if we can use a cached url without locking, that's great.
            return Ok(cached_url);
        }

        let mutex = {
            let mut url_mutexes = self.url_mutex.lock().await;
            url_mutexes
                .entry(file_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };

        let _lock = mutex.lock().await;

        // check again if the url is cached, it might have been requested
        // while we waited for the lock.
        let cached_url = self.get_cached_url(&file_key).await?;
        if let Some(cached_url) = cached_url {
            return Ok(cached_url);
        }

        let url = format!(
            "{}/torrents/requestdl?torrent_id={}&file_id={}&token={}",
            BASE_URL, torrent_id, file_id, self.token
        );

        info!(
            "Requesting download link for file: {} via {}",
            file_key, url
        );
        self.wait().await;
        let start = Utc::now();
        let response = self
            .add_headers(self.client.get(url), false)
            .send()
            .await?
            .json()
            .await?;

        let data: String = self.parse_response(response)?;
        self.set_cached_url(&file_key, &data, start + chrono::Duration::minutes(175))
            .await?;

        Ok(data)
    }

    async fn set_cached_url(
        &self,
        file_hash: &str,
        url: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        let cached_url = ExpiringItem::new(url.to_string(), expires_at);
        self.url_cache
            .lock()
            .await
            .insert(file_hash.to_string(), cached_url);

        Ok(())
    }

    async fn get_cached_url(&self, file_id: &str) -> Result<Option<String>> {
        let mut url_cache = self.url_cache.lock().await;
        match url_cache.get(file_id) {
            Some(cached_url) => {
                if cached_url.is_expired() {
                    url_cache.remove(file_id);
                    return Ok(None);
                }

                return Ok(Some(cached_url.item.to_string()));
            }
            None => Ok(None),
        }
    }

    fn add_headers(&self, builder: reqwest::RequestBuilder, auth: bool) -> reqwest::RequestBuilder {
        let builder = if auth {
            builder.header("Authorization", format!("Bearer {}", self.token))
        } else {
            builder
        };

        builder
            .header("Accept", "application/json")
            .header("User-Agent", get_user_agent())
    }

    fn parse_response<T: DeserializeOwned>(
        &self,
        mut response: serde_json::Value,
    ) -> Result<T, TorboxApiError> {
        // doing it like this avoids issues with serdes untagged enums,
        // mostly that if the response is a success but deserialization fails,
        // it skips the error and tries to deserialize as an error which then
        // either succeeds or causes an obscure error about no variant matching.
        let is_success = response["success"].as_bool().unwrap_or(false);
        if is_success {
            let data = response["data"].take();
            let data_clone = data.clone();
            let result: T = serde_json::from_value(data).map_err(|e| TorboxApiError {
                error: Some("Deserialization error".to_string()),
                detail: e.to_string(),
                data: data_clone,
            })?;

            Ok(result)
        } else {
            let error = serde_json::from_value::<TorboxApiError>(response.clone()).unwrap();
            Err(error)
        }
    }

    async fn wait(&self) {
        loop {
            if let Err(sleep) = self.limiter.try_wait() {
                std::thread::sleep(sleep);
                continue;
            }

            break;
        }
    }
}
