use crate::{
    cache::Cache,
    entities::{nodes, torrent_files},
};
use anyhow::Result;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set, prelude::*,
};
use std::{collections::HashSet, sync::Arc, time::Duration};

const EXTRACTOR_INTERVAL_SECS: u64 = 30;
const VIDEO_EXTS: [&str; 7] = ["mp4", "mkv", "avi", "mov", "flv", "wmv", "webm"];

pub async fn start_duration_extractor(db: DatabaseConnection, cache: Arc<Cache>) -> Result<()> {
    tracing::info!("Starting duration extractor task");
    let mut extraction_errors = HashSet::new();

    loop {
        tokio::time::sleep(Duration::from_secs(EXTRACTOR_INTERVAL_SECS)).await;

        let cache_entries = cache.get_all_entries();
        for entry in cache_entries.into_iter() {
            if entry.get_duration_hint_secs().is_some() {
                // Skip files that already have a duration hint
                continue;
            }

            let entry_file = entry.get_file();
            if extraction_errors.contains(&entry_file.id) {
                continue;
            }

            if !VIDEO_EXTS
                .iter()
                .any(|ext| entry_file.path.to_lowercase().ends_with(ext))
            {
                // Skip non-video files
                continue;
            }

            if !entry.has_cached_chunks() {
                // these files are effectively unused, so theres no reason
                // to load metadata for streaming when it wont be used.
                continue;
            }

            let result = nodes::Entity::find()
                .filter(nodes::Column::FileId.eq(entry_file.id))
                .find_also_related(torrent_files::Entity)
                .one(&db)
                .await?;

            let Some((node, Some(file))) = result else {
                continue;
            };

            let node_disk_path = node.get_disk_path(&db).await?;
            match ffprobe::ffprobe(&node_disk_path) {
                Ok(probe) => {
                    let duration_secs = probe
                        .format
                        .duration
                        .as_ref()
                        .and_then(|duration| duration.parse::<f64>().ok());

                    if let Some(duration_secs) = duration_secs {
                        tracing::info!(
                            "Extracted duration `{}` seconds for file_id `{}`",
                            duration_secs,
                            file.id
                        );
                        entry.set_duration_hint_secs(duration_secs as u64);
                        let mut active_file = file.into_active_model();
                        active_file.duration_hint_secs = Set(Some(duration_secs as i64));
                        active_file.save(&db).await?;
                    } else {
                        tracing::warn!(
                            "No duration information found via ffprobe for file_id `{}`",
                            file.id
                        );

                        extraction_errors.insert(file.id);
                    }
                }
                Err(e) => {
                    tracing::error!("ffprobe failed for `{:?}`: {}", node_disk_path, e);
                    extraction_errors.insert(file.id);
                }
            }
        }
    }
}
