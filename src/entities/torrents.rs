use crate::{config::get_config, qbittorrent::QBittorrentTorrent};
use sea_orm::entity::prelude::*;
use serde::Serialize;
use specta::Type;

#[derive(Clone, Debug, Serialize, Type, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "torrents")]
#[specta(rename = "Torrent")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(column_type = "Text")]
    pub hash: String,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    #[sea_orm(unique)]
    pub remote_id: Option<i64>,
    #[sea_orm(column_type = "Text")]
    pub magnet_uri: String,
    pub state: TorrentState,
    pub progress: f32,
    pub seeds: u32,
    pub peers: u32,
    pub upload_speed: u32,
    pub download_speed: u32,
    pub ratio: f32,
    pub error_message: Option<String>,
    pub eta_secs: i64,
    pub size: i64,
    pub category: Option<String>,
    pub files_created: bool,
    pub created_at: i64,
    pub updated_at: i64,
    pub checked_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::nodes::Entity")]
    Nodes,
    #[sea_orm(has_many = "super::torrent_files::Entity")]
    TorrentFiles,
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Nodes.def()
    }
}

impl Related<super::torrent_files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TorrentFiles.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub fn to_qbittorrent(self) -> QBittorrentTorrent {
        let config = get_config();
        let save_path = config
            .mount_path
            .join("downloads")
            .to_string_lossy()
            .into_owned();

        QBittorrentTorrent {
            hash: self.hash,
            name: self.name,
            size: self.size,
            progress: self.progress,
            eta_secs: self.eta_secs as u32,
            state: self.state.to_str().to_string(),
            category: self.category.clone(),
            save_path: Some(save_path),
            ratio: self.ratio,
            ratio_limit: None,
            seeding_time: None,
            seeding_time_limit: None,
            inactive_seeding_time_limit: None,
            last_activity: self.updated_at as u64,
        }
    }
}

#[derive(Debug, PartialEq, Clone, Eq, Serialize, Type, Copy, EnumIter, DeriveActiveEnum)]
#[sea_orm(rs_type = "i64", db_type = "Integer")]
pub enum TorrentState {
    Pending = 0,
    Downloading = 1,
    Ready = 2,
    Stalled = 3,
    Error = 4,
    Removing = 5,
}

impl TorrentState {
    pub fn from_str(state: &str) -> Self {
        match state {
            "queued" => TorrentState::Pending,
            "metaDL" => TorrentState::Pending,
            "checking" => TorrentState::Pending,
            "checkingResumeData" => TorrentState::Pending,
            "paused" => TorrentState::Pending,
            "downloading" => TorrentState::Downloading,
            "completed" => TorrentState::Downloading,
            "stalledDL" => TorrentState::Stalled,
            "stalled" => TorrentState::Stalled,
            "stalled (no seeds)" => TorrentState::Stalled,
            "processing" => TorrentState::Stalled,
            "uploading" => TorrentState::Ready,
            "uploading (no peers)" => TorrentState::Ready,
            "cached" => TorrentState::Ready,
            _ => TorrentState::Error,
        }
    }

    pub fn to_str(&self) -> &'static str {
        // maps to a valid qbittorrent state
        match self {
            TorrentState::Pending => "queuedDL",
            TorrentState::Downloading => "downloading",
            TorrentState::Ready => "stalledUP",
            TorrentState::Stalled => "stalledDL",
            TorrentState::Error => "error",
            TorrentState::Removing => "unknown",
        }
    }
}
