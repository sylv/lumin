use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Clone, Eq, Copy, Serialize, Deserialize, sqlx::Type)]
#[repr(i32)]
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
