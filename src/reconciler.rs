use crate::config::get_config;
use crate::debrid::{Debrid, TorboxApiErrorType, TorboxError, TorboxTorrentFile};
use crate::helpers::should_ignore_path::should_ignore_path;
use crate::state::TorrentState;
use anyhow::Result;
use sqlx::{Sqlite, SqlitePool, Transaction};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time::sleep;

const RECHECK_INTERVAL_SECS: u64 = 10 * 60; // 10 minutes
const MIN_RECHECK_INTERVAL_SECS: u64 = 30; // 30 seconds

pub async fn start_reconciler(db: &SqlitePool, debrid: Arc<Debrid>, notifier: Arc<Notify>) -> Result<()> {
    // gives time for the reconciler to be blocked on startup, without taking
    // too much time for an initial sync
    sleep(Duration::from_secs(5)).await;

    let config = get_config();
    let mut download_limit: usize = 20;

    loop {
        tracing::debug!("Reconciling torrents");
        let mut remote_torrents: HashMap<Vec<u8>, _> = debrid
            .get_torrent_list(false)
            .await?
            .into_iter()
            .map(|t| (hex::decode(&t.hash).unwrap(), t))
            .collect();

        let active_count = remote_torrents.values().into_iter().filter(|t| t.active).count();

        // todo: this was using streaming, but it holds the db connection and because we have a single
        // connection, it means inner queries will block indefinitely.
        // let local_torrents = torrents::Entity::find().all(db).await?;
        let local_torrents = sqlx::query!(
            "SELECT id, hash, state as \"state: TorrentState\", error_message, hidden, magnet_uri, finished_at FROM torrents"
        )
        .fetch_all(db)
        .await?;

        for local_torrent in local_torrents {
            let torrent_hash = hex::encode(&local_torrent.hash);
            let live_ref_count = sqlx::query_scalar!(
                "SELECT COUNT(*) as count FROM nodes WHERE torrent_id = ? AND readonly = 0",
                local_torrent.id
            )
            .fetch_one(db)
            .await?;

            // files_created check is necessary or else when we add a torrent, we instantly remove it.
            // we have to wait until the files are created in the download dir.
            let initial_state = if local_torrent.hidden == 1 && live_ref_count == 0 {
                // remove torrents with no references, they are essentially dead.
                tracing::info!("marking unused torrent {} for removal", torrent_hash);
                TorrentState::Removing
            } else {
                local_torrent.state
            };

            let debrid_torrent = match remote_torrents.remove(&local_torrent.hash) {
                Some(torrent) => torrent,
                None => {
                    if initial_state == TorrentState::Removing {
                        // https://tenor.com/bYVT6.gif
                        tracing::warn!("removing torrent {}", torrent_hash);
                        sqlx::query!("DELETE FROM torrents WHERE id = ?", local_torrent.id)
                            .execute(db)
                            .await?;

                        continue;
                    }

                    if active_count >= download_limit {
                        tracing::debug!(
                            "torrent download limit {} hit, not adding torrent {}",
                            download_limit,
                            torrent_hash
                        );
                        continue;
                    }

                    // torrent does not exist on the debrid service, we need to add it
                    match debrid.create_from_magnet(&local_torrent.magnet_uri).await {
                        Err(TorboxError::ApiError(api_error)) => match api_error.data {
                            TorboxApiErrorType::ActiveLimit { active_limit } => {
                                tracing::warn!("ACTIVE_LIMIT error hit, limiting active torrents to {}", active_limit);
                                download_limit = active_limit as usize;
                                continue;
                            }
                            _ => {
                                tracing::error!("Failed to create torrent from magnet: {}", api_error);
                                continue;
                            }
                        },
                        Err(e) => {
                            tracing::error!("Failed to create torrent from magnet: {}", e);
                            continue;
                        }
                        Ok(created_torrent) => debrid.get_torrent_info(&created_torrent.torrent_id).await?,
                    }
                }
            };

            if initial_state == TorrentState::Removing {
                // remove the torrent from the debrid service
                tracing::info!("removing torrent {}", torrent_hash);
                debrid.delete_torrent(&debrid_torrent.id).await?;
                sqlx::query!("DELETE FROM torrents WHERE id = ?", local_torrent.id)
                    .execute(db)
                    .await?;

                continue;
            }

            let mut dir_name = None;
            if let Some(files) = debrid_torrent.files {
                dir_name = files
                    .first()
                    .and_then(|file| file.name.split_once('/'))
                    .map(|(dir, _)| dir.to_string());

                let filtered_files = files
                    .into_iter()
                    .filter(|file| !should_ignore_path(&file.name))
                    .collect::<Vec<_>>();

                if filtered_files.len() == 0 {
                    // todo: it would be nice if we could include more specific information in the error message
                    // this handles a few cases:
                    // - torbox has (had?) a bug where if a user requested a torernt be zipped, it was zipped for everyone
                    // (apparently not a bug, but a feature:tm:!), which caused it to be unstreamable. because we filter out
                    // zip files, those broken torrents will trigger this.
                    // - torrents that are intentionally malicious and that only contain EXEs or other silly things
                    tracing::error!("torrent {} has no valid files, marking as failed", torrent_hash);
                    let state = TorrentState::Error as i64;
                    let error_message = "Torrent has no valid files".to_string();
                    sqlx::query!(
                        "UPDATE torrents SET state = ?, error_message = ? WHERE id = ?",
                        state,
                        error_message,
                        local_torrent.id
                    )
                    .execute(db)
                    .await?;
                } else {
                    let mut tx = db.begin().await?;
                    for file in filtered_files.into_iter() {
                        if should_ignore_path(&file.name) {
                            tracing::warn!("ignoring file {} in torrent {}", file.name, torrent_hash);
                            continue;
                        }

                        let file_id = file.id as i64;
                        let file_size = file.size as i64;

                        let file_id = sqlx::query_scalar!(
                            "INSERT INTO torrent_files (torrent_id, path, debrid_id, size) VALUES (?, ?, ?, ?)
                            ON CONFLICT(torrent_id, path) DO UPDATE SET debrid_id = excluded.debrid_id, size = excluded.size
                            RETURNING id",
                            local_torrent.id,
                            file.name,
                            file_id,
                            file_size,
                        )
                        .fetch_one(tx.as_mut())
                        .await?;

                        create_nodes_for_file(&mut tx, local_torrent.id, file_id, &file).await?;
                    }

                    tx.commit().await?;
                }
            }

            let mut next_state = TorrentState::from_str(&debrid_torrent.download_state);
            if debrid_torrent.download_present && next_state == TorrentState::Downloading {
                // sometimes the download is present but the torrent state does not agree.
                // this seems okay and speeds up torrent availability.
                next_state = TorrentState::Ready;
            }

            // if next_state == TorrentState::Ready && initial_state != next_state {
            //     // if the torrent changes into a Ready state, we want to verify the torrent is ready.
            //     // torbox for some reason has a lot of "broken" torrents that give a database error
            //     // when you try and stream them. so whatever, this works for now.
            //     let can_download = debrid.get_download_link(debrid_torrent.id as i64, 0).await.ok();

            //     if can_download.is_none() {
            //         tracing::warn!(
            //             "Torrent {} has become Ready but its download link is broken. Marking it as failed",
            //             local_torrent.hash
            //         );
            //         let mut local_torrent = local_torrent.into_active_model();
            //         local_torrent.state = Set(TorrentState::Error);
            //         local_torrent.error_message = Set(Some(
            //             "Failed to create download link for torrent, its likely corrupted".to_string(),
            //         ));
            //         local_torrent.save(db).await?;
            //         continue;
            //     }
            // }

            if next_state != TorrentState::try_from(local_torrent.state)? {
                tracing::info!(
                    "torrent {} changed from {:?} to {:?}",
                    torrent_hash,
                    initial_state,
                    next_state
                );
            }

            let debrid_id = debrid_torrent.id as i64;
            let finished_at = if next_state == TorrentState::Ready {
                Some(
                    local_torrent
                        .finished_at
                        .unwrap_or_else(|| chrono::Utc::now().timestamp_millis()),
                )
            } else {
                None
            };

            let now = chrono::Utc::now().timestamp_millis();
            let eta = debrid_torrent.eta as i64;
            let size = debrid_torrent.size as i64;
            let seeds = debrid_torrent.seeds as i64;
            let peers = debrid_torrent.peers as i64;
            sqlx::query!(
                "UPDATE torrents SET
                    name = COALESCE(name, ?),
                    state = ?,
                    debrid_id = ?,
                    progress = ?,
                    upload_speed = ?,
                    download_speed = ?,
                    seeds = ?,
                    peers = ?,
                    ratio = ?,
                    eta_secs = ?,
                    size = ?,
                    checked_at = ?,
                    finished_at = ?
            ",
                dir_name,
                next_state,
                debrid_id,
                debrid_torrent.progress,
                debrid_torrent.upload_speed,
                debrid_torrent.download_speed,
                seeds,
                peers,
                debrid_torrent.ratio,
                eta,
                size,
                now,
                finished_at,
            )
            .execute(db)
            .await?;
        }

        if config.delete_unmapped && remote_torrents.len() > 0 {
            for (hash, torrent) in remote_torrents {
                let torrent_hash = hex::encode(hash);
                tracing::info!("deleting unmapped debrid torrent {}", torrent_hash);
                debrid.delete_torrent(&torrent.id).await?;
            }
        }

        let finished_at = Instant::now();
        tracing::debug!("finished reconciling torrents");
        tokio::select! {
            _ = sleep(Duration::from_secs(RECHECK_INTERVAL_SECS)) => {}
            _ = notifier.notified() => {
                let since_finished_secs = finished_at.elapsed().as_secs();
                if since_finished_secs < MIN_RECHECK_INTERVAL_SECS {
                    let wait_time = MIN_RECHECK_INTERVAL_SECS - since_finished_secs;
                    tracing::info!("reconciler notified early, waiting {} seconds", wait_time);
                    sleep(Duration::from_secs(wait_time)).await;
                } else {
                    tracing::info!("reconciler notified early, skipping wait");
                }
            }
        }
    }
}

async fn create_nodes_for_file(
    pool: &mut Transaction<'_, Sqlite>,
    torrent_id: i64,
    file_id: i64,
    file: &TorboxTorrentFile,
) -> Result<()> {
    let parts = file.name.split('/').collect::<Vec<&str>>();
    let parts_len = parts.len();
    let mut parent_id = 2; // downloads dir id
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts_len - 1;
        let name = <&str as ToString>::to_string(part);

        if is_last {
            let size = file.size as i64;
            sqlx::query!(
                "INSERT INTO nodes (parent_id, name, readonly, size, file_id, torrent_id) VALUES (?, ?, 1, ?, ?, ?)",
                parent_id,
                name,
                size,
                file_id,
                torrent_id,
            )
            .execute(pool.as_mut())
            .await?;
        } else {
            // we can't do nothing or exec with returning won't work (RETURNING only
            // works if a column is updated or inserted)
            parent_id = sqlx::query_scalar!(
                "INSERT INTO nodes (parent_id, name, readonly) VALUES (?, ?, 1)
                ON CONFLICT (parent_id, name) DO UPDATE SET size = excluded.size
                RETURNING id",
                parent_id,
                name,
            )
            .fetch_one(pool.as_mut())
            .await?;
        }
    }

    return Ok(());
}
