use crate::config::get_config;
use crate::debrid::Debrid;
use crate::entities::torrents::TorrentState;
use crate::entities::{nodes, torrent_files, torrents};
use crate::helpers::should_ignore_path::should_ignore_path;
use anyhow::Result;
use sea_orm::sea_query::OnConflict;
use sea_orm::{DatabaseConnection, IntoActiveModel, TransactionTrait};
use sea_orm::{Set, prelude::*};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::time::sleep;

const RECHECK_INTERVAL_SECS: u64 = 10 * 60; // 10 minutes
const MIN_RECHECK_INTERVAL_SECS: u64 = 30; // 30 seconds
const UNSAFE_EXTS: [&str; 7] = [".exe", ".msi", ".bat", ".lnk", ".cmd", ".sh", ".ps1"];

pub async fn start_reconciler(
    db: &DatabaseConnection,
    debrid: Arc<Debrid>,
    notifier: Arc<Notify>,
    is_scanning: Arc<AtomicBool>,
) -> Result<()> {
    // gives time for the reconciler to be blocked on startup, without taking
    // too much time for an initial sync
    sleep(Duration::from_secs(5)).await;

    let mut finished_at = Instant::now();
    loop {
        if !is_scanning.load(Ordering::SeqCst) {
            tracing::debug!("Reconciling torrents");
            let mut remote_torrents: HashMap<String, _> = debrid
                .get_torrent_list(false)
                .await?
                .into_iter()
                .map(|t| (t.hash.to_lowercase(), t))
                .collect();

            // todo: this was using streaming, but it holds the db connection and because we have a single
            // connection, it means inner queries will block indefinitely.
            let local_torrents = torrents::Entity::find().all(db).await?;
            for local_torrent in local_torrents {
                let total_ref_count = nodes::Entity::find()
                    .filter(nodes::Column::TorrentId.eq(local_torrent.id.clone()))
                    .count(db)
                    .await?;

                let mutable_ref_count = nodes::Entity::find()
                    .filter(nodes::Column::TorrentId.eq(local_torrent.id.clone()))
                    .filter(nodes::Column::Immutable.eq(false))
                    .count(db)
                    .await?;

                // files_created check is necessary or else when we add a torrent, we instantly remove it.
                // we have to wait until the files are created in the download dir.
                let initial_state = if local_torrent.files_created && total_ref_count == 0 {
                    // remove torrents with no references, they are essentially dead.
                    tracing::info!(
                        "Torrent {} has no references, marking for removal",
                        local_torrent.hash
                    );
                    TorrentState::Removing
                } else {
                    local_torrent.state
                };

                let debrid_torrent = match remote_torrents.remove(&local_torrent.hash) {
                    Some(torrent) => torrent,
                    None => {
                        if initial_state == TorrentState::Removing
                            || (local_torrent.files_created && mutable_ref_count == 0)
                        {
                            // https://tenor.com/bYVT6.gif
                            tracing::warn!(
                                "Removing torrent {} as it was removed from debrid and isn't important",
                                local_torrent.hash
                            );
                            torrents::Entity::delete_by_id(local_torrent.id)
                                .exec(db)
                                .await?;

                            continue;
                        }

                        // torrent does not exist on the debrid service, we need to add it
                        let created_torrent =
                            debrid.create_from_magnet(&local_torrent.magnet_uri).await?;

                        debrid.get_torrent_info(&created_torrent.torrent_id).await?
                    }
                };

                if initial_state == TorrentState::Removing {
                    // remove the torrent from the debrid service
                    debrid.delete_torrent(&debrid_torrent.id).await?;
                    tracing::info!("Removing torrent {}", local_torrent.hash);
                    torrents::Entity::delete_by_id(local_torrent.id)
                        .exec(db)
                        .await?;

                    continue;
                }

                let mut files_created = false;
                let mut dir_name = None;
                if let Some(files) = debrid_torrent.files {
                    if files.len() == 0 {
                        tracing::warn!(
                            "Torrent {} has no files, this will cause issues",
                            local_torrent.hash
                        );
                    }

                    dir_name = files
                        .first()
                        .and_then(|file| file.name.split_once('/'))
                        .map(|(dir, _)| dir.to_string());

                    if !local_torrent.files_created && files.len() > 0 {
                        if files.len() == 1 && files[0].name.ends_with(".zip") {
                            // todo: if a user adds a torrent with allow_zip: true and it is zipped, the torrent is zipped for
                            // everyone that downloads it in the future. we can't stream data from a zip, so we have to block
                            // these torrents. there should be a better way to handle this.
                            tracing::warn!(
                                "Torrent {} was zipped and cannot be streamed, marking it as failed",
                                local_torrent.hash
                            );
                            let mut local_torrent = local_torrent.into_active_model();
                            local_torrent.state = Set(TorrentState::Error);
                            local_torrent.error_message =
                                Set(Some("Zipped torrent cannot be streamed".to_string()));
                            local_torrent.save(db).await?;
                            continue;
                        }

                        let all_unsafe = files.iter().all(|file| {
                            UNSAFE_EXTS
                                .iter()
                                .any(|ext| file.name.to_lowercase().ends_with(ext))
                        });

                        if all_unsafe {
                            tracing::warn!(
                                "Torrent {} contains unsafe files, marking it as failed",
                                local_torrent.hash
                            );
                            let mut local_torrent = local_torrent.into_active_model();
                            local_torrent.state = Set(TorrentState::Error);
                            local_torrent.error_message =
                                Set(Some("Torrent contains only unsafe files".to_string()));
                            local_torrent.save(db).await?;
                            continue;
                        }

                        let tx = db.begin().await?;
                        for file in files.into_iter() {
                            if should_ignore_path(&file.name) {
                                tracing::warn!(
                                    "Ignoring file {} in torrent {}",
                                    file.name,
                                    local_torrent.hash
                                );
                                continue;
                            }

                            let file_model = torrent_files::ActiveModel {
                                torrent_id: Set(local_torrent.id),
                                path: Set(file.name),
                                remote_id: Set(file.id as i64),
                                size: Set(file.size as i64),
                                ..Default::default()
                            };

                            let file = torrent_files::Entity::insert(file_model)
                                .on_conflict(
                                    OnConflict::columns([
                                        torrent_files::Column::TorrentId,
                                        torrent_files::Column::Path,
                                    ])
                                    .update_columns([
                                        torrent_files::Column::RemoteId,
                                        torrent_files::Column::Size,
                                    ])
                                    .to_owned(),
                                )
                                .exec_with_returning(&tx)
                                .await?;

                            file.add_to_downloads_folder(&tx).await?;
                        }

                        tx.commit().await?;
                        files_created = true;
                    }
                }

                let mut next_state = TorrentState::from_str(&debrid_torrent.download_state);
                if debrid_torrent.download_present && next_state == TorrentState::Downloading {
                    tracing::warn!(
                        "Overriding state from {:?} to Ready because download_present is true",
                        next_state
                    );

                    next_state = TorrentState::Ready;
                }

                if next_state == TorrentState::Ready && initial_state != next_state {
                    // if the torrent changes into a Ready state, we want to verify the torrent is ready.
                    // torbox for some reason has a lot of "broken" torrents that give a database error
                    // when you try and stream them. so whatever, this works for now.
                    let can_download = debrid
                        .get_download_link(debrid_torrent.id as i64, 0)
                        .await
                        .ok();

                    if can_download.is_none() {
                        tracing::warn!(
                            "Torrent {} has become Ready but its download link is broken. Marking it as failed",
                            local_torrent.hash
                        );
                        let mut local_torrent = local_torrent.into_active_model();
                        local_torrent.state = Set(TorrentState::Error);
                        local_torrent.error_message = Set(Some(
                            "Failed to create download link for torrent, its likely corrupted"
                                .to_string(),
                        ));
                        local_torrent.save(db).await?;
                        continue;
                    }
                }

                let remote_id = debrid_torrent.id as i64;
                if next_state != local_torrent.state {
                    tracing::info!(
                        "Torrent {} changed from {:?} to {:?}",
                        local_torrent.hash,
                        initial_state,
                        next_state
                    );
                }

                let has_finished = local_torrent.finished_at.is_some();

                let mut local_torrent = local_torrent.into_active_model();
                local_torrent.state = Set(next_state);
                local_torrent.remote_id = Set(Some(remote_id));
                local_torrent.progress = Set(debrid_torrent.progress);
                local_torrent.upload_speed = Set(debrid_torrent.upload_speed);
                local_torrent.download_speed = Set(debrid_torrent.download_speed);
                local_torrent.seeds = Set(debrid_torrent.seeds);
                local_torrent.peers = Set(debrid_torrent.peers);
                local_torrent.ratio = Set(debrid_torrent.ratio);
                local_torrent.eta_secs = Set(debrid_torrent.eta as i64);
                local_torrent.size = Set(debrid_torrent.size as i64);
                local_torrent.checked_at = Set(Some(chrono::Utc::now().timestamp_millis()));
                local_torrent.updated_at = Set(chrono::Utc::now().timestamp_millis());

                // the torrent name has to match the directory name on disk.
                // debrid_torrent.name doesn't always match, which causes mismatches with sonarr
                // and fucks things up.
                if let Some(dir_name) = dir_name {
                    local_torrent.name = Set(dir_name);
                }

                if !has_finished && next_state == TorrentState::Ready {
                    local_torrent.finished_at = Set(Some(chrono::Utc::now().timestamp_millis()));
                }

                if files_created {
                    local_torrent.files_created = Set(true);
                }

                local_torrent.save(db).await?;
            }

            let config = get_config();
            if config.delete_unused && remote_torrents.len() > 0 {
                for (hash, torrent) in remote_torrents {
                    tracing::info!("Deleting unused torrent {}", hash);
                    debrid.delete_torrent(&torrent.id).await?;
                }
            }

            finished_at = Instant::now();
            tracing::debug!("Finished reconciling torrents");
        } else {
            tracing::debug!("Scanner is blocked, skipping reconciler");
        }

        tokio::select! {
            _ = sleep(Duration::from_secs(RECHECK_INTERVAL_SECS)) => {}
            _ = notifier.notified() => {
                let since_finished_secs = finished_at.elapsed().as_secs();
                if since_finished_secs < MIN_RECHECK_INTERVAL_SECS {
                    let wait_time = MIN_RECHECK_INTERVAL_SECS - since_finished_secs;
                    tracing::info!("Reconciler notified early, waiting {} seconds", wait_time);
                    sleep(Duration::from_secs(wait_time)).await;
                } else {
                    tracing::info!("Reconciler notified early, skipping wait");
                }
            }
        }
    }
}
