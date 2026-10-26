use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use sqlx::MySqlPool;
use std::collections::BTreeMap;

use crate::inventory::MoveError;

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum UndoStmt {
    Update {
        table: String,
        key: BTreeMap<String, Json>,
        set: BTreeMap<String, Json>,
    },
    Insert {
        table: String,
        values: BTreeMap<String, Json>,
    },
    Delete {
        table: String,
        key: BTreeMap<String, Json>,
    },
}

fn allowed_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        "characters" => Some(&[
            "charId",
            "level",
            "exp",
            "sp",
            "sex",
            "face",
            "hairStyle",
            "hairColor",
            "karma",
            "nobless",
            "title",
            "char_name",
            "deletetime",
            "accesslevel",
            "pvpkills",
            "pkkills",
        ]),
        "items" => Some(&[
            "object_id",
            "owner_id",
            "item_id",
            "count",
            "enchant_level",
            "loc",
            "loc_data",
        ]),
        "punishments" => Some(&["id", "key", "affect", "type", "expiration", "reason", "punishedBy"]),
        "account_data" => Some(&["account_name", "var", "value"]),
        _ => None,
    }
}

fn validate_stmt(stmt: &UndoStmt) -> Result<()> {
    match stmt {
        UndoStmt::Update { table, key, set } => {
            let cols = allowed_columns(table).ok_or_else(|| anyhow::anyhow!("table not allowed: {table}"))?;
            for c in key.keys().chain(set.keys()) {
                if !cols.contains(&c.as_str()) {
                    bail!("column not allowed: {table}.{c}");
                }
            }
        }
        UndoStmt::Insert { table, values } => {
            let cols = allowed_columns(table).ok_or_else(|| anyhow::anyhow!("table not allowed: {table}"))?;
            for c in values.keys() {
                if !cols.contains(&c.as_str()) {
                    bail!("column not allowed: {table}.{c}");
                }
            }
        }
        UndoStmt::Delete { table, key } => {
            let cols = allowed_columns(table).ok_or_else(|| anyhow::anyhow!("table not allowed: {table}"))?;
            for c in key.keys() {
                if !cols.contains(&c.as_str()) {
                    bail!("column not allowed: {table}.{c}");
                }
            }
        }
    }
    Ok(())
}

enum Owned {
    I(i64),
    OI(Option<i64>),
    S(String),
}

fn to_owned_bind(v: &Json) -> Result<Owned> {
    Ok(match v {
        Json::Number(n) => {
            if let Some(i) = n.as_i64() {
                Owned::I(i)
            } else if let Some(f) = n.as_f64() {
                Owned::I(f as i64)
            } else {
                bail!("unsupported number");
            }
        }
        Json::String(s) => Owned::S(s.clone()),
        Json::Bool(b) => Owned::I(if *b { 1 } else { 0 }),
        Json::Null => Owned::OI(None),
        _ => bail!("unsupported value type"),
    })
}

macro_rules! bind_all {
    ($q:expr, $vals:expr) => {
        {
            let mut q = $q;
            for v in $vals {
                match v {
                    Owned::I(i) => q = q.bind(i),
                    Owned::OI(i) => q = q.bind(i),
                    Owned::S(s) => q = q.bind(s),
                }
            }
            q
        }
    };
}

async fn execute_stmt(
    tx: &mut sqlx::MySqlConnection,
    stmt: &UndoStmt,
) -> Result<()> {
    validate_stmt(stmt)?;
    match stmt {
        UndoStmt::Update { table, key, set } => {
            let set_sql: Vec<String> = set.keys().map(|c| format!("`{c}` = ?")).collect();
            let key_sql: Vec<String> = key.keys().map(|c| format!("`{c}` = ?")).collect();
            let sql = format!(
                "UPDATE `{table}` SET {} WHERE {}",
                set_sql.join(", "),
                key_sql.join(" AND ")
            );
            let owned: Vec<Owned> = set
                .values()
                .chain(key.values())
                .map(to_owned_bind)
                .collect::<Result<_>>()?;
            let q = bind_all!(sqlx::query(&sql), &owned);
            q.execute(&mut *tx).await?;
        }
        UndoStmt::Insert { table, values } => {
            let cols: Vec<String> = values.keys().map(|c| format!("`{c}`")).collect();
            let marks: Vec<&str> = values.keys().map(|_| "?").collect();
            let sql = format!(
                "INSERT INTO `{table}` ({}) VALUES ({})",
                cols.join(", "),
                marks.join(", ")
            );
            let owned: Vec<Owned> = values.values().map(to_owned_bind).collect::<Result<_>>()?;
            let q = bind_all!(sqlx::query(&sql), &owned);
            q.execute(&mut *tx).await?;
        }
        UndoStmt::Delete { table, key } => {
            let key_sql: Vec<String> = key.keys().map(|c| format!("`{c}` = ?")).collect();
            let sql = format!("DELETE FROM `{table}` WHERE {}", key_sql.join(" AND "));
            let owned: Vec<Owned> = key.values().map(to_owned_bind).collect::<Result<_>>()?;
            let q = bind_all!(sqlx::query(&sql), &owned);
            q.execute(&mut *tx).await?;
        }
    }
    Ok(())
}

pub async fn ensure_schema(pool: &MySqlPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_admin_ops (
            id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            performed_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            actor VARCHAR(45) NOT NULL,
            action VARCHAR(40) NOT NULL,
            target VARCHAR(80) NOT NULL,
            summary VARCHAR(255) NOT NULL,
            undo_json TEXT NOT NULL,
            undone TINYINT NOT NULL DEFAULT 0,
            undone_at TIMESTAMP NULL DEFAULT NULL,
            undone_by VARCHAR(45) NULL,
            INDEX l2o_admin_ops_time (performed_at)
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_op(
    tx: &mut sqlx::MySqlConnection,
    actor: &str,
    action: &str,
    target: &str,
    summary: &str,
    undo: &[UndoStmt],
) -> Result<i64> {
    let undo_json = serde_json::to_string(undo)?;
    let res = sqlx::query(
        "INSERT INTO l2o_admin_ops (actor, action, target, summary, undo_json) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(actor)
    .bind(action)
    .bind(target)
    .bind(summary)
    .bind(undo_json)
    .execute(&mut *tx)
    .await?;
    Ok(res.last_insert_id() as i64)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AdminOpRow {
    pub id: i64,
    pub performed_at: Option<chrono::NaiveDateTime>,
    pub actor: String,
    pub action: String,
    pub target: String,
    pub summary: String,
    pub undone: i8,
    pub undone_at: Option<chrono::NaiveDateTime>,
    pub undone_by: Option<String>,
}

pub async fn recent_ops(pool: &MySqlPool, limit: i64) -> Result<Vec<AdminOpRow>> {
    let rows = sqlx::query_as::<_, AdminOpRow>(
        r#"
        SELECT id, CAST(performed_at AS DATETIME) AS performed_at, actor, action, target, summary,
               undone, CAST(undone_at AS DATETIME) AS undone_at, undone_by
        FROM l2o_admin_ops
        ORDER BY id DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

const PERM_BAN_MS: i64 = 253_402_300_799_000;

pub async fn ban_account(
    pool: &MySqlPool,
    actor: &str,
    login: &str,
    duration_ms: Option<i64>,
    reason: &str,
) -> Result<(), MoveError> {
    let mut tx = pool.begin().await?;

    let exists: Option<(i32,)> = sqlx::query_as("SELECT accessLevel FROM accounts WHERE login = ?")
        .bind(login)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Err(MoveError::ItemNotFound);
    }

    let end = duration_ms.map(|d| chrono::Utc::now().timestamp_millis() + d).unwrap_or(0);
    let ban_temp_end = if end == 0 { PERM_BAN_MS } else { end };

    let res = sqlx::query(
        "INSERT INTO punishments (`key`, `affect`, `type`, `expiration`, `reason`, `punishedBy`) VALUES (?, 'ACCOUNT', 'BAN', ?, ?, ?)",
    )
    .bind(login)
    .bind(end)
    .bind(reason)
    .bind(actor)
    .execute(&mut *tx)
    .await?;
    let punishment_id = res.last_insert_id() as i64;

    let prior: Option<(String,)> = sqlx::query_as(
        "SELECT CAST(value AS CHAR) FROM account_data WHERE account_name = ? AND var = 'ban_temp' LIMIT 1",
    )
    .bind(login)
    .fetch_optional(&mut *tx)
    .await?;

    sqlx::query("INSERT INTO account_data (account_name, var, value) VALUES (?, 'ban_temp', ?) ON DUPLICATE KEY UPDATE value = VALUES(value)")
        .bind(login)
        .bind(ban_temp_end.to_string())
        .execute(&mut *tx)
        .await?;

    let mut undo = vec![UndoStmt::Delete {
        table: "punishments".to_string(),
        key: [("id".to_string(), Json::from(punishment_id))].into_iter().collect(),
    }];
    match prior {
        Some((old,)) => {
            undo.push(UndoStmt::Update {
                table: "account_data".to_string(),
                key: [
                    ("account_name".to_string(), Json::from(login)),
                    ("var".to_string(), Json::from("ban_temp")),
                ]
                .into_iter()
                .collect(),
                set: [("value".to_string(), Json::from(old))].into_iter().collect(),
            });
        }
        None => {
            undo.push(UndoStmt::Delete {
                table: "account_data".to_string(),
                key: [
                    ("account_name".to_string(), Json::from(login)),
                    ("var".to_string(), Json::from("ban_temp")),
                ]
                .into_iter()
                .collect(),
            });
        }
    }

    let summary = if let Some(d) = duration_ms {
        format!(
            "Banned {} for {} ({}): {}",
            login,
            format_duration(d),
            actor,
            reason
        )
    } else {
        format!("Permanently banned {} ({}): {}", login, actor, reason)
    };
    insert_op(&mut tx, actor, "ban", login, &summary, &undo).await?;

    tx.commit().await?;
    Ok(())
}

fn format_duration(ms: i64) -> String {
    let hours = ms / 3_600_000;
    if hours >= 24 {
        format!("{}d", hours / 24)
    } else if hours >= 1 {
        format!("{}h", hours)
    } else {
        format!("{}m", ms / 60_000)
    }
}

pub async fn unban_account(pool: &MySqlPool, actor: &str, login: &str) -> Result<(), MoveError> {
    let mut tx = pool.begin().await?;

    let rows: Vec<(i64, String, String, i64, String, String)> = sqlx::query_as(
        "SELECT CAST(id AS SIGNED), `key`, affect, expiration, reason, punishedBy FROM punishments WHERE `key` = ? AND type = 'BAN'",
    )
    .bind(login)
    .fetch_all(&mut *tx)
    .await?;

    let prior: Option<(String,)> = sqlx::query_as(
        "SELECT CAST(value AS CHAR) FROM account_data WHERE account_name = ? AND var = 'ban_temp' LIMIT 1",
    )
    .bind(login)
    .fetch_optional(&mut *tx)
    .await?;

    if rows.is_empty() && prior.is_none() {
        return Err(MoveError::NotAllowed);
    }

    let mut undo: Vec<UndoStmt> = Vec::new();
    for (id, key, affect, expiration, reason, punished_by) in &rows {
        let mut values: BTreeMap<String, Json> = BTreeMap::new();
        values.insert("id".into(), Json::from(*id));
        values.insert("key".into(), Json::from(key.clone()));
        values.insert("affect".into(), Json::from(affect.clone()));
        values.insert("type".into(), Json::from("BAN"));
        values.insert("expiration".into(), Json::from(*expiration));
        values.insert("reason".into(), Json::from(reason.clone()));
        values.insert("punishedBy".into(), Json::from(punished_by.clone()));
        undo.push(UndoStmt::Insert {
            table: "punishments".to_string(),
            values,
        });
    }
    if let Some((value,)) = prior {
        let mut values: BTreeMap<String, Json> = BTreeMap::new();
        values.insert("account_name".into(), Json::from(login));
        values.insert("var".into(), Json::from("ban_temp"));
        values.insert("value".into(), Json::from(value));
        undo.push(UndoStmt::Insert {
            table: "account_data".to_string(),
            values,
        });
        sqlx::query("DELETE FROM account_data WHERE account_name = ? AND var = 'ban_temp'")
            .bind(login)
            .execute(&mut *tx)
            .await?;
    }
    if !rows.is_empty() {
        sqlx::query("DELETE FROM punishments WHERE `key` = ? AND type = 'BAN'")
            .bind(login)
            .execute(&mut *tx)
            .await?;
    }

    let summary = format!("Unbanned {} ({}), removed {} ban entries", login, actor, rows.len());
    insert_op(&mut tx, actor, "unban", login, &summary, &undo).await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Default)]
pub struct CharacterChanges {
    pub char_name: Option<String>,
    pub level: Option<i64>,
    pub exp: Option<i64>,
    pub sp: Option<i64>,
    pub sex: Option<i64>,
    pub face: Option<i64>,
    pub hair_style: Option<i64>,
    pub hair_color: Option<i64>,
    pub karma: Option<i64>,
    pub nobless: Option<bool>,
    pub title: Option<String>,
    pub access_level: Option<i64>,
}

impl CharacterChanges {
    pub fn is_empty(&self) -> bool {
        self.char_name.is_none()
            && self.level.is_none()
            && self.exp.is_none()
            && self.sp.is_none()
            && self.sex.is_none()
            && self.face.is_none()
            && self.hair_style.is_none()
            && self.hair_color.is_none()
            && self.karma.is_none()
            && self.nobless.is_none()
            && self.title.is_none()
            && self.access_level.is_none()
    }
}

pub async fn update_character(
    pool: &MySqlPool,
    actor: &str,
    char_id: u32,
    changes: &CharacterChanges,
) -> Result<(), MoveError> {
    if changes.is_empty() {
        return Err(MoveError::NotAllowed);
    }
    if let Some(level) = changes.level {
        if !(1..=100).contains(&level) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.sex {
        if !(0..=1).contains(&v) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.face {
        if !(0..=10).contains(&v) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.hair_style {
        if !(0..=10).contains(&v) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.hair_color {
        if !(0..=10).contains(&v) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.karma {
        if v < 0 {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(v) = changes.access_level {
        if !(-100..=127).contains(&v) {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(title) = &changes.title {
        if title.chars().count() > 21 {
            return Err(MoveError::NotAllowed);
        }
    }
    if let Some(name) = &changes.char_name {
        let len = name.chars().count();
        if !(1..=16).contains(&len)
            || !name.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(MoveError::ValueInvalid);
        }
    }

    let mut tx = pool.begin().await?;

    let existing: Option<(String,)> =
        sqlx::query_as("SELECT char_name FROM characters WHERE charId = ? FOR UPDATE")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((current_name,)) = existing else {
        return Err(MoveError::CharacterNotFound);
    };

    if let Some(name) = &changes.char_name {
        if !name.eq_ignore_ascii_case(&current_name) {
            let taken: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM characters WHERE char_name = ? AND charId <> ?",
            )
            .bind(name)
            .bind(char_id)
            .fetch_one(&mut *tx)
            .await?;
            if taken.0 > 0 {
                return Err(MoveError::NameTaken);
            }
        }
    }

    let mut sets: Vec<String> = Vec::new();
    let mut binds_set: Vec<Owned> = Vec::new();
    let mut undo_set: BTreeMap<String, Json> = BTreeMap::new();

    let old: Option<(String, i64, i64, i64, i64, i64, i64, i64, i64, i64, String, i64)> = sqlx::query_as(
        "SELECT char_name, CAST(COALESCE(level,1) AS SIGNED), CAST(COALESCE(exp,0) AS SIGNED), CAST(COALESCE(sp,0) AS SIGNED), CAST(COALESCE(sex,0) AS SIGNED), CAST(COALESCE(face,0) AS SIGNED), CAST(COALESCE(hairStyle,0) AS SIGNED), CAST(COALESCE(hairColor,0) AS SIGNED), CAST(COALESCE(karma,0) AS SIGNED), CAST(COALESCE(nobless,0) AS SIGNED), COALESCE(title,''), CAST(COALESCE(accesslevel,0) AS SIGNED) FROM characters WHERE charId = ?",
    )
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((o_name, o_level, o_exp, o_sp, o_sex, o_face, o_hair, o_hairc, o_karma, o_nobless, o_title, o_access)) = old else {
        return Err(MoveError::CharacterNotFound);
    };

    macro_rules! add_change {
        ($field:expr, $col:expr, $old:expr, $owned:expr) => {
            if let Some(v) = &$field {
                sets.push(format!("`{}` = ?", $col));
                binds_set.push($owned(v));
                undo_set.insert($col.to_string(), $old);
            }
        };
    }

    add_change!(changes.char_name, "char_name", Json::from(o_name.clone()), |v: &String| Owned::S(v.clone()));
    add_change!(changes.level, "level", Json::from(o_level), |v: &i64| Owned::I(*v));
    add_change!(changes.exp, "exp", Json::from(o_exp.max(0) as u64), |v: &i64| Owned::I(*v));
    add_change!(changes.sp, "sp", Json::from(o_sp.max(0) as u64), |v: &i64| Owned::I(*v));
    add_change!(changes.sex, "sex", Json::from(o_sex), |v: &i64| Owned::I(*v));
    add_change!(changes.face, "face", Json::from(o_face), |v: &i64| Owned::I(*v));
    add_change!(changes.hair_style, "hairStyle", Json::from(o_hair), |v: &i64| Owned::I(*v));
    add_change!(changes.hair_color, "hairColor", Json::from(o_hairc), |v: &i64| Owned::I(*v));
    add_change!(changes.karma, "karma", Json::from(o_karma.max(0) as u64), |v: &i64| Owned::I(*v));
    add_change!(changes.nobless, "nobless", Json::from(o_nobless), |v: &bool| Owned::I(if *v { 1 } else { 0 }));
    add_change!(changes.title, "title", Json::from(o_title.clone()), |v: &String| Owned::S(v.clone()));
    add_change!(changes.access_level, "accesslevel", Json::from(o_access), |v: &i64| Owned::I(*v));

    if sets.is_empty() {
        return Err(MoveError::NotAllowed);
    }

    let sql = format!(
        "UPDATE characters SET {} WHERE charId = ?",
        sets.join(", ")
    );
    binds_set.push(Owned::I(char_id as i64));
    let q = bind_all!(sqlx::query(&sql), &binds_set);
    q.execute(&mut *tx).await?;

    let mut summary_parts: Vec<String> = Vec::new();
    for (col, v) in &undo_set {
        summary_parts.push(format!("{col}={}", json_short(v)));
    }
    let summary = format!(
        "Edited character {} [{}]: {}",
        o_name,
        char_id,
        summary_parts.join(", ")
    );

    let undo = vec![UndoStmt::Update {
        table: "characters".to_string(),
        key: [("charId".to_string(), Json::from(char_id as i64))]
            .into_iter()
            .collect(),
        set: undo_set,
    }];
    insert_op(&mut tx, actor, "char_edit", &char_id.to_string(), &summary, &undo).await?;

    tx.commit().await?;
    Ok(())
}

fn json_short(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub async fn restore_character(pool: &MySqlPool, actor: &str, char_id: u32) -> Result<(), MoveError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, u64)> =
        sqlx::query_as("SELECT char_name, deletetime FROM characters WHERE charId = ? FOR UPDATE")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((name, deletetime)) = row else {
        return Err(MoveError::CharacterNotFound);
    };
    if deletetime == 0 {
        return Err(MoveError::NotAllowed);
    }
    sqlx::query("UPDATE characters SET deletetime = 0 WHERE charId = ?")
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    let undo = vec![UndoStmt::Update {
        table: "characters".to_string(),
        key: [("charId".to_string(), Json::from(char_id as i64))].into_iter().collect(),
        set: [("deletetime".to_string(), Json::from(deletetime as i64))].into_iter().collect(),
    }];
    let summary = format!("Restored character {} [{}] from pending deletion", name, char_id);
    insert_op(&mut tx, actor, "char_restore", &char_id.to_string(), &summary, &undo).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn grant_item(
    pool: &MySqlPool,
    actor: &str,
    char_id: u32,
    item_id: i32,
    count: u64,
    enchant: i64,
    stackable: bool,
) -> Result<(), MoveError> {
    if count == 0 {
        return Err(MoveError::NotAllowed);
    }
    if !stackable && count > 1 {
        return Err(MoveError::NotAllowed);
    }
    let mut tx = pool.begin().await?;
    let ch: Option<(u8, String)> =
        sqlx::query_as("SELECT online, char_name FROM characters WHERE charId = ? FOR UPDATE")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((online, name)) = ch else {
        return Err(MoveError::CharacterNotFound);
    };
    if online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let slots: Vec<(i32,)> =
        sqlx::query_as("SELECT loc_data FROM items WHERE owner_id = ? AND loc = 'INVENTORY'")
            .bind(char_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut used: Vec<i64> = slots.into_iter().map(|(v,)| v.max(0) as i64).collect();
    used.sort_unstable();
    used.dedup();
    let mut slot = 0i64;
    for u in used {
        if u == slot {
            slot += 1;
        } else if u > slot {
            break;
        }
    }

    let res = sqlx::query("INSERT INTO l2o_object_ids () VALUES ()")
        .execute(&mut *tx)
        .await?;
    let object_id = 2_000_000_000i64 + res.last_insert_id() as i64;

    sqlx::query(
        "INSERT INTO items (object_id, owner_id, item_id, count, enchant_level, loc, loc_data) VALUES (?, ?, ?, ?, ?, 'INVENTORY', ?)",
    )
    .bind(object_id as i32)
    .bind(char_id)
    .bind(item_id)
    .bind(count)
    .bind(enchant)
    .bind(slot)
    .execute(&mut *tx)
    .await?;

    let undo = vec![UndoStmt::Delete {
        table: "items".to_string(),
        key: [("object_id".to_string(), Json::from(object_id))].into_iter().collect(),
    }];
    let summary = format!(
        "Granted item {} x{} (+{}) to {} [{}]",
        item_id, count, enchant, name, char_id
    );
    insert_op(&mut tx, actor, "grant_item", &char_id.to_string(), &summary, &undo).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn remove_item(
    pool: &MySqlPool,
    actor: &str,
    char_id: u32,
    object_id: i32,
) -> Result<(), MoveError> {
    let mut tx = pool.begin().await?;
    let ch: Option<(u8, String)> =
        sqlx::query_as("SELECT online, char_name FROM characters WHERE charId = ? FOR UPDATE")
            .bind(char_id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some((online, name)) = ch else {
        return Err(MoveError::CharacterNotFound);
    };
    if online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let row: Option<(Option<i32>, u64, Option<i32>, Option<String>, Option<i32>)> = sqlx::query_as(
        "SELECT item_id, count, enchant_level, loc, loc_data FROM items WHERE object_id = ? AND owner_id = ? LIMIT 1 FOR UPDATE",
    )
    .bind(object_id)
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((item_id, count, enchant, loc, loc_data)) = row else {
        return Err(MoveError::ItemNotFound);
    };
    let loc = loc.unwrap_or_else(|| "INVENTORY".to_string());
    if loc == "PAPERDOLL" {
        return Err(MoveError::NotAllowed);
    }

    sqlx::query("DELETE FROM items WHERE object_id = ?")
        .bind(object_id)
        .execute(&mut *tx)
        .await?;
    let _ = sqlx::query("DELETE FROM item_variables WHERE id = ?")
        .bind(object_id)
        .execute(&mut *tx)
        .await;

    let mut values: BTreeMap<String, Json> = BTreeMap::new();
    values.insert("object_id".into(), Json::from(object_id as i64));
    values.insert("owner_id".into(), Json::from(char_id as i64));
    values.insert("item_id".into(), Json::from(item_id.unwrap_or(0) as i64));
    values.insert("count".into(), Json::from(count));
    values.insert("enchant_level".into(), Json::from(enchant.unwrap_or(0) as i64));
    values.insert("loc".into(), Json::from(loc.clone()));
    values.insert("loc_data".into(), Json::from(loc_data.unwrap_or(0) as i64));
    let undo = vec![UndoStmt::Insert {
        table: "items".to_string(),
        values,
    }];
    let summary = format!(
        "Removed item {} x{} from {} [{}] ({})",
        item_id.unwrap_or(0),
        count,
        name,
        char_id,
        loc
    );
    insert_op(&mut tx, actor, "remove_item", &char_id.to_string(), &summary, &undo).await?;
    tx.commit().await?;
    Ok(())
}

pub async fn undo_op(pool: &MySqlPool, actor: &str, op_id: i64) -> Result<String, MoveError> {
    let mut tx = pool.begin().await?;
    let row: Option<(String, String, i8)> = sqlx::query_as(
        "SELECT summary, undo_json, undone FROM l2o_admin_ops WHERE id = ? FOR UPDATE",
    )
    .bind(op_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((summary, undo_json, undone)) = row else {
        return Err(MoveError::ItemNotFound);
    };
    if undone == 1 {
        return Err(MoveError::Conflict);
    }
    let stmts: Vec<UndoStmt> = serde_json::from_str(&undo_json)
        .map_err(|e| MoveError::Db(anyhow::anyhow!("invalid undo data: {e}")))?;

    for stmt in &stmts {
        execute_stmt(&mut tx, stmt)
            .await
            .map_err(MoveError::Db)?;
    }

    sqlx::query(
        "UPDATE l2o_admin_ops SET undone = 1, undone_at = CURRENT_TIMESTAMP, undone_by = ? WHERE id = ?",
    )
    .bind(actor)
    .bind(op_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(summary)
}
