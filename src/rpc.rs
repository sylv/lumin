use crate::{
    AppState,
    entities::{
        torrent_files,
        torrents::{self, TorrentState},
    },
};
use axum::{Router, extract::State};
use juno::{
    errors::{RpcError, RpcStatus},
    router::RpcRouter,
    rpc,
};
use sea_orm::{Set, prelude::*};
use std::sync::Arc;

#[rpc(query)]
async fn get_torrents(
    State(state): State<Arc<AppState>>,
) -> Result<Vec<torrents::Model>, RpcError> {
    let db = state.db.clone();
    let torrents = torrents::Entity::find()
        .all(&db)
        .await
        .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

    Ok(torrents)
}

#[rpc(query)]
async fn get_torrent_files(
    State(state): State<Arc<AppState>>,
    torrent_id: i32,
) -> Result<Vec<torrent_files::Model>, RpcError> {
    let db = state.db.clone();
    let files = torrent_files::Entity::find()
        .filter(torrent_files::Column::TorrentId.eq(torrent_id))
        .all(&db)
        .await
        .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

    // todo: include cache stats (chunk list (downloading, downloaded))
    // todo: include reader stats (read bytes, position, chunks read)
    Ok(files)
}

#[rpc(query)]
async fn delete_torrent(
    State(state): State<Arc<AppState>>,
    torrent_id: i64,
) -> Result<(), RpcError> {
    let db = state.db.clone();
    torrents::Entity::update(torrents::ActiveModel {
        id: Set(torrent_id),
        state: Set(TorrentState::Removing),
        ..Default::default()
    })
    .exec(&db)
    .await
    .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

    Ok(())
}

pub fn get_rpc_router() -> Router<Arc<AppState>> {
    RpcRouter::new()
        .for_state::<Arc<AppState>>()
        .add(get_torrents)
        .add(get_torrent_files)
        .add(delete_torrent)
        .write_client("client/src/@generated/server.ts")
        .unwrap()
        .to_router()
}
