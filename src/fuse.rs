use fuse3::raw::prelude::*;
use fuse3::{Inode, Result};
use futures_util::stream::{self, Iter};
use sea_orm::{QuerySelect, Set, prelude::*};
use std::ffi::{OsStr, OsString};
use std::num::NonZeroU32;
use std::time::Duration;
use std::vec::IntoIter;
use tracing::trace;

use crate::cache::Cache;
use crate::entities::{nodes, torrent_files, torrents};
use std::sync::Arc;

const TTL: Duration = Duration::from_secs(1); // 1 second TTL
const TEST_INODE: u64 = 0xFFFFFFFF;

pub struct LuminFS {
    db: DatabaseConnection,
    cache: Arc<Cache>,
}

impl LuminFS {
    pub fn new(db: DatabaseConnection, cache: Arc<Cache>) -> Self {
        Self { db, cache }
    }

    async fn get_node(&self, ino: u64) -> anyhow::Result<Option<nodes::Model>> {
        let ino = ino as i64;
        let node = nodes::Entity::find_active(true)
            .filter(nodes::Column::Id.eq(ino))
            .one(&self.db)
            .await?;

        Ok(node)
    }

    async fn get_node_by_parent_and_name(
        &self,
        parent_ino: u64,
        name: &OsStr,
    ) -> anyhow::Result<Option<nodes::Model>> {
        let name_str = name.to_string_lossy().to_string();
        let parent_ino = parent_ino as i64;
        let node = nodes::Entity::find_active(true)
            .filter(nodes::Column::ParentId.eq(parent_ino))
            .filter(nodes::Column::Name.eq(name_str))
            .one(&self.db)
            .await?;

        Ok(node)
    }

    fn get_test_attr(&self) -> FileAttr {
        let now = std::time::SystemTime::now();
        FileAttr {
            ino: TEST_INODE,
            atime: now.into(),
            ctime: now.into(),
            mtime: now.into(),
            blksize: 4096,
            blocks: 0,
            gid: 0,
            kind: fuse3::FileType::RegularFile,
            nlink: 1,
            perm: 0o644,
            rdev: 0,
            size: 0,
            uid: 0,
        }
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

    async fn forget(&self, _req: Request, _inode: u64, _nlookup: u64) {}

    async fn lookup(&self, _req: Request, parent: u64, name: &OsStr) -> Result<ReplyEntry> {
        trace!("lookup(parent={}, name={:?})", parent, name);
        match self.get_node_by_parent_and_name(parent, name).await {
            Ok(Some(node)) => {
                let attr = node.get_attr();
                Ok(ReplyEntry {
                    attr: attr,
                    generation: 0,
                    ttl: TTL,
                })
            }
            Ok(None) => Err(libc::ENOENT.into()),
            Err(e) => {
                tracing::error!("lookup db error: {}", e);
                Err(libc::EIO.into())
            }
        }
    }

    async fn mkdir(
        &self,
        _req: Request,
        parent_id: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
    ) -> Result<ReplyEntry> {
        trace!("mkdir(parent={}, name={:?})", parent_id, name);
        let Some(parent) = nodes::Entity::find_by_id(parent_id as i64)
            .one(&self.db)
            .await
            .map_err(|e| {
                tracing::error!("mkdir db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?
        else {
            return Err(libc::ENOENT.into());
        };

        if parent.file_id.is_some() {
            // Cannot create a directory inside a file
            return Err(libc::ENOTDIR.into());
        }

        if parent.immutable {}

        let name_str = name.to_string_lossy().to_string();
        let node = nodes::Entity::insert(nodes::ActiveModel {
            parent_id: Set(Some(parent_id as i64)),
            name: Set(name_str),
            immutable: Set(false),
            ..Default::default()
        })
        .exec_with_returning(&self.db)
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
        let name_str = name.to_string_lossy();
        let to_delete = nodes::Entity::find()
            .filter(nodes::Column::ParentId.eq(parent as i64))
            .filter(nodes::Column::Name.eq(name_str))
            .one(&self.db)
            .await
            .map_err(|e| {
                tracing::error!("rmdir db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

        let Some(to_delete) = to_delete else {
            return Err(libc::ENOENT.into());
        };

        if to_delete.immutable {
            // download folder nodes are immutable
            return Err(libc::EPERM.into());
        }

        if to_delete.file_id.is_some() {
            // this is not a directory
            return Err(libc::ENOTDIR.into());
        }

        nodes::Entity::delete_by_id(to_delete.id)
            .exec(&self.db)
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
        inode: Inode,
        new_parent: Inode,
        new_name: &OsStr,
    ) -> Result<ReplyEntry> {
        let node = self.get_node(inode).await.map_err(|e| {
            tracing::error!("link db error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        let Some(node) = node else {
            return Err(libc::ENOENT.into());
        };

        if node.file_id.is_none() {
            return Err(libc::EPERM.into()); // Cannot link directories
        }

        let new_name_str = new_name.to_string_lossy().to_string();
        let new_node = nodes::Entity::insert(nodes::ActiveModel {
            parent_id: Set(Some(new_parent as i64)),
            name: Set(new_name_str),
            size: Set(node.size),
            file_id: Set(node.file_id.clone()),
            torrent_id: Set(node.torrent_id.clone()),
            immutable: Set(false),
            ..Default::default()
        })
        .exec_with_returning(&self.db)
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

        let Some(to_delete) = nodes::Entity::find()
            .filter(nodes::Column::ParentId.eq(parent as i64))
            .filter(nodes::Column::Name.eq(name_str))
            .one(&self.db)
            .await
            .map_err(|e| {
                tracing::error!("unlink db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?
        else {
            return Err(libc::ENOENT.into());
        };

        if to_delete.immutable {
            // download folder nodes are immutable
            return Err(libc::EPERM.into());
        }

        if to_delete.file_id.is_none() {
            // cannot unlink directories
            return Err(libc::EPERM.into());
        }

        nodes::Entity::delete_by_id(to_delete.id)
            .exec(&self.db)
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
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
    ) -> Result<()> {
        trace!(
            "rename(parent={}, name={:?}, newparent={}, newname={:?})",
            parent, name, newparent, newname
        );

        let node = self
            .get_node_by_parent_and_name(parent, name)
            .await
            .map_err(|e| {
                tracing::error!("rename db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

        if let Some(node) = node {
            let newname_str = newname.to_string_lossy().to_string();
            let mut model: nodes::ActiveModel = node.into();
            model.parent_id = Set(Some(newparent as i64));
            model.name = Set(newname_str);
            model.update(&self.db).await.map_err(|e| {
                tracing::error!("rename db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?;

            Ok(())
        } else {
            Err(libc::ENOENT.into())
        }
    }

    async fn create(
        &self,
        _req: Request,
        _parent: Inode,
        name: &OsStr,
        _mode: u32,
        _flags: u32,
    ) -> Result<ReplyCreated> {
        let name_str = name.to_string_lossy();
        if name_str == "sonarr_write_test.txt" || name_str == "radarr_write_test.txt" {
            return Ok(ReplyCreated {
                attr: self.get_test_attr(),
                generation: 0,
                fh: 0,
                flags: 0,
                ttl: TTL,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn mknod(
        &self,
        _req: Request,
        _parent: u64,
        name: &OsStr,
        _mode: u32,
        _rdev: u32,
    ) -> Result<ReplyEntry> {
        let name_str = name.to_string_lossy();
        if name_str == "sonarr_write_test.txt" || name_str == "radarr_write_test.txt" {
            return Ok(ReplyEntry {
                attr: self.get_test_attr(),
                generation: 0,
                ttl: TTL,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn write(
        &self,
        _req: Request,
        inode: Inode,
        _fh: u64,
        _offset: u64,
        data: &[u8],
        _write_flags: u32,
        _flags: u32,
    ) -> Result<ReplyWrite> {
        if inode == TEST_INODE {
            // pass the sonarr/radarr write test
            return Ok(ReplyWrite {
                written: data.len() as u32,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn setattr(
        &self,
        _req: Request,
        inode: Inode,
        _fh: Option<u64>,
        _set_attr: SetAttr,
    ) -> Result<ReplyAttr> {
        if inode == TEST_INODE {
            return Ok(ReplyAttr {
                attr: self.get_test_attr(),
                ttl: TTL,
            });
        }

        return Err(libc::ENOSYS.into());
    }

    async fn flush(&self, _req: Request, inode: Inode, _fh: u64, _lock_owner: u64) -> Result<()> {
        if inode == TEST_INODE {
            return Ok(());
        }

        Err(libc::ENOSYS.into())
    }

    async fn release(
        &self,
        _req: Request,
        inode: Inode,
        _fh: u64,
        _flags: u32,
        _lock_owner: u64,
        _flush: bool,
    ) -> Result<()> {
        if inode == TEST_INODE {
            return Ok(());
        }

        Err(libc::ENOSYS.into())
    }

    async fn getattr(
        &self,
        _req: Request,
        inode: u64,
        _fh: Option<u64>,
        _flags: u32,
    ) -> Result<ReplyAttr> {
        trace!("getattr(inode={})", inode);
        match self.get_node(inode).await {
            Ok(Some(node)) => {
                let attr = node.get_attr();
                Ok(ReplyAttr {
                    attr: attr,
                    ttl: TTL,
                })
            }
            Ok(None) => Err(libc::ENOENT.into()),
            Err(e) => {
                tracing::error!("getattr db error: {}", e);
                Err(libc::EIO.into())
            }
        }
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
        // let node = query_as!(
        //     Node,
        //     "SELECT id, parent_id, name, size, created_at, updated_at, torrent_hash FROM nodes WHERE id = ?",
        //     ino
        // )
        // .fetch_optional(&self.db)
        // .await
        // .map_err(|e| {
        //     tracing::error!("readdirplus db error: {}", e);
        //     fuse3::Errno::from(libc::EIO)
        // })?;
        let node = nodes::Entity::find()
            .filter(nodes::Column::Id.eq(ino))
            .one(&self.db)
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

        let children = nodes::Entity::find_active(true)
            .filter(nodes::Column::ParentId.eq(ino))
            .all(&self.db)
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
            .map(|(ino, kind, name, attr, offset)| {
                Ok(DirectoryEntryPlus {
                    attr: attr,
                    attr_ttl: TTL,
                    entry_ttl: TTL,
                    generation: 0,
                    inode: ino as u64,
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

    async fn read(
        &self,
        _req: Request,
        inode: u64,
        _fh: u64,
        offset: u64,
        size: u32,
    ) -> Result<ReplyData> {
        trace!("read(inode={}, offset={}, size={})", inode, offset, size);
        let inode = inode as i64;
        let (node, file, torrent) = nodes::Entity::find_active(false)
            .filter(nodes::Column::Id.eq(inode))
            .find_also_related(torrent_files::Entity)
            .find_also_related(torrents::Entity)
            .one(&self.db)
            .await
            .map_err(|e| {
                tracing::error!("read db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?
            .ok_or_else(|| libc::ENOENT)?;

        let (Some(file), Some(torrent)) = (file, torrent) else {
            return Err(libc::EISDIR.into());
        };

        if offset + size as u64 > node.size as u64 {
            return Err(libc::EINVAL.into());
        }

        let torrent_remote_id = torrent.remote_id.expect("torrent must have remote_id");
        let file = self.cache.upsert_entry(file, torrent_remote_id);

        let data = file.read_bytes(offset, size as u64).await.map_err(|e| {
            tracing::error!("read cache error: {}", e);
            fuse3::Errno::from(libc::EIO)
        })?;

        Ok(ReplyData { data: data.into() })
    }

    // necessary for podman
    async fn statfs(&self, _req: Request, _inode: fuse3::Inode) -> Result<ReplyStatFs> {
        trace!("statfs(inode={})", _inode);

        // Get total size of all files to estimate blocks
        let (file_count, total_size): (i64, i64) = nodes::Entity::find_active(true)
            .select_only()
            .column_as(nodes::Column::Size.count(), "file_count")
            .column_as(nodes::Column::Size.sum(), "total_size")
            .filter(nodes::Column::Size.is_not_null())
            .into_tuple()
            .one(&self.db)
            .await
            .map_err(|e| {
                tracing::error!("statfs db error: {}", e);
                fuse3::Errno::from(libc::EIO)
            })?
            .unwrap_or((0, 0));

        let file_count = file_count as u64;
        let total_size = total_size as u64;

        const BLOCK_SIZE: u64 = 4096;
        let total_blocks = (total_size + BLOCK_SIZE - 1) / BLOCK_SIZE; // Ceiling division

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
