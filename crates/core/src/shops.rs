use anyhow::Result;
use sqlx::MySqlPool;

use crate::inventory::{GameLimits, MoveError};
pub use crate::inventory::{SHOP_TYPE_BUY, SHOP_TYPE_CRAFT, SHOP_TYPE_SELL};

#[derive(Debug, Clone)]
pub struct ShopItem {
    pub item: i32,
    pub count: u64,
    pub price: u64,
}

#[derive(Debug, Clone)]
pub struct Shop {
    pub kind: i32,
    pub title: String,
    pub time_ms: i64,
    pub items: Vec<ShopItem>,
}

pub async fn get_shop(pool: &MySqlPool, char_id: u32) -> Result<Option<Shop>> {
    let status: Option<(i64, i32, Option<String>)> = sqlx::query_as(
        "SELECT CAST(time AS SIGNED), type, title FROM character_offline_trade WHERE charId = ? LIMIT 1",
    )
    .bind(char_id)
    .fetch_optional(pool)
    .await?;
    let Some((time_ms, kind, title)) = status else {
        return Ok(None);
    };
    let rows: Vec<(i32, u64, u64)> = sqlx::query_as(
        "SELECT CAST(item AS SIGNED), count, price FROM character_offline_trade_items WHERE charId = ?",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await?;
    Ok(Some(Shop {
        kind,
        title: title.unwrap_or_default(),
        time_ms,
        items: rows
            .into_iter()
            .map(|(item, count, price)| ShopItem { item, count, price })
            .collect(),
    }))
}

pub async fn delete_shop(pool: &MySqlPool, char_id: u32) -> Result<(), MoveError> {
    let mut tx = pool.begin().await?;
    let online: Option<(u8,)> = sqlx::query_as("SELECT online FROM characters WHERE charId = ?")
        .bind(char_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((online,)) = online else {
        return Err(MoveError::CharacterNotFound);
    };
    if online == 1 {
        return Err(MoveError::CharacterOnline);
    }
    sqlx::query("DELETE FROM character_offline_trade WHERE charId = ?")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM character_offline_trade_items WHERE charId = ?")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_shop(
    pool: &MySqlPool,
    limits: &GameLimits,
    char_id: u32,
    kind: i32,
    title: &str,
    items: &[ShopItem],
) -> Result<(), MoveError> {
    match kind {
        SHOP_TYPE_SELL | SHOP_TYPE_BUY => {
            if !limits.offline_trade_enabled {
                return Err(MoveError::ShopsDisabled);
            }
        }
        SHOP_TYPE_CRAFT => {
            if !limits.offline_craft_enabled {
                return Err(MoveError::ShopsDisabled);
            }
        }
        _ => return Err(MoveError::NotAllowed),
    }
    if items.is_empty() {
        return Err(MoveError::NotAllowed);
    }
    for item in items {
        if item.price == 0 {
            return Err(MoveError::NotAllowed);
        }
        if kind != SHOP_TYPE_CRAFT && item.count == 0 {
            return Err(MoveError::NotAllowed);
        }
    }

    let mut tx = pool.begin().await?;
    let online: Option<(u8,)> = sqlx::query_as("SELECT online FROM characters WHERE charId = ? FOR UPDATE")
        .bind(char_id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some((online,)) = online else {
        return Err(MoveError::CharacterNotFound);
    };
    if online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    match kind {
        SHOP_TYPE_SELL => {
            for item in items {
                let row: Option<(u64,)> = sqlx::query_as(
                    "SELECT count FROM items WHERE object_id = ? AND owner_id = ? AND loc = 'INVENTORY' LIMIT 1",
                )
                .bind(item.item)
                .bind(char_id)
                .fetch_optional(&mut *tx)
                .await?;
                let Some((available,)) = row else {
                    return Err(MoveError::ItemNotFound);
                };
                if item.count > available {
                    return Err(MoveError::NotAllowed);
                }
            }
        }
        SHOP_TYPE_BUY => {}
        SHOP_TYPE_CRAFT => {
            for item in items {
                let known: (i64,) = sqlx::query_as(
                    "SELECT COUNT(*) FROM character_recipebook WHERE charId = ? AND id = ?",
                )
                .bind(char_id)
                .bind(item.item)
                .fetch_one(&mut *tx)
                .await?;
                if known.0 == 0 {
                    return Err(MoveError::RecipeNotKnown);
                }
            }
        }
        _ => return Err(MoveError::NotAllowed),
    }

    let title = title.trim();
    let title = if title.is_empty() {
        match kind {
            SHOP_TYPE_BUY => "WTB".to_string(),
            SHOP_TYPE_CRAFT => "Craft".to_string(),
            _ => "WTS".to_string(),
        }
    } else {
        title.chars().take(50).collect()
    };

    sqlx::query("DELETE FROM character_offline_trade WHERE charId = ?")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM character_offline_trade_items WHERE charId = ?")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;

    let now_ms = chrono::Utc::now().timestamp_millis();
    sqlx::query("INSERT INTO character_offline_trade (`charId`, `time`, `type`, `title`) VALUES (?, ?, ?, ?)")
        .bind(char_id)
        .bind(now_ms)
        .bind(kind)
        .bind(&title)
        .execute(&mut *tx)
        .await?;
    for item in items {
        sqlx::query(
            "INSERT INTO character_offline_trade_items (`charId`, `item`, `count`, `price`) VALUES (?, ?, ?, ?)",
        )
        .bind(char_id)
        .bind(item.item)
        .bind(item.count)
        .bind(item.price)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(())
}
