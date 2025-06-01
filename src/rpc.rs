use crate::{
    AppState,
    entities::{
        torrent_files,
        torrents::{self, TorrentState},
    },
    helpers::{
        add_trackers_to_magnet_uri::add_trackers_to_magnet_uri, parse_magnet_uri::parse_magnet_uri,
    },
};
use axum::{Router, extract::State};
use juno::{
    errors::{RpcError, RpcStatus},
    router::RpcRouter,
    rpc,
};
use sea_orm::{Set, TransactionTrait, prelude::*};
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

#[rpc(mutation)]
async fn add_torrent(
    State(state): State<Arc<AppState>>,
    magnet_uris: Vec<String>,
    category: Option<String>,
) -> Result<(), RpcError> {
    let tx = state
        .db
        .begin()
        .await
        .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

    for magnet_uri in magnet_uris {
        let magnet_uri = add_trackers_to_magnet_uri(&magnet_uri);
        let Some(meta) = parse_magnet_uri(&magnet_uri) else {
            return Err(RpcError::new(
                RpcStatus::BadRequest,
                "Invalid magnet URI".to_string(),
            ));
        };

        let existing = torrents::Entity::find()
            .filter(torrents::Column::Hash.eq(&meta.hash))
            .one(&tx)
            .await
            .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

        if let Some(existing) = existing {
            // todo: ensure the files still exist on disk under the downloads dir
            if existing.category != category {
                // if the torrent already exists but has a different category, update it
                torrents::Entity::update(torrents::ActiveModel {
                    id: Set(existing.id),
                    category: Set(category),
                    ..Default::default()
                })
                .exec(&tx)
                .await
                .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;
            }

            existing
                .add_to_downloads_folder(&tx)
                .await
                .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

            return Ok(());
        }

        torrents::Entity::insert(torrents::ActiveModel {
            hash: Set(meta.hash.clone()),
            name: Set(meta.name.unwrap_or(meta.hash)),
            category: Set(category.clone()),
            state: Set(TorrentState::Pending),
            magnet_uri: Set(magnet_uri),
            ..Default::default()
        })
        .exec(&tx)
        .await
        .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;
    }

    tx.commit()
        .await
        .map_err(|e| RpcError::new(RpcStatus::InternalServerError, e.to_string()))?;

    state.notifier.notify_one();
    Ok(())
}

#[rpc(mutation)]
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

#[rpc(mutation)]
async fn reconcile_torrents(State(state): State<Arc<AppState>>) -> () {
    state.notifier.notify_one();
}

pub fn get_rpc_router() -> Router<Arc<AppState>> {
    RpcRouter::new()
        .for_state::<Arc<AppState>>()
        .add(get_torrents)
        .add(get_torrent_files)
        .add(delete_torrent)
        .add(add_torrent)
        .add(reconcile_torrents)
        .write_client("client/src/@generated/server.ts")
        .unwrap()
        .to_router()
}
