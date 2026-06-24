use chrono::NaiveDateTime;
use diesel::prelude::*;
use uuid::Uuid;

use crate::schema::items;

#[derive(Debug, Queryable, Selectable)]
#[diesel(table_name = items)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub created_at: NaiveDateTime,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = items)]
pub struct NewItem {
    pub name: String,
}

impl From<Item> for shared::Item {
    fn from(item: Item) -> Self {
        shared::Item {
            id: item.id,
            name: item.name,
            created_at: item.created_at,
        }
    }
}
