use anyhow::Result;
use sqlx::MySqlPool;

use crate::models::{
    AccountRow, CharacterRow, DashboardStats, ItemOwnerRow, ItemRow, OnlineRow,
};

pub async fn dashboard_stats(pool: &MySqlPool) -> Result<DashboardStats> {
    let row = sqlx::query_as::<_, DashboardStats>(
        r#"
        SELECT
            CAST((SELECT COUNT(*) FROM accounts) AS SIGNED) AS accounts,
            CAST((SELECT COUNT(*) FROM characters WHERE deletetime = 0) AS SIGNED) AS characters,
            CAST((SELECT COUNT(*) FROM characters WHERE online = 1) AS SIGNED) AS online,
            CAST((SELECT COUNT(*) FROM items) AS SIGNED) AS items,
            CAST((SELECT COALESCE(SUM(count), 0) FROM items WHERE item_id = 57) AS SIGNED) AS adena,
            CAST((SELECT COUNT(*) FROM clan_data) AS SIGNED) AS clans
        "#,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn online_players(pool: &MySqlPool, limit: i64) -> Result<Vec<OnlineRow>> {
    let rows = sqlx::query_as::<_, OnlineRow>(
        r#"
        SELECT c.charId AS char_id, c.char_name, c.account_name, c.level, c.classid,
               cd.clan_name
        FROM characters c
        LEFT JOIN clan_data cd ON cd.clan_id = c.clanid
        WHERE c.online = 1 AND c.deletetime = 0
        ORDER BY c.level DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn accounts_page(pool: &MySqlPool, q: &str) -> Result<Vec<AccountRow>> {
    let like = format!("%{q}%");
    let rows = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT a.login, a.accessLevel AS access_level, a.lastactive,
               a.lastIP AS last_ip, CAST(a.created_time AS DATETIME) AS created_time, a.email,
               p.expiration AS ban_expiration, p.reason AS ban_reason
        FROM accounts a
        LEFT JOIN (
            SELECT `key` AS k, MAX(expiration) AS expiration, MAX(reason) AS reason
            FROM punishments
            WHERE type = 'BAN'
            GROUP BY `key`
        ) p ON p.k = a.login
        WHERE a.login LIKE ?
        ORDER BY a.lastactive DESC
        LIMIT 200
        "#,
    )
    .bind(&like)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn account_detail(pool: &MySqlPool, login: &str) -> Result<Option<AccountRow>> {
    let rows = sqlx::query_as::<_, AccountRow>(
        r#"
        SELECT a.login, a.accessLevel AS access_level, a.lastactive,
               a.lastIP AS last_ip, CAST(a.created_time AS DATETIME) AS created_time, a.email,
               p.expiration AS ban_expiration, p.reason AS ban_reason
        FROM accounts a
        LEFT JOIN (
            SELECT `key` AS k, MAX(expiration) AS expiration, MAX(reason) AS reason
            FROM punishments
            WHERE type = 'BAN'
            GROUP BY `key`
        ) p ON p.k = a.login
        WHERE a.login = ?
        LIMIT 1
        "#,
    )
    .bind(login)
    .fetch_optional(pool)
    .await?;
    Ok(rows)
}

pub async fn search_characters(pool: &MySqlPool, q: &str) -> Result<Vec<CharacterRow>> {
    let like = format!("%{q}%");
    let rows = sqlx::query_as::<_, CharacterRow>(
        r#"
        SELECT c.charId AS char_id, c.char_name, c.account_name, c.level, c.classid,
               c.race, c.clanid AS clan_id, cd.clan_name, c.online, c.pvpkills,
               c.pkkills, c.karma, c.title, c.accesslevel, c.lastAccess AS last_access,
               c.createDate AS create_date, c.deletetime, c.nobless, c.exp, c.sp,
               c.vitality_points, c.x, c.y, c.z
        FROM characters c
        LEFT JOIN clan_data cd ON cd.clan_id = c.clanid
        WHERE c.char_name LIKE ? OR c.account_name LIKE ?
        ORDER BY c.lastAccess DESC
        LIMIT 200
        "#,
    )
    .bind(&like)
    .bind(&like)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn character_by_id(pool: &MySqlPool, char_id: u32) -> Result<Option<CharacterRow>> {
    let row = sqlx::query_as::<_, CharacterRow>(
        r#"
        SELECT c.charId AS char_id, c.char_name, c.account_name, c.level, c.classid,
               c.race, c.clanid AS clan_id, cd.clan_name, c.online, c.pvpkills,
               c.pkkills, c.karma, c.title, c.accesslevel, c.lastAccess AS last_access,
               c.createDate AS create_date, c.deletetime, c.nobless, c.exp, c.sp,
               c.vitality_points, c.x, c.y, c.z
        FROM characters c
        LEFT JOIN clan_data cd ON cd.clan_id = c.clanid
        WHERE c.charId = ?
        LIMIT 1
        "#,
    )
    .bind(char_id)
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

pub async fn character_items(pool: &MySqlPool, char_id: u32) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query_as::<_, ItemRow>(
        r#"
        SELECT owner_id, object_id, item_id, count, enchant_level, loc, loc_data
        FROM items
        WHERE owner_id = ?
        ORDER BY loc, object_id
        "#,
    )
    .bind(char_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn character_vitals(
    pool: &MySqlPool,
    char_id: u32,
) -> Result<(i64, i64, i64, i64)> {
    let row: (i64, i64, i64, i64) = sqlx::query_as(
        r#"
        SELECT CAST(COALESCE(curHp, 0) AS SIGNED), CAST(COALESCE(maxHp, 0) AS SIGNED),
               CAST(COALESCE(curMp, 0) AS SIGNED), CAST(COALESCE(maxMp, 0) AS SIGNED)
        FROM characters WHERE charId = ?
        "#,
    )
    .bind(char_id)
    .fetch_one(pool)
    .await?;
    Ok(row)
}

pub async fn character_skill_level(
    pool: &MySqlPool,
    char_id: u32,
    skill_id: i32,
) -> Result<Option<i32>> {
    let row: Option<(i64,)> = sqlx::query_as(
        "SELECT CAST(skill_level AS SIGNED) FROM character_skills WHERE charId = ? AND skill_id = ? LIMIT 1",
    )
    .bind(char_id)
    .bind(skill_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(v,)| v as i32))
}

pub async fn character_recipe_ids(pool: &MySqlPool, char_id: u32) -> Result<Vec<i32>> {
    let rows: Vec<(i64,)> = sqlx::query_as(
        "SELECT CAST(id AS SIGNED) FROM character_recipebook WHERE charId = ?",
    )
    .bind(char_id)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(v,)| v as i32).collect())
}

pub async fn object_item_map(
    pool: &MySqlPool,
    char_id: u32,
) -> Result<std::collections::HashMap<i32, i32>> {
    let rows: Vec<(i32, Option<i32>)> =
        sqlx::query_as("SELECT object_id, item_id FROM items WHERE owner_id = ?")
            .bind(char_id)
            .fetch_all(pool)
            .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(oid, item)| item.map(|i| (oid, i)))
        .collect())
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PunishmentRow {
    pub id: i64,
    pub affect: String,
    pub kind: String,
    pub expiration: i64,
    pub reason: String,
    pub punished_by: String,
}

pub async fn account_punishments(pool: &MySqlPool, login: &str) -> Result<Vec<PunishmentRow>> {
    let rows = sqlx::query_as::<_, PunishmentRow>(
        "SELECT id, affect, type AS kind, expiration, reason, punishedBy AS punished_by FROM punishments WHERE `key` = ? ORDER BY id DESC",
    )
    .bind(login)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn portal_characters(pool: &MySqlPool, login: &str) -> Result<Vec<CharacterRow>> {
    let rows = sqlx::query_as::<_, CharacterRow>(
        r#"
        SELECT c.charId AS char_id, c.char_name, c.account_name, c.level, c.classid,
               c.race, c.clanid AS clan_id, cd.clan_name, c.online, c.pvpkills,
               c.pkkills, c.karma, c.title, c.accesslevel, c.lastAccess AS last_access,
               c.createDate AS create_date, c.deletetime, c.nobless, c.exp, c.sp,
               c.vitality_points, c.x, c.y, c.z
        FROM characters c
        LEFT JOIN clan_data cd ON cd.clan_id = c.clanid
        WHERE c.account_name = ? AND c.deletetime = 0
        ORDER BY c.lastAccess DESC
        "#,
    )
    .bind(login)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn clan_leader(pool: &MySqlPool, clan_id: u32) -> Result<Option<u32>> {
    if clan_id == 0 {
        return Ok(None);
    }
    let row: Option<(i64,)> =
        sqlx::query_as("SELECT leader_id FROM clan_data WHERE clan_id = ? LIMIT 1")
            .bind(clan_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.and_then(|(leader,)| {
        if leader > 0 {
            Some(leader as u32)
        } else {
            None
        }
    }))
}

pub async fn items_in_loc(pool: &MySqlPool, owner_id: i64, loc: &str) -> Result<Vec<ItemRow>> {
    let rows = sqlx::query_as::<_, ItemRow>(
        r#"
        SELECT owner_id, object_id, item_id, count, enchant_level, loc, loc_data
        FROM items
        WHERE owner_id = ? AND loc = ?
        ORDER BY loc_data, object_id
        "#,
    )
    .bind(owner_id)
    .bind(loc)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn item_owners(pool: &MySqlPool, item_id: i32) -> Result<Vec<ItemOwnerRow>> {
    let rows = sqlx::query_as::<_, ItemOwnerRow>(
        r#"
        SELECT i.item_id, i.count, i.enchant_level, i.loc, i.loc_data,
               c.char_name, c.account_name, c.charId AS char_id
        FROM items i
        LEFT JOIN characters c ON c.charId = i.owner_id
        WHERE i.item_id = ?
        ORDER BY i.count DESC
        LIMIT 500
        "#,
    )
    .bind(item_id)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
