use anyhow::Result;
use sqlx::MySqlPool;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct GameLimits {
    pub inv_no_dwarf: i64,
    pub inv_dwarf: i64,
    pub inv_gm: i64,
    pub wh_no_dwarf: i64,
    pub wh_dwarf: i64,
    pub wh_clan: i64,
    pub members_can_withdraw_from_clanwh: bool,
    pub offline_trade_enabled: bool,
    pub offline_craft_enabled: bool,
    pub restore_offliners: bool,
}

impl Default for GameLimits {
    fn default() -> Self {
        Self {
            inv_no_dwarf: 80,
            inv_dwarf: 100,
            inv_gm: 250,
            wh_no_dwarf: 100,
            wh_dwarf: 120,
            wh_clan: 200,
            members_can_withdraw_from_clanwh: false,
            offline_trade_enabled: false,
            offline_craft_enabled: false,
            restore_offliners: false,
        }
    }
}

impl GameLimits {
    pub fn load(player_ini: &Path, offline_trade_ini: &Path) -> Self {
        let mut limits = Self::default();
        if let Ok(text) = std::fs::read_to_string(player_ini) {
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim();
                let value = value.trim();
                let int_val = || value.parse::<i64>().ok();
                match key {
                    "MaximumSlotsForNoDwarf" => limits.inv_no_dwarf = int_val().unwrap_or(limits.inv_no_dwarf),
                    "MaximumSlotsForDwarf" => limits.inv_dwarf = int_val().unwrap_or(limits.inv_dwarf),
                    "MaximumSlotsForGMPlayer" => limits.inv_gm = int_val().unwrap_or(limits.inv_gm),
                    "MaximumWarehouseSlotsForNoDwarf" => limits.wh_no_dwarf = int_val().unwrap_or(limits.wh_no_dwarf),
                    "MaximumWarehouseSlotsForDwarf" => limits.wh_dwarf = int_val().unwrap_or(limits.wh_dwarf),
                    "MaximumWarehouseSlotsForClan" => limits.wh_clan = int_val().unwrap_or(limits.wh_clan),
                    "AltMembersCanWithdrawFromClanWH" => {
                        limits.members_can_withdraw_from_clanwh = value.eq_ignore_ascii_case("true")
                    }
                    _ => {}
                }
            }
        } else {
            tracing::warn!("could not read {:?}, using default limits", player_ini);
        }
        if let Ok(text) = std::fs::read_to_string(offline_trade_ini) {
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let value = value.trim();
                match key.trim() {
                    "OfflineTradeEnable" => limits.offline_trade_enabled = value.eq_ignore_ascii_case("true"),
                    "OfflineCraftEnable" => limits.offline_craft_enabled = value.eq_ignore_ascii_case("true"),
                    "RestoreOffliners" => limits.restore_offliners = value.eq_ignore_ascii_case("true"),
                    _ => {}
                }
            }
        } else {
            tracing::warn!(
                "could not read {:?}, offline shops disabled",
                offline_trade_ini
            );
        }
        limits
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MoveDirection {
    InventoryToWarehouse,
    WarehouseToInventory,
    InventoryToClan,
    ClanToInventory,
}

#[derive(Debug)]
pub struct MoveOutcome {
    pub item_id: i32,
    pub count: u64,
    pub from_loc: &'static str,
    pub to_loc: &'static str,
}

#[derive(Debug)]
pub enum MoveError {
    CharacterNotFound,
    CharacterOnline,
    NotInClan,
    NotClanLeader,
    ItemNotFound,
    StorageFull,
    Conflict,
    NotCrystallizable,
    NotAllowed,
    RecipeNotKnown,
    SkillTooLow,
    IngredientsMissing,
    NotEnoughMana,
    NotEnoughHp,
    NotSellable,
    ShopsDisabled,
    NameTaken,
    ValueInvalid,
    Db(anyhow::Error),
}

impl std::fmt::Display for MoveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MoveError::CharacterNotFound => write!(f, "character not found"),
            MoveError::CharacterOnline => write!(f, "character is online in game"),
            MoveError::NotInClan => write!(f, "character is not in a clan"),
            MoveError::NotClanLeader => write!(f, "only the clan leader can withdraw from the clan warehouse"),
            MoveError::ItemNotFound => write!(f, "item not found"),
            MoveError::StorageFull => write!(f, "storage is full"),
            MoveError::Conflict => write!(f, "item state changed, please retry"),
            MoveError::NotCrystallizable => write!(f, "item cannot be crystallized"),
            MoveError::NotAllowed => write!(f, "operation not allowed for this item"),
            MoveError::RecipeNotKnown => write!(f, "recipe is not in the character's recipe book"),
            MoveError::SkillTooLow => write!(f, "create item skill level too low"),
            MoveError::IngredientsMissing => write!(f, "missing ingredients"),
            MoveError::NotEnoughMana => write!(f, "not enough MP"),
            MoveError::NotEnoughHp => write!(f, "not enough HP"),
            MoveError::NotSellable => write!(f, "item cannot be sold"),
            MoveError::ShopsDisabled => write!(f, "offline shops are disabled on this server"),
            MoveError::NameTaken => write!(f, "character name is already taken"),
            MoveError::ValueInvalid => write!(f, "invalid value"),
            MoveError::Db(e) => write!(f, "database error: {e:#}"),
        }
    }
}

impl From<sqlx::Error> for MoveError {
    fn from(e: sqlx::Error) -> Self {
        MoveError::Db(e.into())
    }
}

impl From<anyhow::Error> for MoveError {
    fn from(e: anyhow::Error) -> Self {
        MoveError::Db(e)
    }
}

pub async fn ensure_schema(pool: &MySqlPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_item_moves (
            id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            moved_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            actor_login VARCHAR(45) NOT NULL,
            char_id INT UNSIGNED NOT NULL,
            object_id INT NOT NULL,
            item_id INT NOT NULL,
            count BIGINT UNSIGNED NOT NULL,
            enchant_level INT NOT NULL DEFAULT 0,
            from_loc VARCHAR(10) NOT NULL,
            to_loc VARCHAR(10) NOT NULL,
            INDEX l2o_item_moves_char (char_id),
            INDEX l2o_item_moves_time (moved_at)
        )
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_object_ids (
            id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_crafts (
            id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            crafted_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
            actor_login VARCHAR(45) NOT NULL,
            char_id INT UNSIGNED NOT NULL,
            recipe_id INT NOT NULL,
            recipe_name VARCHAR(100) NOT NULL,
            product_id INT NOT NULL,
            product_count BIGINT NOT NULL,
            rare TINYINT NOT NULL DEFAULT 0,
            success TINYINT NOT NULL,
            mp_spent BIGINT NOT NULL DEFAULT 0,
            hp_spent BIGINT NOT NULL DEFAULT 0,
            INDEX l2o_crafts_char (char_id),
            INDEX l2o_crafts_time (crafted_at)
        )
        "#,
    )
    .execute(pool)
    .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct CrystalInfo {
    pub crystal_type: String,
    pub base_count: i64,
    pub is_weapon: bool,
}

pub fn crystal_value(
    crystal_type: &str,
    base_count: i64,
    is_weapon: bool,
    enchant: i32,
) -> Option<(i32, i64)> {
    let (crystal_id, bonus_armor, bonus_weapon) = match crystal_type.to_uppercase().as_str() {
        "D" => (1458, 11i64, 90i64),
        "C" => (1459, 6, 45),
        "B" => (1460, 11, 67),
        "A" => (1461, 20, 145),
        "S" | "S80" | "S84" => (1462, 25, 250),
        _ => return None,
    };
    if base_count <= 0 {
        return None;
    }
    let mut amount = base_count;
    if enchant > 3 {
        let e = enchant as i64;
        amount += if is_weapon {
            bonus_weapon * ((2 * e) - 3)
        } else {
            bonus_armor * ((3 * e) - 6)
        };
    }
    Some((crystal_id, amount.max(1)))
}

#[derive(Debug)]
pub struct CrystalOutcome {
    pub item_id: i32,
    pub crystals_item_id: i32,
    pub crystals: i64,
}

#[derive(Debug)]
pub struct DeleteOutcome {
    pub item_id: i32,
    pub count: u64,
    pub from_loc: &'static str,
}

struct CharInfo {
    online: u8,
    clan_id: u32,
    race: u8,
    access_level: i32,
    clan_leader_id: u32,
}

pub const SHOP_TYPE_SELL: i32 = 1;
pub const SHOP_TYPE_BUY: i32 = 3;
pub const SHOP_TYPE_CRAFT: i32 = 5;

async fn alloc_object_id(tx: &mut sqlx::MySqlConnection) -> Result<i64> {
    let res = sqlx::query("INSERT INTO l2o_object_ids () VALUES ()")
        .execute(&mut *tx)
        .await?;
    Ok(2_000_000_000 + res.last_insert_id() as i64)
}

async fn add_item_to(
    tx: &mut sqlx::MySqlConnection,
    owner_id: i64,
    loc: &str,
    item_id: i32,
    count: i64,
    stackable: bool,
) -> Result<()> {
    if stackable {
        if let Some((stack_oid, stack_count)) = find_stack(tx, owner_id, loc, item_id, 0).await? {
            sqlx::query("UPDATE items SET count = ? WHERE object_id = ?")
                .bind(stack_count + count as u64)
                .bind(stack_oid)
                .execute(&mut *tx)
                .await?;
            return Ok(());
        }
    }
    let mut loc_data = 0i64;
    if loc == "INVENTORY" {
        loc_data = first_free_slot(tx, owner_id as u32).await?.unwrap_or(0);
    }
    let object_id = alloc_object_id(tx).await?;
    sqlx::query(
        "INSERT INTO items (object_id, owner_id, item_id, count, enchant_level, loc, loc_data) VALUES (?, ?, ?, ?, 0, ?, ?)",
    )
    .bind(object_id as i32)
    .bind(owner_id)
    .bind(item_id)
    .bind(count as u64)
    .bind(loc)
    .bind(loc_data)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

async fn inventory_counts(
    tx: &mut sqlx::MySqlConnection,
    char_id: u32,
) -> Result<std::collections::HashMap<i32, u64>> {
    let rows: Vec<(i32, i64)> = sqlx::query_as(
        "SELECT item_id, CAST(SUM(count) AS SIGNED) FROM items WHERE owner_id = ? AND loc = 'INVENTORY' AND item_id IS NOT NULL GROUP BY item_id",
    )
    .bind(char_id)
    .fetch_all(&mut *tx)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, count)| (id, count.max(0) as u64))
        .collect())
}

fn ingredients_ok(
    counts: &std::collections::HashMap<i32, u64>,
    ingredients: &[(i32, i64)],
) -> bool {
    ingredients.iter().all(|(id, need)| {
        counts.get(id).copied().unwrap_or(0) >= (*need).max(0) as u64
    })
}

async fn consume_ingredients(
    tx: &mut sqlx::MySqlConnection,
    char_id: u32,
    ingredients: &[(i32, i64)],
) -> Result<(), MoveError> {
    for (item_id, need) in ingredients {
        let mut remaining = (*need).max(0) as u64;
        let rows: Vec<(i32, u64)> = sqlx::query_as(
            "SELECT object_id, count FROM items WHERE owner_id = ? AND loc = 'INVENTORY' AND item_id = ? ORDER BY count DESC FOR UPDATE",
        )
        .bind(char_id)
        .bind(item_id)
        .fetch_all(&mut *tx)
        .await?;
        for (oid, count) in rows {
            if remaining == 0 {
                break;
            }
            let take = count.min(remaining);
            if take == count {
                sqlx::query("DELETE FROM items WHERE object_id = ?")
                    .bind(oid)
                    .execute(&mut *tx)
                    .await?;
                cleanup_item_aux(tx, oid).await?;
            } else {
                sqlx::query("UPDATE items SET count = ? WHERE object_id = ?")
                    .bind(count - take)
                    .bind(oid)
                    .execute(&mut *tx)
                    .await?;
            }
            remaining -= take;
        }
        if remaining > 0 {
            return Err(MoveError::IngredientsMissing);
        }
    }
    Ok(())
}

#[derive(Debug)]
pub struct CraftOutcome {
    pub success: bool,
    pub rare: bool,
    pub product_id: i32,
    pub product_count: i64,
    pub mp_spent: i64,
    pub hp_spent: i64,
}

fn roll_percent(threshold: i32) -> bool {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    if threshold >= 100 {
        return true;
    }
    if threshold <= 0 {
        return false;
    }
    (OsRng.next_u32() % 100) < threshold as u32
}

pub async fn craft_item(
    pool: &MySqlPool,
    recipe: &crate::gamedata::Recipe,
    char_id: u32,
    item_stackable: impl Fn(i32) -> bool,
    actor_login: &str,
) -> Result<CraftOutcome, MoveError> {
    let mut tx = pool.begin().await?;

    let Some(ch) = load_char(&mut tx, char_id).await? else {
        return Err(MoveError::CharacterNotFound);
    };
    if ch.online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let known: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM character_recipebook WHERE charId = ? AND id = ?",
    )
    .bind(char_id)
    .bind(recipe.id)
    .fetch_one(&mut *tx)
    .await?;
    if known.0 == 0 {
        return Err(MoveError::RecipeNotKnown);
    }

    let skill_id: i32 = if recipe.dwarven { 172 } else { 1320 };
    let skill_row: Option<(i64,)> = sqlx::query_as(
        "SELECT CAST(skill_level AS SIGNED) FROM character_skills WHERE charId = ? AND skill_id = ? LIMIT 1",
    )
    .bind(char_id)
    .bind(skill_id)
    .fetch_optional(&mut *tx)
    .await?;
    let skill_level = skill_row.map(|(l,)| l as i32).unwrap_or(0);
    if recipe.level > skill_level {
        return Err(MoveError::SkillTooLow);
    }

    let vitals: (i64, i64) = sqlx::query_as(
        "SELECT CAST(COALESCE(curHp, 0) AS SIGNED), CAST(COALESCE(curMp, 0) AS SIGNED) FROM characters WHERE charId = ?",
    )
    .bind(char_id)
    .fetch_one(&mut *tx)
    .await?;
    let (cur_hp, cur_mp) = vitals;
    if recipe.hp_cost > 0 && cur_hp <= recipe.hp_cost {
        return Err(MoveError::NotEnoughHp);
    }
    if recipe.mp_cost > 0 && cur_mp < recipe.mp_cost {
        return Err(MoveError::NotEnoughMana);
    }

    let counts = inventory_counts(&mut tx, char_id).await?;
    if !ingredients_ok(&counts, &recipe.ingredients) {
        return Err(MoveError::IngredientsMissing);
    }

    consume_ingredients(&mut tx, char_id, &recipe.ingredients).await?;

    if recipe.mp_cost > 0 || recipe.hp_cost > 0 {
        sqlx::query("UPDATE characters SET curHp = curHp - ?, curMp = curMp - ? WHERE charId = ?")
            .bind(recipe.hp_cost)
            .bind(recipe.mp_cost)
            .bind(char_id)
            .execute(&mut *tx)
            .await?;
    }

    let success = roll_percent(recipe.success_rate);
    let mut product_id = recipe.product_id;
    let mut product_count = recipe.product_count;
    let mut rare = false;
    if success {
        if let Some(rare_id) = recipe.rare_item_id {
            if recipe.rarity > 0 && roll_percent(recipe.rarity.min(100) as i32) {
                rare = true;
                product_id = rare_id;
                product_count = recipe.rare_count;
            }
        }
        add_item_to(
            &mut tx,
            char_id as i64,
            "INVENTORY",
            product_id,
            product_count,
            item_stackable(product_id),
        )
        .await?;
    }

    sqlx::query(
        r#"
        INSERT INTO l2o_crafts
            (actor_login, char_id, recipe_id, recipe_name, product_id, product_count, rare, success, mp_spent, hp_spent)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(actor_login)
    .bind(char_id)
    .bind(recipe.id)
    .bind(&recipe.name)
    .bind(product_id)
    .bind(product_count)
    .bind(rare as i32)
    .bind(success as i32)
    .bind(recipe.mp_cost)
    .bind(recipe.hp_cost)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(CraftOutcome {
        success,
        rare,
        product_id,
        product_count,
        mp_spent: recipe.mp_cost,
        hp_spent: recipe.hp_cost,
    })
}

#[derive(Debug)]
pub struct SellOutcome {
    pub sold: i64,
    pub failed: i64,
    pub adena: i64,
    pub last_why: String,
}

pub async fn sell_items(
    pool: &MySqlPool,
    char_id: u32,
    object_ids: &[i32],
    price_of: impl Fn(i32) -> i64,
    rate_percent: i64,
    actor_login: &str,
) -> Result<SellOutcome, MoveError> {
    let mut tx = pool.begin().await?;

    let Some(ch) = load_char(&mut tx, char_id).await? else {
        return Err(MoveError::CharacterNotFound);
    };
    if ch.online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let mut adena_total: i64 = 0;
    let mut sold = 0i64;
    let mut failed = 0i64;
    let mut last_why = String::new();

    for object_id in object_ids {
        let row: Option<(i32, u64)> = sqlx::query_as(
            "SELECT item_id, count FROM items WHERE object_id = ? AND owner_id = ? AND loc = 'INVENTORY' LIMIT 1 FOR UPDATE",
        )
        .bind(object_id)
        .bind(char_id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some((item_id, count)) = row else {
            failed += 1;
            last_why = "item".to_string();
            continue;
        };
        if item_id == 57 {
            failed += 1;
            last_why = "notsellable".to_string();
            continue;
        }
        let price = price_of(item_id);
        if price <= 0 {
            failed += 1;
            last_why = "notsellable".to_string();
            continue;
        }
        let mut gain = (price * count as i64 * rate_percent) / 100;
        if gain <= 0 && rate_percent > 0 {
            gain = 1;
        }

        let deleted = sqlx::query("DELETE FROM items WHERE object_id = ? AND owner_id = ?")
            .bind(object_id)
            .bind(char_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() != 1 {
            failed += 1;
            last_why = "state".to_string();
            continue;
        }
        cleanup_item_aux(&mut tx, *object_id).await?;

        sqlx::query(
            r#"
            INSERT INTO l2o_item_moves
                (actor_login, char_id, object_id, item_id, count, enchant_level, from_loc, to_loc)
            VALUES (?, ?, ?, ?, ?, 0, 'INVENTORY', 'SELL')
            "#,
        )
        .bind(actor_login)
        .bind(char_id)
        .bind(object_id)
        .bind(item_id)
        .bind(count)
        .execute(&mut *tx)
        .await?;

        adena_total += gain;
        sold += 1;
    }

    if adena_total > 0 {
        add_item_to(
            &mut tx,
            char_id as i64,
            "INVENTORY",
            57,
            adena_total,
            true,
        )
        .await?;
    }

    tx.commit().await?;

    Ok(SellOutcome {
        sold,
        failed,
        adena: adena_total,
        last_why,
    })
}

struct ItemInfo {
    item_id: i32,
    count: u64,
    enchant_level: i32,
    stackable: bool,
}

async fn load_char(
    tx: &mut sqlx::MySqlConnection,
    char_id: u32,
) -> Result<Option<CharInfo>> {
    let row: Option<(u8, u32, u8, i32, i64)> = sqlx::query_as(
        r#"
        SELECT c.online, c.clanid, c.race, COALESCE(c.accesslevel, 0),
               COALESCE(cd.leader_id, 0)
        FROM characters c
        LEFT JOIN clan_data cd ON cd.clan_id = c.clanid
        WHERE c.charId = ?
        LIMIT 1
        FOR UPDATE
        "#,
    )
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row.map(|(online, clan_id, race, access_level, clan_leader_id)| CharInfo {
        online,
        clan_id,
        race,
        access_level,
        clan_leader_id: clan_leader_id.max(0) as u32,
    }))
}

async fn count_loc(
    tx: &mut sqlx::MySqlConnection,
    owner_id: i64,
    loc: &str,
) -> Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items WHERE owner_id = ? AND loc = ?")
        .bind(owner_id)
        .bind(loc)
        .fetch_one(&mut *tx)
        .await?;
    Ok(row.0)
}

async fn find_stack(
    tx: &mut sqlx::MySqlConnection,
    owner_id: i64,
    loc: &str,
    item_id: i32,
    exclude_object_id: i32,
) -> Result<Option<(i32, u64)>> {
    let row: Option<(i32, u64)> = sqlx::query_as(
        "SELECT object_id, count FROM items WHERE owner_id = ? AND loc = ? AND item_id = ? AND object_id != ? LIMIT 1 FOR UPDATE",
    )
    .bind(owner_id)
    .bind(loc)
    .bind(item_id)
    .bind(exclude_object_id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(row)
}

async fn first_free_slot(tx: &mut sqlx::MySqlConnection, char_id: u32) -> Result<Option<i64>> {
    let rows: Vec<(i32,)> =
        sqlx::query_as("SELECT loc_data FROM items WHERE owner_id = ? AND loc = 'INVENTORY'")
            .bind(char_id)
            .fetch_all(&mut *tx)
            .await?;
    let mut used: Vec<i64> = rows.into_iter().map(|(v,)| v.max(0) as i64).collect();
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
    Ok(Some(slot))
}

pub async fn move_item(
    pool: &MySqlPool,
    limits: &GameLimits,
    char_id: u32,
    object_id: i32,
    direction: MoveDirection,
    item_stackable: impl Fn(i32) -> bool,
    actor_login: &str,
) -> Result<MoveOutcome, MoveError> {
    let mut tx = pool.begin().await?;

    let Some(ch) = load_char(&mut tx, char_id).await? else {
        return Err(MoveError::CharacterNotFound);
    };
    if ch.online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let (from_loc, expected_owner): (&str, i64) = match direction {
        MoveDirection::InventoryToWarehouse | MoveDirection::InventoryToClan => {
            ("INVENTORY", char_id as i64)
        }
        MoveDirection::WarehouseToInventory => ("WAREHOUSE", char_id as i64),
        MoveDirection::ClanToInventory => ("CLANWH", ch.clan_id as i64),
    };

    let needs_clan = matches!(
        direction,
        MoveDirection::InventoryToClan | MoveDirection::ClanToInventory
    );
    if needs_clan && ch.clan_id == 0 {
        return Err(MoveError::NotInClan);
    }
    if direction == MoveDirection::ClanToInventory
        && !limits.members_can_withdraw_from_clanwh
        && ch.clan_leader_id != char_id
    {
        return Err(MoveError::NotClanLeader);
    }

    let item_row: Option<(i32, u64, i32)> = sqlx::query_as(
        "SELECT item_id, count, COALESCE(enchant_level, 0) FROM items WHERE object_id = ? AND owner_id = ? AND loc = ? LIMIT 1 FOR UPDATE",
    )
    .bind(object_id)
    .bind(expected_owner)
    .bind(from_loc)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((item_id, count, enchant_level)) = item_row else {
        return Err(MoveError::ItemNotFound);
    };
    let stackable = item_stackable(item_id);
    let item = ItemInfo {
        item_id,
        count,
        enchant_level,
        stackable,
    };

    let (to_loc, new_owner, from_loc_static, to_loc_static): (
        &str,
        i64,
        &'static str,
        &'static str,
    ) = match direction {
        MoveDirection::InventoryToWarehouse => ("WAREHOUSE", char_id as i64, "INVENTORY", "WAREHOUSE"),
        MoveDirection::WarehouseToInventory => ("INVENTORY", char_id as i64, "WAREHOUSE", "INVENTORY"),
        MoveDirection::InventoryToClan => ("CLANWH", ch.clan_id as i64, "INVENTORY", "CLANWH"),
        MoveDirection::ClanToInventory => ("INVENTORY", char_id as i64, "CLANWH", "INVENTORY"),
    };

    let merge_target = if item.stackable {
        find_stack(&mut tx, new_owner, to_loc, item.item_id, object_id).await?
    } else {
        None
    };

    let mut new_loc_data: i64 = 0;
    if to_loc == "INVENTORY" {
        if merge_target.is_none() {
            let limit = if ch.access_level > 0 {
                limits.inv_gm
            } else if ch.race == 4 {
                limits.inv_dwarf
            } else {
                limits.inv_no_dwarf
            };
            let slot = first_free_slot(&mut tx, char_id).await?.unwrap_or(0);
            if slot >= limit {
                return Err(MoveError::StorageFull);
            }
            new_loc_data = slot;
        }
    } else if merge_target.is_none() {
        let (used, limit) = if to_loc == "WAREHOUSE" {
            let limit = if ch.race == 4 {
                limits.wh_dwarf
            } else {
                limits.wh_no_dwarf
            };
            (
                count_loc(&mut tx, char_id as i64, "WAREHOUSE").await?,
                limit,
            )
        } else {
            (
                count_loc(&mut tx, ch.clan_id as i64, "CLANWH").await?,
                limits.wh_clan,
            )
        };
        if used + 1 > limit {
            return Err(MoveError::StorageFull);
        }
    }

    if let Some((target_oid, target_count)) = merge_target {
        let new_count = target_count + item.count;
        sqlx::query("UPDATE items SET count = ? WHERE object_id = ?")
            .bind(new_count)
            .bind(target_oid)
            .execute(&mut *tx)
            .await?;
        let deleted =
            sqlx::query("DELETE FROM items WHERE object_id = ? AND owner_id = ? AND loc = ?")
                .bind(object_id)
                .bind(expected_owner)
                .bind(from_loc)
                .execute(&mut *tx)
                .await?;
        if deleted.rows_affected() != 1 {
            return Err(MoveError::Conflict);
        }
        cleanup_item_aux(&mut tx, object_id).await?;
    } else {
        let result = sqlx::query(
            "UPDATE items SET owner_id = ?, loc = ?, loc_data = ? WHERE object_id = ? AND owner_id = ? AND loc = ?",
        )
        .bind(new_owner)
        .bind(to_loc)
        .bind(new_loc_data)
        .bind(object_id)
        .bind(expected_owner)
        .bind(from_loc)
        .execute(&mut *tx)
        .await?;

        if result.rows_affected() != 1 {
            return Err(MoveError::Conflict);
        }
    }

    sqlx::query(
        r#"
        INSERT INTO l2o_item_moves
            (actor_login, char_id, object_id, item_id, count, enchant_level, from_loc, to_loc)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind(actor_login)
    .bind(char_id)
    .bind(object_id)
    .bind(item.item_id)
    .bind(item.count)
    .bind(item.enchant_level)
    .bind(from_loc_static)
    .bind(to_loc_static)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(MoveOutcome {
        item_id: item.item_id,
        count: item.count,
        from_loc: from_loc_static,
        to_loc: to_loc_static,
    })
}

async fn cleanup_item_aux(
    tx: &mut sqlx::MySqlConnection,
    object_id: i32,
) -> Result<()> {
    let _ = sqlx::query("DELETE FROM item_variables WHERE id = ?")
        .bind(object_id)
        .execute(&mut *tx)
        .await;
    Ok(())
}

pub async fn crystallize_item(
    pool: &MySqlPool,
    char_id: u32,
    object_id: i32,
    crystal_info: impl Fn(i32) -> Option<CrystalInfo>,
    actor_login: &str,
) -> Result<CrystalOutcome, MoveError> {
    let mut tx = pool.begin().await?;

    let Some(ch) = load_char(&mut tx, char_id).await? else {
        return Err(MoveError::CharacterNotFound);
    };
    if ch.online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let row: Option<(i32, u64, i32, i64)> = sqlx::query_as(
        r#"
        SELECT item_id, count, COALESCE(enchant_level, 0), CAST(time AS SIGNED)
        FROM items
        WHERE object_id = ? AND owner_id = ? AND loc = 'INVENTORY'
        LIMIT 1
        FOR UPDATE
        "#,
    )
    .bind(object_id)
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((item_id, count, enchant, time)) = row else {
        return Err(MoveError::ItemNotFound);
    };
    if time > 0 {
        return Err(MoveError::NotCrystallizable);
    }

    let Some(info) = crystal_info(item_id) else {
        return Err(MoveError::NotCrystallizable);
    };
    let Some((crystal_id, amount)) =
        crystal_value(&info.crystal_type, info.base_count, info.is_weapon, enchant)
    else {
        return Err(MoveError::NotCrystallizable);
    };

    let existing: Option<(i32, u64)> = sqlx::query_as(
        r#"
        SELECT object_id, count FROM items
        WHERE owner_id = ? AND loc = 'INVENTORY' AND item_id = ? AND object_id != ?
        LIMIT 1
        FOR UPDATE
        "#,
    )
    .bind(char_id)
    .bind(crystal_id)
    .bind(object_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some((stack_oid, stack_count)) = existing {
        let new_count = stack_count + amount as u64;
        sqlx::query("UPDATE items SET count = ? WHERE object_id = ?")
            .bind(new_count)
            .bind(stack_oid)
            .execute(&mut *tx)
            .await?;
        let deleted = sqlx::query("DELETE FROM items WHERE object_id = ? AND owner_id = ?")
            .bind(object_id)
            .bind(char_id)
            .execute(&mut *tx)
            .await?;
        if deleted.rows_affected() != 1 {
            return Err(MoveError::Conflict);
        }
    } else {
        let updated = sqlx::query(
            r#"
            UPDATE items
            SET item_id = ?, count = ?, enchant_level = 0, custom_type1 = 0,
                custom_type2 = 0, mana_left = -1, time = 0
            WHERE object_id = ? AND owner_id = ? AND loc = 'INVENTORY'
            "#,
        )
        .bind(crystal_id)
        .bind(amount as u64)
        .bind(object_id)
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() != 1 {
            return Err(MoveError::Conflict);
        }
    }
    cleanup_item_aux(&mut tx, object_id).await?;

    sqlx::query(
        r#"
        INSERT INTO l2o_item_moves
            (actor_login, char_id, object_id, item_id, count, enchant_level, from_loc, to_loc)
        VALUES (?, ?, ?, ?, ?, ?, 'INVENTORY', 'CRYSTAL')
        "#,
    )
    .bind(actor_login)
    .bind(char_id)
    .bind(object_id)
    .bind(item_id)
    .bind(count)
    .bind(enchant)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(CrystalOutcome {
        item_id,
        crystals_item_id: crystal_id,
        crystals: amount,
    })
}

pub async fn delete_item(
    pool: &MySqlPool,
    char_id: u32,
    object_id: i32,
    actor_login: &str,
) -> Result<DeleteOutcome, MoveError> {
    let mut tx = pool.begin().await?;

    let Some(ch) = load_char(&mut tx, char_id).await? else {
        return Err(MoveError::CharacterNotFound);
    };
    if ch.online == 1 {
        return Err(MoveError::CharacterOnline);
    }

    let row: Option<(i32, u64, String)> = sqlx::query_as(
        r#"
        SELECT item_id, count, loc FROM items
        WHERE object_id = ? AND owner_id = ? AND loc IN ('INVENTORY', 'WAREHOUSE')
        LIMIT 1
        FOR UPDATE
        "#,
    )
    .bind(object_id)
    .bind(char_id)
    .fetch_optional(&mut *tx)
    .await?;

    let Some((item_id, count, loc)) = row else {
        return Err(MoveError::NotAllowed);
    };

    let deleted = sqlx::query("DELETE FROM items WHERE object_id = ? AND owner_id = ?")
        .bind(object_id)
        .bind(char_id)
        .execute(&mut *tx)
        .await?;
    if deleted.rows_affected() != 1 {
        return Err(MoveError::Conflict);
    }
    cleanup_item_aux(&mut tx, object_id).await?;

    let from_loc: &'static str = if loc == "INVENTORY" {
        "INVENTORY"
    } else {
        "WAREHOUSE"
    };

    sqlx::query(
        r#"
        INSERT INTO l2o_item_moves
            (actor_login, char_id, object_id, item_id, count, enchant_level, from_loc, to_loc)
        VALUES (?, ?, ?, ?, ?, 0, ?, 'DELETE')
        "#,
    )
    .bind(actor_login)
    .bind(char_id)
    .bind(object_id)
    .bind(item_id)
    .bind(count)
    .bind(from_loc)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;

    Ok(DeleteOutcome {
        item_id,
        count,
        from_loc,
    })
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct MoveRow {
    pub id: i64,
    pub moved_at: Option<chrono::NaiveDateTime>,
    pub actor_login: String,
    pub char_id: u32,
    pub char_name: Option<String>,
    pub object_id: i32,
    pub item_id: i32,
    pub count: u64,
    pub enchant_level: i32,
    pub from_loc: String,
    pub to_loc: String,
}

pub async fn recent_moves(pool: &MySqlPool, limit: i64) -> Result<Vec<MoveRow>> {
    let rows = sqlx::query_as::<_, MoveRow>(
        r#"
        SELECT m.id, CAST(m.moved_at AS DATETIME) AS moved_at, m.actor_login, m.char_id,
               c.char_name, m.object_id, m.item_id, m.count, m.enchant_level,
               m.from_loc, m.to_loc
        FROM l2o_item_moves m
        LEFT JOIN characters c ON c.charId = m.char_id
        ORDER BY m.id DESC
        LIMIT ?
        "#,
    )
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
