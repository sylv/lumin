use crate::cache::{Cache, CacheFile};
use crate::mount::node::{Node, TEST_NODE_ID, get_test_attr};
use fuse3::Result;
use fuse3::raw::prelude::*;
use futures_util::stream::{self, Iter};
use sqlx::SqlitePool;
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;
use std::vec::IntoIter;
use tracing::trace;

mod node;

const TTL: Duration = Duration::from_secs(1); // 1 second TTL

pub struct LuminFS {
    pool: SqlitePool,
    cache: Arc<Cache>,
}

impl LuminFS {
    pub fn new(pool: SqlitePool, cache: Arc<Cache>) -> Self {
        Self { pool, cache }
    }
}

// todo: cache node/file/torrent metadata
impl Filesystem for LuminFS {
    async fn init(&self, _req: Request) -> Result<ReplyInit> {
        Ok(ReplyInit {
            max_write: NonZeroU32::new(16 * 1024).unwrap(),
        })
    }

    async fn destroy(&self, _req: Request) {}

    async fn forget(&self, _req: Request, _node_id: u64, _nlookup: u64) {}

    async fn lookup(&self, _req: Request, parent: u64, name: &OsStr) -> Result<ReplyEntry> {
        trace!("lookup(parent={}, name={:?})", parent, name);

        let parent = parent as i64;
        let name_str = name.to_string_lossy().to_string();
        let node = sqlx::query_as!(
            Node,
            "SELECT id, parent_id, size, created_at, updated_at, file_id, name FROM nodes WHERE parent_id = ? AND name = ?",
            parent,
            name_str
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("lookup db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(node) = node else {
            return Err(libc::ENOENT.into());
        };

        let attr = node.get_attr();
        Ok(ReplyEntry {
            attr: attr,
            generation: 0,
            ttl: TTL,
        })
    }

    async fn mkdir(&self, _req: Request, parent_id: u64, name: &OsStr, _mode: u32, _umask: u32) -> Result<ReplyEntry> {
        trace!("mkdir(parent={}, name={:?})", parent_id, name);
        let parent_id = parent_id as i64;
        let parent = sqlx::query!("SELECT id, file_id, readonly FROM nodes WHERE id = ?", parent_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("mkdir db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

        let Some(parent) = parent else {
            return Err(libc::ENOENT.into());
        };

        if parent.file_id.is_some() {
            // Cannot create a directory inside a file
            return Err(libc::ENOTDIR.into());
        }

        if parent.readonly == 1 && parent.id != 1 {
            // cannot create directories inside immutable nodes
            // (except the root node, that would be a little silly)
            return Err(libc::EPERM.into());
        }

        let name_str = name.to_string_lossy().to_string();
        let node = sqlx::query_as!(
            Node,
            "INSERT INTO nodes (parent_id, name, readonly) VALUES (?, ?, ?) 
            RETURNING id, parent_id, size, created_at, updated_at, file_id, name",
            parent_id,
            name_str,
            0
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("mkdir db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let attr = node.get_attr();
        Ok(ReplyEntry {
            attr: attr,
            generation: 0,
            ttl: TTL,
        })
    }

    async fn rmdir(&self, _req: Request, parent: u64, name: &OsStr) -> Result<()> {
        trace!("rmdir(parent={}, name={:?})", parent, name);
        let parent_id = parent as i64;
        let name_str = name.to_string_lossy();
        let to_delete = sqlx::query!(
            "SELECT id, readonly, file_id FROM nodes WHERE parent_id = ? AND name = ?",
            parent_id,
            name_str
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("rmdir db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(to_delete) = to_delete else {
            return Err(libc::ENOENT.into());
        };

        if to_delete.readonly == 1 {
            // download folder nodes are immutable
            return Err(libc::EPERM.into());
        }

        if to_delete.file_id.is_some() {
            // this is not a directory
            return Err(libc::ENOTDIR.into());
        }

        sqlx::query!("DELETE FROM nodes WHERE id = ?", to_delete.id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("rmdir db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

        Ok(())
    }

    async fn link(
        &self,
        _req: Request,
        original_node_id: u64,
        link_parent_id: u64,
        link_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let original_node_id = original_node_id as i64;
        let original_node = sqlx::query!(
            "SELECT id, size, file_id, torrent_id FROM nodes WHERE id = ?",
            original_node_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("link db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(original_node) = original_node else {
            return Err(libc::ENOENT.into());
        };

        if original_node.file_id.is_none() {
            return Err(libc::EPERM.into()); // Cannot link directories
        }

        let link_name_str = link_name.to_string_lossy().to_string();
        let link_parent_id = link_parent_id as i64;
        let new_node = sqlx::query_as!(
            Node,
            "INSERT INTO nodes (parent_id, name, size, file_id, torrent_id, readonly) VALUES (?, ?, ?, ?, ?, ?) 
            RETURNING id, parent_id, size, created_at, updated_at, file_id, name",
            link_parent_id,
            link_name_str,
            original_node.size,
            original_node.file_id,
            original_node.torrent_id,
            0
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("link db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let attr = new_node.get_attr();
        Ok(ReplyEntry {
            attr: attr,
            generation: 0,
            ttl: TTL,
        })
    }

    async fn unlink(&self, _req: Request, parent: u64, name: &OsStr) -> Result<()> {
        trace!("unlink(parent={}, name={:?})", parent, name);
        let name_str = name.to_string_lossy();
        if name_str == "sonarr_write_test.txt" || name_str == "radarr_write_test.txt" {
            return Ok(());
        }

        let parent = parent as i64;
        let to_delete = sqlx::query!(
            "SELECT id, readonly, file_id FROM nodes WHERE parent_id = ? AND name = ?",
            parent,
            name_str
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("unlink db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(to_delete) = to_delete else {
            return Err(libc::ENOENT.into());
        };

        if to_delete.readonly == 1 {
            // download folder nodes are immutable
            return Err(libc::EPERM.into());
        }

        if to_delete.file_id.is_none() {
            // cannot unlink directories
            return Err(libc::EPERM.into());
        }

        sqlx::query!("DELETE FROM nodes WHERE id = ?", to_delete.id)
            .execute(&self.pool)
            .await
            .map_err(|e| {
                tracing::error!("unlink db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

        Ok(())
    }

    async fn rename(
        &self,
        _req: Request,
        parent_id: u64,
        name: &OsStr,
        new_parent: u64,
        new_name: &OsStr,
    ) -> Result<()> {
        trace!(
            "rename(parent={}, name={:?}, newparent={}, newname={:?})",
            parent_id, name, new_parent, new_name
        );

        let parent_id = parent_id as i64;
        let new_parent_id = new_parent as i64;
        let name_str = name.to_string_lossy().to_string();
        let new_name_str = new_name.to_string_lossy().to_string();
        let result = sqlx::query!(
            "UPDATE nodes SET parent_id = ?, name = ? WHERE parent_id = ? AND name = ?",
            new_parent_id,
            new_name_str,
            parent_id,
            name_str,
        )
        .execute(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("rename db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        if result.rows_affected() == 0 {
            return Err(libc::ENOENT.into());
        }

        Ok(())
    }

    async fn create(&self, _req: Request, _parent: u64, name: &OsStr, _mode: u32, _flags: u32) -> Result<ReplyCreated> {
        let name_str = name.to_string_lossy();
        if name_str == "sonarr_write_test.txt" || name_str == "radarr_write_test.txt" {
            return Ok(ReplyCreated {
                attr: get_test_attr(),
                generation: 0,
                fh: 0,
                flags: 0,
                ttl: TTL,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn mknod(&self, _req: Request, _parent: u64, name: &OsStr, _mode: u32, _rdev: u32) -> Result<ReplyEntry> {
        let name_str = name.to_string_lossy();
        if name_str == "sonarr_write_test.txt" || name_str == "radarr_write_test.txt" {
            return Ok(ReplyEntry {
                attr: get_test_attr(),
                generation: 0,
                ttl: TTL,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn write(
        &self,
        _req: Request,
        node_id: u64,
        _fh: u64,
        _offset: u64,
        data: &[u8],
        _write_flags: u32,
        _flags: u32,
    ) -> Result<ReplyWrite> {
        if node_id == TEST_NODE_ID {
            // pass the sonarr/radarr write test
            return Ok(ReplyWrite {
                written: data.len() as u32,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn setattr(&self, _req: Request, node_id: u64, _fh: Option<u64>, set_attr: SetAttr) -> Result<ReplyAttr> {
        trace!("setattr(node_id={}, fh={:?}, set_attr={:?})", node_id, _fh, set_attr);

        if node_id == TEST_NODE_ID {
            return Ok(ReplyAttr {
                attr: get_test_attr(),
                ttl: TTL,
            });
        };

        let node_id = node_id as i64;
        let node = sqlx::query_as!(
            Node,
            "SELECT id, parent_id, size, created_at, updated_at, file_id, name FROM nodes WHERE id = ?",
            node_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("setattr db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(node) = node else {
            return Err(libc::ENOENT.into());
        };

        // sonarr, when deleting files, appears to change the files attr
        // so we just kinda.. don't, or else sonarr can't delete files.
        Ok(ReplyAttr {
            attr: node.get_attr(),
            ttl: TTL,
        })
    }

    async fn getattr(&self, _req: Request, node_id: u64, _fh: Option<u64>, _flags: u32) -> Result<ReplyAttr> {
        trace!("getattr(node_id={})", node_id);
        let node_id = node_id as i64;
        let node = sqlx::query_as!(
            Node,
            "SELECT id, parent_id, size, created_at, updated_at, file_id, name FROM nodes WHERE id = ?",
            node_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("getattr db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(node) = node else {
            return Err(libc::ENOENT.into());
        };

        let attr = node.get_attr();
        Ok(ReplyAttr { attr: attr, ttl: TTL })
    }

    async fn flush(&self, _req: Request, node_id: u64, _fh: u64, _lock_owner: u64) -> Result<()> {
        if node_id == TEST_NODE_ID {
            return Ok(());
        }

        Err(libc::ENOSYS.into())
    }

    async fn release(
        &self,
        _req: Request,
        node_id: u64,
        _fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
    ) -> Result<()> {
        if node_id == TEST_NODE_ID {
            return Ok(());
        }

        Err(libc::ENOSYS.into())
    }

    type DirEntryStream<'a>
        = Iter<IntoIter<Result<DirectoryEntry>>>
    where
        Self: 'a;

    type DirEntryPlusStream<'a>
        = Iter<IntoIter<Result<DirectoryEntryPlus>>>
    where
        Self: 'a;

    async fn readdirplus(
        &self,
        _req: Request,
        ino: u64,
        _fh: u64,
        offset: u64,
        _lock_owner: u64,
    ) -> Result<ReplyDirectoryPlus<Self::DirEntryPlusStream<'_>>> {
        trace!("readdirplus(ino={}, offset={})", ino, offset);
        let ino = ino as i64;
        let node = sqlx::query_as!(
            Node,
            "SELECT id, parent_id, size, created_at, updated_at, file_id, name FROM nodes WHERE id = ?",
            ino
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("readdirplus db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(node) = node else {
            return Err(libc::ENOENT.into());
        };

        if node.file_id.is_some() {
            return Err(libc::ENOTDIR.into());
        }

        // todo: this should support offset/limit and maybe streaming
        let children = sqlx::query_as!(
            Node,
            "SELECT id, parent_id, size, created_at, updated_at, file_id, name FROM nodes WHERE parent_id = ?",
            ino
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("readdirplus db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let parent_ino = node.parent_id.unwrap_or(1);
        let mut entries = vec![
            (node.id, FileType::Directory, ".", node.get_attr(), 1),
            (parent_ino, FileType::Directory, "..", node.get_attr(), 2),
        ];

        for (offset, child) in children.iter().enumerate() {
            let attr = child.get_attr();
            entries.push((child.id, attr.kind, &child.name, attr, offset as u64 + 3));
        }

        let children = entries
            .into_iter()
            .map(|(node_id, kind, name, attr, offset)| {
                Ok(DirectoryEntryPlus {
                    attr: attr,
                    attr_ttl: TTL,
                    entry_ttl: TTL,
                    generation: 0,
                    inode: node_id as u64,
                    kind,
                    name: OsString::from(name),
                    offset: offset as i64,
                })
            })
            .skip(offset as _)
            .collect::<Vec<_>>();

        Ok(ReplyDirectoryPlus {
            entries: stream::iter(children),
        })
    }

    async fn read(&self, _req: Request, node_id: u64, _fh: u64, offset: u64, size: u32) -> Result<ReplyData> {
        trace!("read(node_id={}, offset={}, size={})", node_id, offset, size);
        // let (node, file, torrent) = nodes::Entity::find_active(false)
        //     .filter(nodes::Column::Id.eq(node_id))
        //     .find_also_related(torrent_files::Entity)
        //     .find_also_related(torrents::Entity)
        //     .one(&self.pool)
        //     .await
        //     .map_err(|e| {
        //         tracing::error!("read db error: {}", e);
        //         fuse3::Errno::from(libc::EIO)
        //     })?
        //     .ok_or_else(|| libc::ENOENT)?;

        // let (Some(file), Some(torrent)) = (file, torrent) else {
        //     return Err(libc::EISDIR.into());
        // };

        // todo: this should be handled by the "upsert_entry" call, we should just give it a file id.
        // doing it this way means for every read request we are scanning 3 tables
        let node_id = node_id as i64;
        let cache_file = sqlx::query_as!(
            CacheFile,
            r#"SELECT tf.id AS "id!", tf.size AS "size!", tf.path AS "path!", tf.debrid_id AS "file_debrid_id!", t.debrid_id AS "torrent_debrid_id!"
            FROM nodes
            LEFT JOIN torrent_files tf ON tf.id = nodes.file_id
            LEFT JOIN torrents t ON t.id = tf.torrent_id
            WHERE nodes.id = ? AND tf.debrid_id IS NOT NULL AND t.debrid_id IS NOT NULL"#,
            node_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("read db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        // todo: missing debrid_id will cause ENOENT but it should cause EIO
        let Some(cache_file) = cache_file else {
            return Err(libc::ENOENT.into());
        };

        if offset + size as u64 > cache_file.size as u64 {
            return Err(libc::EINVAL.into());
        }

        let file = self.cache.upsert_entry(cache_file);
        let data = file.read_bytes(offset, size as u64).await.map_err(|e| {
            tracing::error!("cache read error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        Ok(ReplyData { data: data.into() })
    }

    // necessary for podman for some reason.
    // todo: this is kinda gross, it would be better to.. do something else. maybe just use fake data?
    async fn statfs(&self, _req: Request, _node_id: u64) -> Result<ReplyStatFs> {
        trace!("statfs(node_id={})", _node_id);
        let result = sqlx::query!(
            "SELECT COUNT(*) AS file_count, COALESCE(SUM(size), 0) AS total_size FROM nodes WHERE size IS NOT NULL"
        )
        .fetch_one(&self.pool)
        .await
        .map_err(|e| {
            tracing::error!("statfs db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let file_count = result.file_count as u64;
        let total_size = result.total_size as u64;

        const BLOCK_SIZE: u64 = 4096;
        let total_blocks = (total_size + BLOCK_SIZE - 1) / BLOCK_SIZE;

        // Reserve approximately 10% of space as free
        let free_blocks = total_blocks / 10;

        Ok(ReplyStatFs {
            blocks: total_blocks,      // Total data blocks based on actual file sizes
            bfree: free_blocks,        // Free blocks (10% of total)
            bavail: free_blocks,       // Free blocks available to unprivileged user
            files: file_count,         // Actual number of files in the system
            ffree: 1_000_000,          // We can create plenty of new files
            bsize: BLOCK_SIZE as u32,  // Block size
            namelen: 255,              // Maximum length of filenames
            frsize: BLOCK_SIZE as u32, // Fragment size same as block size
        })
    }
}
