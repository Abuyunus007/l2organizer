#[derive(Debug, Clone, sqlx::FromRow)]
pub struct AccountRow {
    pub login: String,
    pub access_level: i8,
    pub lastactive: u64,
    pub last_ip: Option<String>,
    pub created_time: Option<chrono::NaiveDateTime>,
    pub email: Option<String>,
    pub ban_expiration: Option<i64>,
    pub ban_reason: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct CharacterRow {
    pub char_id: u32,
    pub char_name: String,
    pub account_name: Option<String>,
    pub level: Option<u8>,
    pub classid: Option<u8>,
    pub race: Option<u8>,
    pub clan_id: Option<u32>,
    pub clan_name: Option<String>,
    pub online: Option<u8>,
    pub pvpkills: Option<u16>,
    pub pkkills: Option<u16>,
    pub karma: Option<u32>,
    pub title: Option<String>,
    pub accesslevel: Option<i32>,
    pub last_access: u64,
    pub create_date: chrono::NaiveDate,
    pub deletetime: u64,
    pub nobless: u8,
    pub exp: Option<u64>,
    pub sp: u64,
    pub vitality_points: u16,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub z: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemRow {
    pub owner_id: Option<i32>,
    pub object_id: i32,
    pub item_id: Option<i32>,
    pub count: u64,
    pub enchant_level: Option<i32>,
    pub loc: Option<String>,
    pub loc_data: Option<i32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ItemOwnerRow {
    pub item_id: Option<i32>,
    pub count: u64,
    pub enchant_level: Option<i32>,
    pub loc: Option<String>,
    pub loc_data: Option<i32>,
    pub char_name: Option<String>,
    pub account_name: Option<String>,
    pub char_id: Option<u32>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OnlineRow {
    pub char_id: u32,
    pub char_name: String,
    pub account_name: Option<String>,
    pub level: Option<u8>,
    pub classid: Option<u8>,
    pub clan_name: Option<String>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct DashboardStats {
    pub accounts: i64,
    pub characters: i64,
    pub online: i64,
    pub items: i64,
    pub adena: i64,
    pub clans: i64,
}
