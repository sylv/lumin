use fuse3::raw::prelude::*;
use std::time::{Duration, SystemTime};

pub const TEST_NODE_ID: u64 = 0xFFFFFFFF;
const DEFAULT_UID: u32 = 1000;
const DEFAULT_GID: u32 = 1000;
const DEFAULT_FILE_PERMS: u16 = 0x777;
const DEFAULT_DIR_PERMS: u16 = 0o777;

#[derive(sqlx::FromRow)]
pub struct Node {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub size: i64,
    pub name: String,
    pub file_id: Option<i64>,
    pub updated_at: i64,
    pub created_at: i64,
}

impl Node {
    pub fn get_kind(&self) -> FileType {
        if self.file_id.is_some() {
            FileType::RegularFile
        } else {
            FileType::Directory
        }
    }

    pub fn get_attr(&self) -> FileAttr {
        let kind = self.get_kind();
        let perm = if kind == FileType::Directory {
            DEFAULT_DIR_PERMS
        } else {
            DEFAULT_FILE_PERMS
        };

        let ctime: SystemTime = SystemTime::UNIX_EPOCH + Duration::from_secs(self.created_at as u64);
        let mtime: SystemTime = SystemTime::UNIX_EPOCH + Duration::from_secs(self.updated_at as u64);
        let atime = mtime;

        FileAttr {
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
}

pub fn get_test_attr() -> FileAttr {
    let now = std::time::SystemTime::now();
    FileAttr {
        ino: TEST_NODE_ID,
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
