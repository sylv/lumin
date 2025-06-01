use super::nodes;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{DatabaseTransaction, Set};
use serde::Serialize;
use specta::Type;

#[derive(Clone, Debug, PartialEq, Serialize, Type, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "torrent_files")]
#[specta(rename = "TorrentFile")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub remote_id: i64,
    pub torrent_id: i64,
    pub duration_hint_secs: Option<i64>,
    #[sea_orm(column_type = "Text")]
    pub path: String,
    pub size: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::nodes::Entity")]
    Nodes,
    #[sea_orm(
        belongs_to = "super::torrents::Entity",
        from = "Column::TorrentId",
        to = "super::torrents::Column::Id",
        on_update = "NoAction",
        on_delete = "Cascade"
    )]
    Torrents,
}

impl Related<super::nodes::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Nodes.def()
    }
}

impl Related<super::torrents::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Torrents.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    pub async fn add_to_downloads_folder(
        &self,
        db: &DatabaseTransaction,
    ) -> Result<(), sea_orm::DbErr> {
        let parts = self.path.split('/').collect::<Vec<&str>>();
        let parts_len = parts.len();
        let mut parent_id = 2; // downloads dir id
        for (i, part) in parts.iter().enumerate() {
            let is_last = i == parts_len - 1;
            let name = <&str as ToString>::to_string(part);
            let mut node = nodes::ActiveModel {
                parent_id: Set(Some(parent_id)),
                immutable: Set(true),
                name: Set(name),
                ..Default::default()
            };

            if is_last {
                node.size = Set(self.size);
                node.file_id = Set(Some(self.id));
                node.torrent_id = Set(Some(self.torrent_id));
            }

            let node = nodes::Entity::insert(node)
                .on_conflict(
                    OnConflict::columns([nodes::Column::ParentId, nodes::Column::Name])
                        // we can't do nothing or exec with returning won't work (RETURNING only
                        // works if a column is updated or inserted)
                        .update_column(nodes::Column::Size)
                        .to_owned(),
                )
                .exec_with_returning(db)
                .await?;

            parent_id = node.id;
            if is_last {
                return Ok(());
            }
        }

        unreachable!("Should have returned in the loop")
    }
}
