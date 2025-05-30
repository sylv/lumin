use crate::entities::{torrent_blocks, torrent_files, torrents};
use anyhow::Result;
use sea_orm::{Set, TransactionTrait, prelude::*};

pub async fn block_torrent(
    db: DatabaseConnection,
    torrent_id: i64,
    torrent_hash: &str,
    block_reason: &str,
) -> Result<()> {
    let torrent_hash = torrent_hash.to_lowercase();
    let tx = db.begin().await?;
    drop(db);

    // Remove files/nodes associated with this torrent
    torrent_files::Entity::delete_many()
        .filter(torrent_files::Column::TorrentId.eq(torrent_id))
        .exec(&tx)
        .await?;

    // Mark torrent for removal by the reconciler
    torrents::Entity::update_many()
        .set(torrents::ActiveModel {
            state: Set(torrents::TorrentState::Removing),
            ..Default::default()
        })
        .filter(torrents::Column::Hash.eq(&torrent_hash))
        .exec(&tx)
        .await?;

    // Add to blocked_torrents table
    torrent_blocks::Entity::insert(torrent_blocks::ActiveModel {
        hash: Set(torrent_hash),
        block_reason: Set(block_reason.to_string()),
        ..Default::default()
    })
    .exec(&tx)
    .await?;

    // Commit transaction
    tx.commit().await?;

    Ok(())
}
