use crate::config::get_config;
use crate::state::TorrentState;
use serde::Serialize;
use sqlx::{FromRow, SqlitePool};

#[derive(FromRow, Debug)]
pub struct Torrent {
    pub id: i64,
    pub hash: Vec<u8>,
    pub name: String,
    pub state: TorrentState,
    pub magnet_uri: String,
    pub progress: f64,
    pub upload_speed: i64,
    pub download_speed: i64,
    pub ratio: f64,
    pub eta_secs: i64,
    pub seeds: i64,
    pub peers: i64,
    pub size: i64,
    pub hidden: i64,
    pub category: Option<String>,
    pub created_at: i64,
    pub checked_at: Option<i64>,
    pub finished_at: Option<i64>,
}

impl Torrent {
    pub fn to_qbittorrent(&self) -> QBittorrentTorrent {
        let save_path = get_config().mount_path.join("downloads");
        QBittorrentTorrent {
            hash: hex::encode(&self.hash),
            name: self.name.clone(),
            size: self.size,
            progress: self.progress,
            eta_secs: self.eta_secs as u32,
            state: self.state.to_str().to_string(),
            category: self.category.clone(),
            save_path: Some(save_path.to_string_lossy().into_owned()),
            ratio: self.ratio,
            ratio_limit: None,
            seeding_time: None,
            seeding_time_limit: None,
            inactive_seeding_time_limit: None,
            last_activity: self.checked_at.unwrap_or(self.created_at) as u64,
        }
    }

    pub async fn find_by_hash(hash: &str, pool: &SqlitePool) -> Result<Option<Torrent>, sqlx::Error> {
        sqlx::query_as!(
            Torrent,
            r#"SELECT
                id,
                hash,
                name,
                state as "state: TorrentState",
                magnet_uri,
                progress,
                upload_speed,
                download_speed,
                ratio,
                eta_secs,
                seeds,
                peers,
                size,
                hidden,
                category,
                created_at,
                checked_at,
                finished_at
            FROM torrents
            WHERE hash = ? AND hidden = 0"#,
            hash
        )
        .fetch_optional(pool)
        .await
    }
}

#[derive(Debug, Serialize)]
pub struct QBittorrentTorrent {
    pub hash: String,
    pub name: String,
    pub size: i64,
    pub progress: f64, // 0.0-1.0
    #[serde(rename = "eta")]
    pub eta_secs: u32,
    pub state: String,
    pub category: Option<String>,
    pub save_path: Option<String>,
    pub ratio: f64,
    pub ratio_limit: Option<f64>,
    pub seeding_time: Option<u32>,
    pub seeding_time_limit: Option<u32>,
    pub inactive_seeding_time_limit: Option<u32>,
    pub last_activity: u64,
}
