use super::{
    torrent_files,
    torrents::{self, TorrentState},
};
use crate::config::get_config;
use sea_orm::{Condition, entity::prelude::*};
use std::time::{Duration, SystemTime};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "nodes")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub parent_id: Option<i64>,
    #[sea_orm(column_type = "Text")]
    pub name: String,
    pub size: i64,
    pub is_automatic: bool,
    pub torrent_id: Option<i64>,
    pub file_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "Entity",
        from = "Column::ParentId",
        to = "Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    SelfRef,
    #[sea_orm(
        belongs_to = "super::torrent_files::Entity",
        from = "Column::FileId",
        to = "super::torrent_files::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    TorrentFiles,
    #[sea_orm(
        belongs_to = "super::torrents::Entity",
        from = "Column::TorrentId",
        to = "super::torrents::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Torrents,
}

impl Related<super::torrent_files::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::TorrentFiles.def()
    }
}

impl Related<super::torrents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Torrents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

const DEFAULT_UID: u32 = 1000;
const DEFAULT_GID: u32 = 1000;
const DEFAULT_FILE_PERMS: u16 = 0x777;
const DEFAULT_DIR_PERMS: u16 = 0o777;

impl Model {
    pub fn get_kind(&self) -> fuse3::FileType {
        if self.file_id.is_some() {
            fuse3::FileType::RegularFile
        } else {
            fuse3::FileType::Directory
        }
    }

    pub fn get_attr(&self) -> fuse3::raw::reply::FileAttr {
        let kind = self.get_kind();
        let perm = if kind == fuse3::FileType::Directory {
            DEFAULT_DIR_PERMS
        } else {
            DEFAULT_FILE_PERMS
        };

        let ctime = SystemTime::UNIX_EPOCH + Duration::from_secs(self.created_at as u64);
        let mtime = SystemTime::UNIX_EPOCH + Duration::from_secs(self.updated_at as u64);
        let atime = mtime;

        fuse3::raw::reply::FileAttr {
            ino: self.id as u64,
            size: self.size as u64,
            blocks: 0,
            atime: atime.into(),
            mtime: mtime.into(),
            ctime: ctime.into(),
            kind,
            perm,
            nlink: 1,
            uid: DEFAULT_UID,
            gid: DEFAULT_GID,
            rdev: 0,
            blksize: 512,
        }
    }

    pub async fn get_disk_path(&self, db: &DatabaseConnection) -> Result<String, DbErr> {
        let mut parts = Vec::new();
        let mut current = self.clone();
        while let Some(parent_id) = current.parent_id {
            parts.push(current.name.clone());

            // root node
            if parent_id == 1 {
                break;
            }

            current = Entity::find_by_id(parent_id)
                .one(db)
                .await?
                .expect("parent node not found");
        }

        assert!(!parts.is_empty(), "Root node should not be empty");
        let path = parts.into_iter().rev().collect::<Vec<_>>().join("/");

        let config = get_config();
        let path = format!("{}/{}", config.mount_path.to_string_lossy(), path);
        Ok(path)
    }
}

impl Entity {
    /// Find a node that is either a directory or an active file.
    /// Active files are files where the backing torrent is ready.
    pub fn find_active(add_join: bool) -> Select<Entity> {
        let mut query = Entity::find().filter(
            Condition::any()
                // directories
                .add(Column::FileId.is_null())
                // files with a ready torrent
                .add(
                    Condition::all()
                        .add(Column::FileId.is_not_null())
                        .add(torrents::Column::RemoteId.is_not_null())
                        .add(torrents::Column::State.eq(TorrentState::Ready)),
                ),
        );

        if add_join {
            query = query
                .left_join(torrent_files::Entity)
                .left_join(torrents::Entity);
        }

        query
    }
}
