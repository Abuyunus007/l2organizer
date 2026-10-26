use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use askama::Template;
use axum::extract::{Extension, Form, Path, Query, Request, State};
use axum::http::header::{HeaderMap, HeaderValue, COOKIE, SET_COOKIE};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use chrono::{Local, TimeZone};
use l2o_core::inventory::{GameLimits, MoveDirection, MoveError};
use l2o_core::portal::PlayerSession;
use l2o_core::shops::{ShopItem, SHOP_TYPE_BUY, SHOP_TYPE_CRAFT, SHOP_TYPE_SELL};
use l2o_core::MySqlPool;
use serde::Deserialize;
use tower_http::services::ServeDir;

struct AppState {
    pool: MySqlPool,
    items: HashMap<i32, l2o_core::gamedata::ItemInfo>,
    recipes: HashMap<i32, l2o_core::gamedata::Recipe>,
    materials: HashSet<i32>,
    limits: GameLimits,
    gm_access_level: i8,
    sell_rate_percent: i64,
}

type SharedState = Arc<AppState>;

struct AppError(anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("{:#}", self.0);
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("internal error: {:#}", self.0),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

fn fmt_ms(ms: i64) -> String {
    if ms <= 0 {
        return "-".to_string();
    }
    match Local.timestamp_millis_opt(ms) {
        chrono::LocalResult::Single(t) => t.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".to_string(),
    }
}

fn fmt_ban(expiration: Option<i64>, reason: Option<String>) -> String {
    match expiration {
        None => "-".to_string(),
        Some(0) => format!("Permanent ({})", reason.unwrap_or_default()),
        Some(ms) if ms <= chrono::Utc::now().timestamp_millis() => {
            format!("expired ({})", reason.unwrap_or_default())
        }
        Some(ms) => format!(
            "until {} ({})",
            fmt_ms(ms),
            reason.unwrap_or_default()
        ),
    }
}

#[derive(Template)]
#[template(path = "dashboard.html")]
struct DashboardTemplate {
    stats_accounts: i64,
    stats_characters: i64,
    stats_online: i64,
    stats_items: i64,
    stats_adena: i64,
    stats_clans: i64,
    online: Vec<OnlineView>,
}

struct OnlineView {
    char_id: u32,
    char_name: String,
    account_name: String,
    level: String,
    classid: String,
    clan_name: String,
}

async fn dashboard(State(state): State<SharedState>) -> Result<Html<String>, AppError> {
    let stats = l2o_core::queries::dashboard_stats(&state.pool).await?;
    let online = l2o_core::queries::online_players(&state.pool, 50).await?;
    let online = online
        .into_iter()
        .map(|o| OnlineView {
            char_id: o.char_id,
            char_name: o.char_name,
            account_name: o.account_name.unwrap_or_default(),
            level: o.level.map(|l| l.to_string()).unwrap_or_default(),
            classid: o.classid.map(|c| c.to_string()).unwrap_or_default(),
            clan_name: o.clan_name.unwrap_or_default(),
        })
        .collect();
    let tpl = DashboardTemplate {
        stats_accounts: stats.accounts,
        stats_characters: stats.characters,
        stats_online: stats.online,
        stats_items: stats.items,
        stats_adena: stats.adena,
        stats_clans: stats.clans,
        online,
    };
    Ok(Html(tpl.render()?))
}

#[derive(Deserialize)]
struct SearchParams {
    q: Option<String>,
    id: Option<String>,
    owner: Option<String>,
    act: Option<String>,
    ok: Option<String>,
    fail: Option<String>,
    why: Option<String>,
    extra: Option<String>,
    rare: Option<String>,
    mp: Option<String>,
    pid: Option<String>,
    err: Option<String>,
}

#[derive(Template)]
#[template(path = "accounts.html")]
struct AccountsTemplate {
    q: String,
    accounts: Vec<AccountView>,
}

struct AccountView {
    login: String,
    access_level: i8,
    last_active: String,
    last_ip: String,
    email: String,
    created: String,
    ban: String,
}

async fn accounts_page(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let q = params.q.unwrap_or_default();
    let rows = l2o_core::queries::accounts_page(&state.pool, &q).await?;
    let accounts = rows
        .into_iter()
        .map(|a| AccountView {
            login: a.login,
            access_level: a.access_level,
            last_active: fmt_ms(a.lastactive as i64),
            last_ip: a.last_ip.unwrap_or_default(),
            email: a.email.unwrap_or_default(),
            created: a
                .created_time
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default(),
            ban: fmt_ban(a.ban_expiration, a.ban_reason),
        })
        .collect();
    Ok(Html(AccountsTemplate { q, accounts }.render()?))
}

#[derive(Template)]
#[template(path = "characters.html")]
struct CharactersTemplate {
    q: String,
    characters: Vec<CharacterView>,
}

struct CharacterView {
    char_id: u32,
    char_name: String,
    account_name: String,
    level: String,
    classid: String,
    clan_name: String,
    online: bool,
    pvp_pk: String,
    last_access: String,
    accesslevel: i32,
    deletetime: u64,
    nobless: bool,
}

fn to_char_view(c: l2o_core::models::CharacterRow) -> CharacterView {
    CharacterView {
        char_id: c.char_id,
        char_name: c.char_name,
        account_name: c.account_name.unwrap_or_default(),
        level: c.level.map(|l| l.to_string()).unwrap_or_default(),
        classid: c.classid.map(|v| v.to_string()).unwrap_or_default(),
        clan_name: c.clan_name.unwrap_or_default(),
        online: c.online == Some(1),
        pvp_pk: format!(
            "{}/{}",
            c.pvpkills.unwrap_or(0),
            c.pkkills.unwrap_or(0)
        ),
        last_access: fmt_ms(c.last_access as i64),
        accesslevel: c.accesslevel.unwrap_or(0),
        deletetime: c.deletetime,
        nobless: c.nobless == 1,
    }
}

async fn characters_page(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let q = params.q.unwrap_or_default();
    let rows = l2o_core::queries::search_characters(&state.pool, &q).await?;
    let characters = rows.into_iter().map(to_char_view).collect();
    Ok(Html(CharactersTemplate { q, characters }.render()?))
}

#[derive(Template)]
#[template(path = "char_detail.html")]
struct CharDetailTemplate {
    msg: String,
    msg_ok: bool,
    char_id: u32,
    char_name: String,
    account_name: String,
    level: String,
    classid: String,
    race: String,
    clan_name: String,
    online: bool,
    title: String,
    accesslevel: i32,
    nobless: bool,
    vitality_points: String,
    karma: String,
    pvp_pk: String,
    sp: u64,
    exp: u64,
    position: String,
    last_access: String,
    create_date: String,
    delete_pending: String,
    inventory: Vec<ItemView>,
    warehouse: Vec<ItemView>,
}

struct ItemView {
    item_id: i32,
    name: String,
    count: u64,
    enchant: String,
    icon_url: String,
}

async fn char_detail(
    State(state): State<SharedState>,
    Path(char_id): Path<u32>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let Some(c) = l2o_core::queries::character_by_id(&state.pool, char_id).await? else {
        return Ok(Html("<html><body>character not found</body></html>".to_string()));
    };
    let (msg, msg_ok) = admin_msg(params.q.as_deref(), params.why.as_deref());
    let items = l2o_core::queries::character_items(&state.pool, char_id).await?;
    let mut inventory = Vec::new();
    let mut warehouse = Vec::new();
    for item in items {
        let item_id = item.item_id.unwrap_or(0);
        let info = state.items.get(&item_id);
        let view = ItemView {
            item_id,
            name: info
                .map(|i| i.name.clone())
                .unwrap_or_else(|| format!("#{item_id}")),
            count: item.count,
            enchant: item
                .enchant_level
                .filter(|&e| e > 0)
                .map(|e| format!("+{e}"))
                .unwrap_or_default(),
            icon_url: format!("/icons/{item_id}.png"),
        };
        match item.loc.as_deref() {
            Some("WAREHOUSE") => warehouse.push(view),
            _ => inventory.push(view),
        }
    }
    let tpl = CharDetailTemplate {
        msg,
        msg_ok,
        char_id: c.char_id,
        char_name: c.char_name,
        account_name: c.account_name.unwrap_or_default(),
        level: c.level.map(|l| l.to_string()).unwrap_or_default(),
        classid: c.classid.map(|v| v.to_string()).unwrap_or_default(),
        race: c.race.map(|v| v.to_string()).unwrap_or_default(),
        clan_name: c.clan_name.unwrap_or_default(),
        online: c.online == Some(1),
        title: c.title.unwrap_or_default(),
        accesslevel: c.accesslevel.unwrap_or(0),
        nobless: c.nobless == 1,
        vitality_points: c.vitality_points.to_string(),
        karma: c.karma.unwrap_or(0).to_string(),
        pvp_pk: format!("{}/{}", c.pvpkills.unwrap_or(0), c.pkkills.unwrap_or(0)),
        sp: c.sp,
        exp: c.exp.unwrap_or(0),
        position: format!(
            "{},{},{}",
            c.x.unwrap_or(0),
            c.y.unwrap_or(0),
            c.z.unwrap_or(0)
        ),
        last_access: fmt_ms(c.last_access as i64),
        create_date: c.create_date.format("%Y-%m-%d").to_string(),
        delete_pending: if c.deletetime > 0 {
            fmt_ms(c.deletetime as i64)
        } else {
            "-".to_string()
        },
        inventory,
        warehouse,
    };
    Ok(Html(tpl.render()?))
}

#[derive(Template)]
#[template(path = "items.html")]
struct ItemsTemplate {
    item_id: String,
    owner_query: String,
    item: Option<ItemInfoView>,
    owners: Vec<ItemOwnerView>,
    owner_matches: Vec<CharacterView>,
}

struct ItemInfoView {
    item_id: i32,
    name: String,
    icon_url: String,
    total_count: u64,
    owner_count: usize,
}

struct ItemOwnerView {
    char_id: u32,
    char_name: String,
    account_name: String,
    count: u64,
    enchant: String,
    loc: String,
}

async fn items_page(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let id_raw = params.id.unwrap_or_default();
    let owner_query = params.owner.unwrap_or_default();
    let mut item = None;
    let mut owners = Vec::new();
    if let Ok(item_id) = id_raw.trim().parse::<i32>() {
        let rows = l2o_core::queries::item_owners(&state.pool, item_id).await?;
        let total: u64 = rows.iter().map(|r| r.count).sum();
        let owner_count = rows.len();
        for r in rows {
            owners.push(ItemOwnerView {
                char_id: r.char_id.unwrap_or(0),
                char_name: r.char_name.unwrap_or_else(|| "(no owner)".into()),
                account_name: r.account_name.unwrap_or_default(),
                count: r.count,
                enchant: r.enchant_level.unwrap_or(0).to_string(),
                loc: r.loc.unwrap_or_default(),
            });
        }
        let info = state.items.get(&item_id);
        item = Some(ItemInfoView {
            item_id,
            name: info
                .map(|i| i.name.clone())
                .unwrap_or_else(|| "(unknown item)".into()),
            icon_url: format!("/icons/{item_id}.png"),
            total_count: total,
            owner_count,
        });
    }
    let mut owner_matches = Vec::new();
    if !owner_query.trim().is_empty() {
        let rows = l2o_core::queries::search_characters(&state.pool, owner_query.trim()).await?;
        owner_matches = rows.into_iter().map(to_char_view).collect();
    }
    let tpl = ItemsTemplate {
        item_id: id_raw,
        owner_query,
        item,
        owners,
        owner_matches,
    };
    Ok(Html(tpl.render()?))
}

#[derive(Deserialize)]
struct LoginForm {
    username: String,
    password: String,
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let header = headers.get(COOKIE)?.to_str().ok()?;
    header
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

async fn login_page(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = cookie_value(&headers, "l2o_session") {
        if l2o_core::auth::session_user(&state.pool, &token).await?.is_some() {
            return Ok(Redirect::to("/").into_response());
        }
    }
    Ok(Html(
        LoginTemplate {
            error: String::new(),
        }
        .render()?,
    )
    .into_response())
}

fn admin_session_response(token: &str) -> Response {
    let mut resp = Redirect::to("/").into_response();
    let cookie = format!("l2o_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=43200");
    resp.headers_mut()
        .append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
    resp
}

async fn login_submit(
    State(state): State<SharedState>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let username = form.username.trim();

    if let Some(user) = l2o_core::auth::verify_login(&state.pool, username, &form.password).await? {
        let token = l2o_core::auth::create_session(&state.pool, user.id).await?;
        tracing::info!("user '{}' logged in", user.username);
        return Ok(admin_session_response(&token));
    }

    match l2o_core::portal::verify_game_login(&state.pool, username, &form.password).await? {
        l2o_core::portal::LoginOutcome::Ok(acc)
            if acc.access_level >= state.gm_access_level =>
        {
            let user_id = l2o_core::auth::find_or_create_game_gm(&state.pool, &acc.login).await?;
            let token = l2o_core::auth::create_session(&state.pool, user_id).await?;
            tracing::info!(
                "GM '{}' (access level {}) logged in with game credentials",
                acc.login,
                acc.access_level
            );
            return Ok(admin_session_response(&token));
        }
        l2o_core::portal::LoginOutcome::Ok(acc) => {
            tracing::warn!("game account '{}' below GM level tried admin login", username);
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let error = format!(
                "Game account recognized, but access level {} is below the required GM level ({}). Players can use the portal under /my.",
                acc.access_level, state.gm_access_level
            );
            return Ok(Html(LoginTemplate { error }.render()?).into_response());
        }
        l2o_core::portal::LoginOutcome::Banned => {
            tracing::warn!("banned account '{}' tried admin login", username);
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            return Ok(Html(
                LoginTemplate {
                    error: "This account is banned.".into(),
                }
                .render()?,
            )
            .into_response());
        }
        l2o_core::portal::LoginOutcome::BadCredentials => {}
    }

    tracing::warn!("failed login attempt for '{}'", username);
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    Ok(Html(
        LoginTemplate {
            error: "Invalid username or password.".into(),
        }
        .render()?,
    )
    .into_response())
}

async fn logout(State(state): State<SharedState>, headers: HeaderMap) -> Result<Response, AppError> {
    if let Some(token) = cookie_value(&headers, "l2o_session") {
        l2o_core::auth::delete_session(&state.pool, &token).await?;
    }
    let mut resp = Redirect::to("/login").into_response();
    resp.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_static("l2o_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0"),
    );
    Ok(resp)
}

async fn require_auth(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(token) = cookie_value(req.headers(), "l2o_session") {
        if let Ok(Some(user)) = l2o_core::auth::session_user(&state.pool, &token).await {
            req.extensions_mut().insert(user);
            return next.run(req).await;
        }
    }
    Redirect::to("/login").into_response()
}

fn move_msg(code: &str) -> (String, bool) {
    match code {
        "ok" => ("Item moved.".to_string(), true),
        "none" => ("No items were selected.".to_string(), false),
        "online" => (
            "Your character is online in the game. Log out completely before moving items.".into(),
            false,
        ),
        "nocl" => ("This character is not in a clan.".to_string(), false),
        "perm" => (
            "Only the clan leader can withdraw from the clan warehouse on this server.".into(),
            false,
        ),
        "item" => ("Item not found in the expected storage. Please reload.".to_string(), false),
        "full" => ("Target storage is full.".to_string(), false),
        "state" => ("Item state changed in the meantime. Please retry.".to_string(), false),
        "notfound" => ("Character not found.".to_string(), false),
        "db" => ("Internal database error. Please contact the administrator.".to_string(), false),
        _ => (String::new(), true),
    }
}

fn err_code(e: &MoveError) -> &'static str {
    match e {
        MoveError::CharacterNotFound => "notfound",
        MoveError::CharacterOnline => "online",
        MoveError::NotInClan => "nocl",
        MoveError::NotClanLeader => "perm",
        MoveError::ItemNotFound => "item",
        MoveError::StorageFull => "full",
        MoveError::Conflict => "state",
        MoveError::NotCrystallizable => "notcryst",
        MoveError::NotAllowed => "notallowed",
        MoveError::RecipeNotKnown => "recipe",
        MoveError::SkillTooLow => "skill",
        MoveError::IngredientsMissing => "ingredients",
        MoveError::NotEnoughMana => "mp",
        MoveError::NotEnoughHp => "hp",
        MoveError::NotSellable => "notsellable",
        MoveError::ShopsDisabled => "shopsdisabled",
        MoveError::NameTaken => "nametaken",
        MoveError::ValueInvalid => "value",
        MoveError::Db(_) => "db",
    }
}

fn why_text(code: &str) -> &'static str {
    match code {
        "full" => "storage full",
        "perm" => "clan leader only",
        "online" => "character online",
        "item" => "item not found",
        "state" => "state changed",
        "notcryst" => "not crystallizable",
        "notallowed" => "not allowed",
        "nocl" => "no clan",
        "notfound" => "character not found",
        "recipe" => "recipe not known",
        "skill" => "create item skill too low",
        "ingredients" => "missing ingredients",
        "mp" => "not enough MP",
        "hp" => "not enough HP",
        "notsellable" => "item cannot be sold",
        "shopsdisabled" => "offline shops disabled",
        "nametaken" => "name already taken",
        "value" => "invalid value",
        "db" => "database error",
        _ => "unknown error",
    }
}

fn admin_msg(q: Option<&str>, why: Option<&str>) -> (String, bool) {
    match q {
        Some("ban_ok") => ("Account banned.".to_string(), true),
        Some("unban_ok") => ("Account unbanned.".to_string(), true),
        Some("edit_ok") => ("Character updated. Undo available under Ops.".to_string(), true),
        Some("restore_ok") => ("Character restored from deletion.".to_string(), true),
        Some("grant_ok") => ("Item granted. Undo available under Ops.".to_string(), true),
        Some("remove_ok") => ("Item removed. Undo available under Ops.".to_string(), true),
        Some("undo_ok") => ("Operation reverted.".to_string(), true),
        Some("err") => (
            format!("Action failed: {}.", why_text(why.unwrap_or(""))),
            false,
        ),
        _ => (String::new(), true),
    }
}

fn batch_msg(act: &str, ok: i64, fail: i64, why: &str, extra: i64) -> (String, bool) {
    let verb = match act {
        "to_wh" => "moved to warehouse",
        "wh_to_inv" | "clan_to_inv" => "moved to inventory",
        "to_clan" => "deposited into the clan warehouse",
        "crystallize" => "crystallized",
        "delete" => "deleted",
        "sell" => "sold",
        _ => "processed",
    };
    let extra_txt = match act {
        "crystallize" if extra > 0 => format!(" Received {extra} crystals."),
        "sell" if extra > 0 => format!(" Received {extra} adena."),
        _ => String::new(),
    };
    if fail == 0 {
        (format!("{ok} item(s) {verb}.{extra_txt}"), true)
    } else if ok == 0 {
        (
            format!("No item was {verb}: {}.", why_text(why)),
            false,
        )
    } else {
        (
            format!(
                "{ok} item(s) {verb}, {fail} failed ({}).{extra_txt}",
                why_text(why)
            ),
            false,
        )
    }
}

#[derive(Template)]
#[template(path = "portal_login.html")]
struct PortalLoginTemplate {
    error: String,
}

#[derive(Template)]
#[template(path = "portal_chars.html")]
struct PortalCharsTemplate {
    login: String,
    is_gm: bool,
    characters: Vec<PortalCharView>,
}

struct PortalCharView {
    char_id: u32,
    char_name: String,
    level: String,
    online: bool,
    clan_name: String,
}

#[derive(Template)]
#[template(path = "portal_overview.html")]
struct PortalOverviewTemplate {
    login: String,
    is_gm: bool,
    active: String,
    is_dwarf: bool,
    msg: String,
    msg_ok: bool,
    online: bool,
    char_id: u32,
    char_name: String,
    level: String,
    classid: String,
    race: String,
    clan_name: String,
    title: String,
    accesslevel: i32,
    nobless: bool,
    karma: String,
    pvp_pk: String,
    vitals: String,
    position: String,
    create_date: String,
    last_access: String,
    delete_pending: String,
    inv_used: i64,
    wh_used: i64,
    clan_used: i64,
    in_clan: bool,
    adena: u64,
    recipe_count: usize,
    shop_label: String,
}

#[derive(Template)]
#[template(path = "portal_items.html")]
struct PortalItemsTemplate {
    login: String,
    is_gm: bool,
    active: String,
    is_dwarf: bool,
    char_id: u32,
    char_name: String,
    online: bool,
    in_clan: bool,
    can_withdraw_clan: bool,
    is_leader: bool,
    inventory: Vec<PortalItem>,
    warehouse: Vec<PortalItem>,
    clanwh: Vec<PortalItem>,
    inv_used: i64,
    inv_limit: i64,
    wh_used: i64,
    wh_limit: i64,
    clan_used: i64,
    clan_limit: i64,
    sell_rate_percent: i64,
    msg: String,
    msg_ok: bool,
}

#[derive(Template)]
#[template(path = "portal_craft.html")]
struct PortalCraftTemplate {
    login: String,
    is_gm: bool,
    active: String,
    is_dwarf: bool,
    char_id: u32,
    char_name: String,
    online: bool,
    vitals: String,
    craft_recipes: Vec<CraftRecipeView>,
    msg: String,
    msg_ok: bool,
}

#[derive(Template)]
#[template(path = "portal_shop.html")]
struct PortalShopTemplate {
    login: String,
    is_gm: bool,
    active: String,
    is_dwarf: bool,
    char_id: u32,
    char_name: String,
    online: bool,
    offline_trade_enabled: bool,
    offline_craft_enabled: bool,
    inventory: Vec<PortalItem>,
    craft_recipes: Vec<CraftRecipeView>,
    shop: Option<ShopView>,
    msg: String,
    msg_ok: bool,
}

struct PortalItem {
    object_id: i32,
    item_id: i32,
    name: String,
    icon_url: String,
    count: u64,
    enchant: String,
    crystal_hint: String,
    is_material: bool,
}

struct CraftRecipeView {
    id: i32,
    name: String,
    product_name: String,
    product_icon: String,
    product_count: i64,
    level: i32,
    dwarven: bool,
    success_rate: i32,
    mp_cost: i64,
    hp_cost: i64,
    ready: bool,
    reason: String,
    ingredients: Vec<IngredientView>,
}

struct IngredientView {
    name: String,
    icon: String,
    need: i64,
    have: u64,
    ok: bool,
}

struct ShopView {
    kind_label: String,
    title: String,
    when: String,
    items: Vec<ShopItemView>,
}

struct ShopItemView {
    label: String,
    icon: String,
    count: u64,
    price: u64,
}

fn item_name(items: &HashMap<i32, l2o_core::gamedata::ItemInfo>, id: i32) -> String {
    items
        .get(&id)
        .map(|i| i.name.clone())
        .unwrap_or_else(|| format!("#{id}"))
}

#[derive(Deserialize)]
struct PortalLoginForm {
    login: String,
    password: String,
}

#[derive(Template)]
#[template(path = "moves.html")]
struct MovesTemplate {
    moves: Vec<MoveView>,
}

struct MoveView {
    when: String,
    actor: String,
    char_id: u32,
    char_name: String,
    item_name: String,
    count: u64,
    enchant: String,
    from_loc: String,
    to_loc: String,
}

fn portal_item_view(
    item: l2o_core::models::ItemRow,
    items: &HashMap<i32, l2o_core::gamedata::ItemInfo>,
    materials: &HashSet<i32>,
) -> PortalItem {
    let item_id = item.item_id.unwrap_or(0);
    let enchant_level = item.enchant_level.unwrap_or(0);
    let crystal_hint = items
        .get(&item_id)
        .and_then(|info| {
            let ct = info.crystal_type.as_deref()?;
            let is_weapon = info.kind == "Weapon";
            l2o_core::inventory::crystal_value(ct, info.crystal_count, is_weapon, enchant_level)
                .map(|(_, amount)| format!("{amount} crystals"))
        })
        .unwrap_or_default();
    PortalItem {
        object_id: item.object_id,
        item_id,
        name: items
            .get(&item_id)
            .map(|i| i.name.clone())
            .unwrap_or_else(|| format!("#{item_id}")),
        icon_url: format!("/icons/{item_id}.png"),
        count: item.count,
        enchant: item
            .enchant_level
            .filter(|&e| e > 0)
            .map(|e| format!("+{e}"))
            .unwrap_or_default(),
        crystal_hint,
        is_material: materials.contains(&item_id),
    }
}

async fn portal_login_page(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = cookie_value(&headers, "l2o_player_session") {
        if l2o_core::portal::session_login(&state.pool, &token).await?.is_some() {
            return Ok(Redirect::to("/my").into_response());
        }
    }
    Ok(Html(
        PortalLoginTemplate {
            error: String::new(),
        }
        .render()?,
    )
    .into_response())
}

async fn portal_login_submit(
    State(state): State<SharedState>,
    Form(form): Form<PortalLoginForm>,
) -> Result<Response, AppError> {
    let login = form.login.trim();
    match l2o_core::portal::verify_game_login(&state.pool, login, &form.password).await? {
        l2o_core::portal::LoginOutcome::Ok(acc) => {
            let token =
                l2o_core::portal::create_session(&state.pool, &acc.login, acc.access_level).await?;
            let mut resp = Redirect::to("/my").into_response();
            let cookie = format!(
                "l2o_player_session={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=43200"
            );
            resp.headers_mut()
                .append(SET_COOKIE, HeaderValue::from_str(&cookie).unwrap());
            tracing::info!("player '{}' logged into portal", login);
            Ok(resp)
        }
        outcome => {
            tracing::warn!("failed portal login for '{}'", login);
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let error = match outcome {
                l2o_core::portal::LoginOutcome::Banned => {
                    "This account is banned.".to_string()
                }
                _ => "Invalid account name or password.".to_string(),
            };
            Ok(Html(PortalLoginTemplate { error }.render()?).into_response())
        }
    }
}

async fn portal_logout(
    State(state): State<SharedState>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    if let Some(token) = cookie_value(&headers, "l2o_player_session") {
        l2o_core::portal::delete_session(&state.pool, &token).await?;
    }
    let mut resp = Redirect::to("/my/login").into_response();
    resp.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_static(
            "l2o_player_session=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        ),
    );
    Ok(resp)
}

async fn require_player_auth(
    State(state): State<SharedState>,
    mut req: Request,
    next: Next,
) -> Response {
    if let Some(token) = cookie_value(req.headers(), "l2o_player_session") {
        if let Ok(Some(session)) = l2o_core::portal::session_login(&state.pool, &token).await {
            req.extensions_mut().insert(session);
            return next.run(req).await;
        }
    }
    Redirect::to("/my/login").into_response()
}

async fn owned_character(
    state: &AppState,
    session: &PlayerSession,
    char_id: u32,
) -> Result<Option<l2o_core::models::CharacterRow>, AppError> {
    let Some(character) = l2o_core::queries::character_by_id(&state.pool, char_id).await? else {
        return Ok(None);
    };
    let owner = character.account_name.clone().unwrap_or_default();
    if !owner.eq_ignore_ascii_case(&session.login) {
        return Ok(None);
    }
    Ok(Some(character))
}

async fn portal_home(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
) -> Result<Html<String>, AppError> {
    let rows = l2o_core::queries::portal_characters(&state.pool, &session.login).await?;
    let characters = rows
        .into_iter()
        .map(|c| PortalCharView {
            char_id: c.char_id,
            char_name: c.char_name,
            level: c.level.map(|l| l.to_string()).unwrap_or_default(),
            online: c.online == Some(1),
            clan_name: c.clan_name.unwrap_or_default(),
        })
        .collect();
    let tpl = PortalCharsTemplate {
        login: session.login,
        is_gm: session.access_level >= state.gm_access_level,
        characters,
    };
    Ok(Html(tpl.render()?))
}

fn build_message(state: &AppState, params: &SearchParams) -> (String, bool) {
    if params.q.as_deref() == Some("batch") {
        let ok = params.ok.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let fail = params.fail.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let extra = params.extra.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        batch_msg(
            params.act.as_deref().unwrap_or(""),
            ok,
            fail,
            params.why.as_deref().unwrap_or(""),
            extra,
        )
    } else if params.q.as_deref() == Some("craft") {
        let ok = params.ok.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let fail = params.fail.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let rare = params.rare.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let mp = params.mp.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
        let pid = params.pid.as_deref().and_then(|v| v.parse::<i32>().ok()).unwrap_or(0);
        let err = params.err.as_deref().unwrap_or("");
        let pname = item_name(&state.items, pid);
        if !err.is_empty() {
            (
                format!("Crafting stopped ({} success so far): {}.", ok, why_text(err)),
                false,
            )
        } else if fail > 0 {
            (
                format!("Crafted {ok} x {pname} ({rare} rare). Failed attempts: {fail}. MP used: {mp}."),
                ok > 0,
            )
        } else {
            (
                format!("Crafted {ok} x {pname} ({rare} rare). MP used: {mp}."),
                true,
            )
        }
    } else if params.q.as_deref() == Some("shop") {
        if let Some(err) = params.err.as_deref() {
            (format!("Could not save shop: {}.", why_text(err)), false)
        } else {
            (
                "Offline shop saved. It will open in game after the next game server restart; keep the character logged out until then.".to_string(),
                true,
            )
        }
    } else if params.q.as_deref() == Some("shopd") {
        if let Some(err) = params.err.as_deref() {
            (format!("Could not delete shop: {}.", why_text(err)), false)
        } else {
            ("Offline shop removed.".to_string(), true)
        }
    } else {
        params.q.as_deref().map(move_msg).unwrap_or_default()
    }
}

struct CharPageCtx {
    character: l2o_core::models::CharacterRow,
    is_dwarf: bool,
    online: bool,
}

async fn char_page_ctx(
    state: &AppState,
    session: &PlayerSession,
    char_id: u32,
) -> Result<Option<CharPageCtx>, AppError> {
    let Some(character) = owned_character(state, session, char_id).await? else {
        return Ok(None);
    };
    let is_dwarf = character.race == Some(4);
    let online = character.online == Some(1);
    Ok(Some(CharPageCtx {
        character,
        is_dwarf,
        online,
    }))
}

async fn build_craft_recipes(
    state: &AppState,
    char_id: u32,
    inv_counts: &HashMap<i32, u64>,
) -> Result<Vec<CraftRecipeView>, AppError> {
    let (cur_hp, _max_hp, cur_mp, _max_mp) =
        l2o_core::queries::character_vitals(&state.pool, char_id).await?;
    let skill_dwarven = l2o_core::queries::character_skill_level(&state.pool, char_id, 172)
        .await?
        .unwrap_or(0);
    let skill_common = l2o_core::queries::character_skill_level(&state.pool, char_id, 1320)
        .await?
        .unwrap_or(0);
    let recipe_ids = l2o_core::queries::character_recipe_ids(&state.pool, char_id).await?;

    Ok(recipe_ids
        .iter()
        .filter_map(|rid| state.recipes.get(rid))
        .map(|r| {
            let skill = if r.dwarven { skill_dwarven } else { skill_common };
            let skill_ok = skill >= r.level;
            let ing_views: Vec<IngredientView> = r
                .ingredients
                .iter()
                .map(|(iid, need)| {
                    let have = inv_counts.get(iid).copied().unwrap_or(0);
                    IngredientView {
                        name: item_name(&state.items, *iid),
                        icon: format!("/icons/{iid}.png"),
                        need: *need,
                        have,
                        ok: have >= (*need).max(0) as u64,
                    }
                })
                .collect();
            let ing_ok = ing_views.iter().all(|i| i.ok);
            let mp_ok = r.mp_cost == 0 || cur_mp >= r.mp_cost;
            let hp_ok = r.hp_cost == 0 || cur_hp > r.hp_cost;
            let (ready, reason) = if !skill_ok {
                (false, "create item skill too low".to_string())
            } else if !ing_ok {
                (false, "missing ingredients".to_string())
            } else if !mp_ok {
                (false, "not enough MP".to_string())
            } else if !hp_ok {
                (false, "not enough HP".to_string())
            } else {
                (true, String::new())
            };
            CraftRecipeView {
                id: r.id,
                name: r.name.clone(),
                product_name: item_name(&state.items, r.product_id),
                product_icon: format!("/icons/{}.png", r.product_id),
                product_count: r.product_count,
                level: r.level,
                dwarven: r.dwarven,
                success_rate: r.success_rate,
                mp_cost: r.mp_cost,
                hp_cost: r.hp_cost,
                ready,
                reason,
                ingredients: ing_views,
            }
        })
        .collect())
}

async fn build_shop_view(
    state: &AppState,
    char_id: u32,
) -> Result<Option<ShopView>, AppError> {
    let Some(s) = l2o_core::shops::get_shop(&state.pool, char_id).await? else {
        return Ok(None);
    };
    let oid_map = if s.kind == SHOP_TYPE_SELL {
        l2o_core::queries::object_item_map(&state.pool, char_id).await?
    } else {
        HashMap::new()
    };
    let items = s
        .items
        .iter()
        .map(|it| {
            let (label, icon_id) = match s.kind {
                SHOP_TYPE_SELL => {
                    let iid = oid_map.get(&it.item).copied().unwrap_or(0);
                    (item_name(&state.items, iid), iid)
                }
                SHOP_TYPE_CRAFT => {
                    let rid = it.item;
                    let name = state
                        .recipes
                        .get(&rid)
                        .map(|r| r.name.clone())
                        .unwrap_or_else(|| format!("Recipe #{rid}"));
                    let icon = state.recipes.get(&rid).map(|r| r.product_id).unwrap_or(0);
                    (name, icon)
                }
                _ => (item_name(&state.items, it.item), it.item),
            };
            ShopItemView {
                label,
                icon: format!("/icons/{icon_id}.png"),
                count: it.count,
                price: it.price,
            }
        })
        .collect();
    Ok(Some(ShopView {
        kind_label: match s.kind {
            SHOP_TYPE_SELL => "Sell shop".to_string(),
            SHOP_TYPE_BUY => "Buy shop".to_string(),
            SHOP_TYPE_CRAFT => "Craft shop".to_string(),
            _ => format!("Type {}", s.kind),
        },
        title: s.title,
        when: fmt_ms(s.time_ms),
        items,
    }))
}

async fn portal_overview(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    let Some(ctx) = char_page_ctx(&state, &session, char_id).await? else {
        return Ok(Html(
            "<html><body style=\"background:#14161a;color:#e6e6e6;font-family:sans-serif\"><p>Character not found on your account.</p></body></html>"
                .to_string(),
        )
        .into_response());
    };
    let character = ctx.character;

    let inventory_rows =
        l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "INVENTORY").await?;
    let wh_used = l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "WAREHOUSE")
        .await?
        .len() as i64;
    let clan_id = character.clan_id.unwrap_or(0);
    let clan_used = if clan_id > 0 {
        l2o_core::queries::items_in_loc(&state.pool, clan_id as i64, "CLANWH")
            .await?
            .len() as i64
    } else {
        0
    };
    let adena: u64 = inventory_rows
        .iter()
        .filter(|r| r.item_id == Some(57))
        .map(|r| r.count)
        .sum();
    let inv_used = inventory_rows.len() as i64;

    let recipe_count = if ctx.is_dwarf {
        l2o_core::queries::character_recipe_ids(&state.pool, char_id)
            .await?
            .len()
    } else {
        0
    };
    let shop_label = match build_shop_view(&state, char_id).await? {
        Some(s) => format!("{} \"{}\"", s.kind_label, s.title),
        None => "no shop stored".to_string(),
    };

    let (cur_hp, max_hp, cur_mp, max_mp) =
        l2o_core::queries::character_vitals(&state.pool, char_id).await?;
    let (msg, msg_ok) = build_message(&state, &params);

    let tpl = PortalOverviewTemplate {
        login: session.login,
        is_gm: session.access_level >= state.gm_access_level,
        active: "overview".to_string(),
        is_dwarf: ctx.is_dwarf,
        msg,
        msg_ok,
        online: ctx.online,
        char_id,
        char_name: character.char_name,
        level: character.level.map(|l| l.to_string()).unwrap_or_default(),
        classid: character.classid.map(|v| v.to_string()).unwrap_or_default(),
        race: character.race.map(|v| v.to_string()).unwrap_or_default(),
        clan_name: character.clan_name.unwrap_or_default(),
        title: character.title.unwrap_or_default(),
        accesslevel: character.accesslevel.unwrap_or(0),
        nobless: character.nobless == 1,
        karma: character.karma.unwrap_or(0).to_string(),
        pvp_pk: format!(
            "{}/{}",
            character.pvpkills.unwrap_or(0),
            character.pkkills.unwrap_or(0)
        ),
        vitals: format!("HP {cur_hp}/{max_hp} · MP {cur_mp}/{max_mp}"),
        position: format!(
            "{},{},{}",
            character.x.unwrap_or(0),
            character.y.unwrap_or(0),
            character.z.unwrap_or(0)
        ),
        create_date: character.create_date.format("%Y-%m-%d").to_string(),
        last_access: fmt_ms(character.last_access as i64),
        delete_pending: if character.deletetime > 0 {
            fmt_ms(character.deletetime as i64)
        } else {
            "-".to_string()
        },
        inv_used,
        wh_used,
        clan_used,
        in_clan: clan_id > 0,
        adena,
        recipe_count,
        shop_label,
    };
    Ok(Html(tpl.render()?).into_response())
}

async fn portal_craft_page(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    let Some(ctx) = char_page_ctx(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };
    if !ctx.is_dwarf {
        let uri = format!("/my/char/{char_id}");
        return Ok(Redirect::to(&uri).into_response());
    }

    let inventory_rows =
        l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "INVENTORY").await?;
    let inv_counts: HashMap<i32, u64> = {
        let mut m: HashMap<i32, u64> = HashMap::new();
        for row in &inventory_rows {
            if let Some(item_id) = row.item_id {
                *m.entry(item_id).or_insert(0) += row.count;
            }
        }
        m
    };
    let craft_recipes = build_craft_recipes(&state, char_id, &inv_counts).await?;
    let (cur_hp, max_hp, cur_mp, max_mp) =
        l2o_core::queries::character_vitals(&state.pool, char_id).await?;
    let (msg, msg_ok) = build_message(&state, &params);

    let tpl = PortalCraftTemplate {
        login: session.login,
        is_gm: session.access_level >= state.gm_access_level,
        active: "craft".to_string(),
        is_dwarf: true,
        char_id,
        char_name: ctx.character.char_name,
        online: ctx.online,
        vitals: format!("HP {cur_hp}/{max_hp} · MP {cur_mp}/{max_mp}"),
        craft_recipes,
        msg,
        msg_ok,
    };
    Ok(Html(tpl.render()?).into_response())
}

async fn portal_shop_page(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    let Some(ctx) = char_page_ctx(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };

    let inventory_rows =
        l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "INVENTORY").await?;
    let inv_counts: HashMap<i32, u64> = {
        let mut m: HashMap<i32, u64> = HashMap::new();
        for row in &inventory_rows {
            if let Some(item_id) = row.item_id {
                *m.entry(item_id).or_insert(0) += row.count;
            }
        }
        m
    };
    let mut inventory: Vec<PortalItem> = inventory_rows
        .into_iter()
        .map(|i| portal_item_view(i, &state.items, &state.materials))
        .collect();
    inventory.sort_by_key(|it| !it.is_material);

    let craft_recipes = if ctx.is_dwarf {
        build_craft_recipes(&state, char_id, &inv_counts).await?
    } else {
        Vec::new()
    };
    let shop = build_shop_view(&state, char_id).await?;
    let (msg, msg_ok) = build_message(&state, &params);

    let tpl = PortalShopTemplate {
        login: session.login,
        is_gm: session.access_level >= state.gm_access_level,
        active: "shop".to_string(),
        is_dwarf: ctx.is_dwarf,
        char_id,
        char_name: ctx.character.char_name,
        online: ctx.online,
        offline_trade_enabled: state.limits.offline_trade_enabled,
        offline_craft_enabled: state.limits.offline_craft_enabled && ctx.is_dwarf,
        inventory,
        craft_recipes,
        shop,
        msg,
        msg_ok,
    };
    Ok(Html(tpl.render()?).into_response())
}

async fn portal_items_page(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Query(params): Query<SearchParams>,
) -> Result<Response, AppError> {
    let Some(character) = owned_character(&state, &session, char_id).await? else {
        return Ok(Html(
            "<html><body style=\"background:#14161a;color:#e6e6e6;font-family:sans-serif\"><p>Character not found on your account.</p></body></html>"
                .to_string(),
        )
        .into_response());
    };

    let inventory_rows =
        l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "INVENTORY").await?;
    let warehouse_rows =
        l2o_core::queries::items_in_loc(&state.pool, char_id as i64, "WAREHOUSE").await?;
    let clan_id = character.clan_id.unwrap_or(0);
    let clanwh_rows = if clan_id > 0 {
        l2o_core::queries::items_in_loc(&state.pool, clan_id as i64, "CLANWH").await?
    } else {
        Vec::new()
    };

    let is_leader = l2o_core::queries::clan_leader(&state.pool, clan_id).await? == Some(char_id);
    let can_withdraw_clan =
        clan_id > 0 && (state.limits.members_can_withdraw_from_clanwh || is_leader);

    let race = character.race.unwrap_or(0);
    let access_level = character.accesslevel.unwrap_or(0);
    let inv_limit = if access_level > 0 {
        state.limits.inv_gm
    } else if race == 4 {
        state.limits.inv_dwarf
    } else {
        state.limits.inv_no_dwarf
    };
    let wh_limit = if race == 4 {
        state.limits.wh_dwarf
    } else {
        state.limits.wh_no_dwarf
    };

    let inv_used = inventory_rows.len() as i64;
    let wh_used = warehouse_rows.len() as i64;
    let clan_used = clanwh_rows.len() as i64;

    let mut inventory: Vec<PortalItem> = inventory_rows
        .into_iter()
        .map(|i| portal_item_view(i, &state.items, &state.materials))
        .collect();
    inventory.sort_by_key(|it| !it.is_material);
    let mut warehouse: Vec<PortalItem> = warehouse_rows
        .into_iter()
        .map(|i| portal_item_view(i, &state.items, &state.materials))
        .collect();
    warehouse.sort_by_key(|it| !it.is_material);
    let mut clanwh: Vec<PortalItem> = clanwh_rows
        .into_iter()
        .map(|i| portal_item_view(i, &state.items, &state.materials))
        .collect();
    clanwh.sort_by_key(|it| !it.is_material);

    let (msg, msg_ok) = build_message(&state, &params);

    let tpl = PortalItemsTemplate {
        login: session.login,
        is_gm: session.access_level >= state.gm_access_level,
        active: "items".to_string(),
        is_dwarf: character.race == Some(4),
        char_id,
        char_name: character.char_name,
        online: character.online == Some(1),
        in_clan: clan_id > 0,
        can_withdraw_clan,
        is_leader,
        inventory,
        warehouse,
        clanwh,
        inv_used,
        inv_limit,
        wh_used,
        wh_limit,
        clan_used,
        clan_limit: state.limits.wh_clan,
        sell_rate_percent: state.sell_rate_percent,
        msg,
        msg_ok,
    };
    Ok(Html(tpl.render()?).into_response())
}
async fn portal_batch(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let Some(_character) = owned_character(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };

    let action = pairs
        .iter()
        .find(|(k, _)| k == "action")
        .map(|(_, v)| v.clone())
        .unwrap_or_default();
    let mut oids: Vec<i32> = pairs
        .iter()
        .filter(|(k, _)| k == "oids")
        .filter_map(|(_, v)| v.parse::<i32>().ok())
        .collect();
    oids.sort_unstable();
    oids.dedup();

    if oids.is_empty() {
        let uri = format!("/my/char/{char_id}/items?q=none");
        return Ok(Redirect::to(&uri).into_response());
    }

    if action == "sell" {
        let items = &state.items;
        let outcome = l2o_core::inventory::sell_items(
            &state.pool,
            char_id,
            &oids,
            |item_id| items.get(&item_id).map(|i| i.price).unwrap_or(0),
            state.sell_rate_percent,
            &session.login,
        )
        .await;
        let (ok, fail, why, extra) = match outcome {
            Ok(o) => (o.sold, o.failed, o.last_why, o.adena),
            Err(e) => {
                if let MoveError::Db(err) = &e {
                    tracing::error!("sell failed: {err:#}");
                }
                (0, oids.len() as i64, err_code(&e).to_string(), 0)
            }
        };
        let mut uri = format!("/my/char/{char_id}/items?q=batch&act=sell&ok={ok}&fail={fail}");
        if !why.is_empty() {
            uri.push_str(&format!("&why={why}"));
        }
        if extra > 0 {
            uri.push_str(&format!("&extra={extra}"));
        }
        return Ok(Redirect::to(&uri).into_response());
    }

    let items = &state.items;
    let info_fn = |item_id: i32| {
        items.get(&item_id).and_then(|i| {
            i.crystal_type
                .as_ref()
                .map(|ct| l2o_core::inventory::CrystalInfo {
                    crystal_type: ct.clone(),
                    base_count: i.crystal_count,
                    is_weapon: i.kind == "Weapon",
                })
        })
    };

    let mut ok = 0i64;
    let mut fail = 0i64;
    let mut last_why = String::new();
    let mut crystal_total = 0i64;

    for oid in oids {
        let result: Result<(), MoveError> = match action.as_str() {
            "to_wh" => l2o_core::inventory::move_item(
                &state.pool,
                &state.limits,
                char_id,
                oid,
                MoveDirection::InventoryToWarehouse,
                |id| items.get(&id).map(|i| i.stackable).unwrap_or(false),
                &session.login,
            )
            .await
            .map(|_| ()),
            "to_clan" => l2o_core::inventory::move_item(
                &state.pool,
                &state.limits,
                char_id,
                oid,
                MoveDirection::InventoryToClan,
                |id| items.get(&id).map(|i| i.stackable).unwrap_or(false),
                &session.login,
            )
            .await
            .map(|_| ()),
            "wh_to_inv" => l2o_core::inventory::move_item(
                &state.pool,
                &state.limits,
                char_id,
                oid,
                MoveDirection::WarehouseToInventory,
                |id| items.get(&id).map(|i| i.stackable).unwrap_or(false),
                &session.login,
            )
            .await
            .map(|_| ()),
            "clan_to_inv" => l2o_core::inventory::move_item(
                &state.pool,
                &state.limits,
                char_id,
                oid,
                MoveDirection::ClanToInventory,
                |id| items.get(&id).map(|i| i.stackable).unwrap_or(false),
                &session.login,
            )
            .await
            .map(|_| ()),
            "crystallize" => {
                match l2o_core::inventory::crystallize_item(
                    &state.pool,
                    char_id,
                    oid,
                    info_fn,
                    &session.login,
                )
                .await
                {
                    Ok(outcome) => {
                        crystal_total += outcome.crystals;
                        tracing::info!(
                            "portal crystallize: {} -> item {} gives {} crystals (object {})",
                            session.login,
                            outcome.item_id,
                            outcome.crystals,
                            oid
                        );
                        Ok(())
                    }
                    Err(e) => Err(e),
                }
            }
            "delete" => l2o_core::inventory::delete_item(&state.pool, char_id, oid, &session.login)
                .await
                .map(|_| ()),
            _ => Err(MoveError::NotAllowed),
        };

        match result {
            Ok(()) => ok += 1,
            Err(e) => {
                fail += 1;
                last_why = err_code(&e).to_string();
                if let MoveError::Db(err) = &e {
                    tracing::error!("portal batch op failed: {err:#}");
                }
            }
        }
    }

    let mut uri = format!("/my/char/{char_id}/items?q=batch&act={action}&ok={ok}&fail={fail}");
    if !last_why.is_empty() {
        uri.push_str(&format!("&why={last_why}"));
    }
    if crystal_total > 0 {
        uri.push_str(&format!("&extra={crystal_total}"));
    }
    Ok(Redirect::to(&uri).into_response())
}

#[derive(Deserialize)]
struct CraftForm {
    recipe_id: i32,
    quantity: Option<i32>,
}

async fn portal_craft(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Form(form): Form<CraftForm>,
) -> Result<Response, AppError> {
    let Some(character) = owned_character(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };
    if character.race != Some(4) {
        let uri = format!("/my/char/{char_id}?q=craft&err=notallowed");
        return Ok(Redirect::to(&uri).into_response());
    }

    let Some(recipe) = state.recipes.get(&form.recipe_id).cloned() else {
        let uri = format!("/my/char/{char_id}/craft?q=craft&err=recipe");
        return Ok(Redirect::to(&uri).into_response());
    };

    let max = form.quantity.unwrap_or(1).clamp(1, 50);
    let items = &state.items;
    let mut successes = 0i64;
    let mut fails = 0i64;
    let mut rare = 0i64;
    let mut mp = 0i64;
    let mut err = String::new();

    for _ in 0..max {
        match l2o_core::inventory::craft_item(
            &state.pool,
            &recipe,
            char_id,
            |id| items.get(&id).map(|i| i.stackable).unwrap_or(false),
            &session.login,
        )
        .await
        {
            Ok(o) => {
                mp += o.mp_spent;
                if o.success {
                    successes += 1;
                    if o.rare {
                        rare += 1;
                    }
                } else {
                    fails += 1;
                }
            }
            Err(e) => {
                if let MoveError::Db(db) = &e {
                    tracing::error!("craft failed: {db:#}");
                }
                err = err_code(&e).to_string();
                break;
            }
        }
    }

    let mut uri = format!(
        "/my/char/{char_id}/craft?q=craft&ok={successes}&fail={fails}&rare={rare}&mp={mp}&pid={}",
        recipe.product_id
    );
    if !err.is_empty() {
        uri.push_str(&format!("&err={err}"));
    }
    Ok(Redirect::to(&uri).into_response())
}

async fn portal_shop_set(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
    Form(pairs): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let Some(character) = owned_character(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };
    if character.race != Some(4) && pairs.iter().any(|(_, v)| v == "craft") {
        let uri = format!("/my/char/{char_id}/shop?q=shop&err=notallowed");
        return Ok(Redirect::to(&uri).into_response());
    }

    let get = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    };

    let kind = match get("kind").as_deref() {
        Some("sell") => SHOP_TYPE_SELL,
        Some("buy") => SHOP_TYPE_BUY,
        Some("craft") => SHOP_TYPE_CRAFT,
        _ => {
            let uri = format!("/my/char/{char_id}/shop?q=shop&err=notallowed");
            return Ok(Redirect::to(&uri).into_response());
        }
    };
    let title = get("title").unwrap_or_default();

    let mut shop_items: Vec<ShopItem> = Vec::new();
    for (_, value) in pairs.iter().filter(|(k, _)| k == "item") {
        let Ok(id) = value.parse::<i32>() else {
            continue;
        };
        let price = get(&format!("price_{id}"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        let count = if kind == SHOP_TYPE_CRAFT {
            0
        } else {
            get(&format!("count_{id}"))
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        };
        shop_items.push(ShopItem {
            item: id,
            count,
            price,
        });
    }

    let result =
        l2o_core::shops::set_shop(&state.pool, &state.limits, char_id, kind, &title, &shop_items)
            .await;
    let uri = match result {
        Ok(()) => {
            tracing::info!("offline shop saved for {} by {}", char_id, session.login);
            format!("/my/char/{char_id}/shop?q=shop&ok=1")
        }
        Err(e) => {
            if let MoveError::Db(db) = &e {
                tracing::error!("shop save failed: {db:#}");
            }
            format!("/my/char/{char_id}/shop?q=shop&err={}", err_code(&e))
        }
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn portal_shop_delete(
    State(state): State<SharedState>,
    Extension(session): Extension<PlayerSession>,
    Path(char_id): Path<u32>,
) -> Result<Response, AppError> {
    let Some(_character) = owned_character(&state, &session, char_id).await? else {
        return Ok(Redirect::to("/my").into_response());
    };
    let result = l2o_core::shops::delete_shop(&state.pool, char_id).await;
    let uri = match result {
        Ok(()) => format!("/my/char/{char_id}/shop?q=shopd&ok=1"),
        Err(e) => format!("/my/char/{char_id}/shop?q=shopd&err={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

#[derive(Template)]
#[template(path = "account_detail.html")]
struct AccountDetailTemplate {
    msg: String,
    msg_ok: bool,
    login: String,
    access_level: i8,
    last_active: String,
    last_ip: String,
    email: String,
    created: String,
    ban: String,
    punishments: Vec<PunishmentView>,
    characters: Vec<CharacterView>,
}

struct PunishmentView {
    affect: String,
    kind: String,
    expires: String,
    reason: String,
    by: String,
}

#[derive(Deserialize)]
struct BanForm {
    duration: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
struct EditForm {
    char_name: Option<String>,
    level: Option<String>,
    exp: Option<String>,
    sp: Option<String>,
    sex: Option<String>,
    face: Option<String>,
    hair_style: Option<String>,
    hair_color: Option<String>,
    karma: Option<String>,
    nobless: Option<String>,
    title: Option<String>,
    access_level: Option<String>,
}

#[derive(Deserialize)]
struct GrantForm {
    item_id: String,
    count: Option<String>,
    enchant: Option<String>,
}

#[derive(Deserialize)]
struct RemoveItemForm {
    object_id: String,
}

fn parse_opt_num(s: &Option<String>) -> Option<i64> {
    s.as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<i64>().ok())
}

async fn account_detail_page(
    State(state): State<SharedState>,
    Path(login): Path<String>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let Some(a) = l2o_core::queries::account_detail(&state.pool, &login).await? else {
        return Ok(Html("<p>account not found</p>".to_string()));
    };
    let punishments = l2o_core::queries::account_punishments(&state.pool, &a.login)
        .await?
        .into_iter()
        .map(|p| PunishmentView {
            affect: p.affect,
            kind: p.kind,
            expires: if p.expiration <= 0 {
                "permanent".to_string()
            } else {
                fmt_ms(p.expiration)
            },
            reason: p.reason,
            by: p.punished_by,
        })
        .collect();
    let characters = l2o_core::queries::portal_characters(&state.pool, &a.login)
        .await?
        .into_iter()
        .map(to_char_view)
        .collect();

    let (msg, msg_ok) = if params.q.as_deref() == Some("err") {
        admin_msg(params.q.as_deref(), params.why.as_deref())
    } else {
        admin_msg(params.q.as_deref(), None)
    };

    let tpl = AccountDetailTemplate {
        msg,
        msg_ok,
        login: a.login,
        access_level: a.access_level,
        last_active: fmt_ms(a.lastactive as i64),
        last_ip: a.last_ip.unwrap_or_default(),
        email: a.email.unwrap_or_default(),
        created: a
            .created_time
            .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default(),
        ban: fmt_ban(a.ban_expiration, a.ban_reason),
        punishments,
        characters,
    };
    Ok(Html(tpl.render()?))
}

async fn account_ban(
    State(state): State<SharedState>,
    Path(login): Path<String>,
    Form(form): Form<BanForm>,
) -> Result<Response, AppError> {
    let duration = match form.duration.as_str() {
        "1h" => Some(3_600_000i64),
        "1d" => Some(86_400_000),
        "7d" => Some(7 * 86_400_000),
        "30d" => Some(30 * 86_400_000),
        _ => None,
    };
    let reason = form
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("No reason given")
        .to_string();
    let result =
        l2o_core::admin_ops::ban_account(&state.pool, "admin", &login, duration, &reason).await;
    let uri = match result {
        Ok(()) => format!("/account/{login}?q=ban_ok"),
        Err(e) => format!("/account/{login}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn account_unban(
    State(state): State<SharedState>,
    Path(login): Path<String>,
) -> Result<Response, AppError> {
    let result = l2o_core::admin_ops::unban_account(&state.pool, "admin", &login).await;
    let uri = match result {
        Ok(()) => format!("/account/{login}?q=unban_ok"),
        Err(e) => format!("/account/{login}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn char_edit(
    State(state): State<SharedState>,
    Path(char_id): Path<u32>,
    Form(form): Form<EditForm>,
) -> Result<Response, AppError> {
    let title = form
        .title
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let name = form
        .char_name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let nobless = match form.nobless.as_deref() {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    };
    let changes = l2o_core::admin_ops::CharacterChanges {
        char_name: name,
        level: parse_opt_num(&form.level),
        exp: parse_opt_num(&form.exp),
        sp: parse_opt_num(&form.sp),
        sex: parse_opt_num(&form.sex),
        face: parse_opt_num(&form.face),
        hair_style: parse_opt_num(&form.hair_style),
        hair_color: parse_opt_num(&form.hair_color),
        karma: parse_opt_num(&form.karma),
        nobless,
        title,
        access_level: parse_opt_num(&form.access_level),
    };
    let result = l2o_core::admin_ops::update_character(&state.pool, "admin", char_id, &changes).await;
    let uri = match result {
        Ok(()) => format!("/chars/{char_id}?q=edit_ok"),
        Err(e) => format!("/chars/{char_id}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn char_restore(
    State(state): State<SharedState>,
    Path(char_id): Path<u32>,
) -> Result<Response, AppError> {
    let result = l2o_core::admin_ops::restore_character(&state.pool, "admin", char_id).await;
    let uri = match result {
        Ok(()) => format!("/chars/{char_id}?q=restore_ok"),
        Err(e) => format!("/chars/{char_id}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn char_grant(
    State(state): State<SharedState>,
    Path(char_id): Path<u32>,
    Form(form): Form<GrantForm>,
) -> Result<Response, AppError> {
    let item_id: i32 = form.item_id.trim().parse().unwrap_or(0);
    let count: u64 = form
        .count
        .as_deref()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(1);
    let enchant: i64 = form
        .enchant
        .as_deref()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);
    let stackable = state
        .items
        .get(&item_id)
        .map(|i| i.stackable)
        .unwrap_or(false);
    let result = l2o_core::admin_ops::grant_item(
        &state.pool,
        "admin",
        char_id,
        item_id,
        count,
        enchant,
        stackable,
    )
    .await;
    let uri = match result {
        Ok(()) => format!("/chars/{char_id}?q=grant_ok"),
        Err(e) => format!("/chars/{char_id}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn char_remove_item(
    State(state): State<SharedState>,
    Path(char_id): Path<u32>,
    Form(form): Form<RemoveItemForm>,
) -> Result<Response, AppError> {
    let object_id: i32 = form.object_id.trim().parse().unwrap_or(0);
    let result = l2o_core::admin_ops::remove_item(&state.pool, "admin", char_id, object_id).await;
    let uri = match result {
        Ok(()) => format!("/chars/{char_id}?q=remove_ok"),
        Err(e) => format!("/chars/{char_id}?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

#[derive(Template)]
#[template(path = "ops.html")]
struct OpsTemplate {
    msg: String,
    msg_ok: bool,
    ops: Vec<OpsView>,
}

struct OpsView {
    id: i64,
    when: String,
    actor: String,
    action: String,
    target: String,
    summary: String,
    undone: bool,
    undone_info: String,
}

async fn ops_page(
    State(state): State<SharedState>,
    Query(params): Query<SearchParams>,
) -> Result<Html<String>, AppError> {
    let ops = l2o_core::admin_ops::recent_ops(&state.pool, 200)
        .await?
        .into_iter()
        .map(|o| OpsView {
            id: o.id,
            when: o
                .performed_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            actor: o.actor,
            action: o.action,
            target: o.target,
            summary: o.summary,
            undone: o.undone == 1,
            undone_info: o
                .undone_at
                .map(|t| {
                    format!(
                        "{} by {}",
                        t.format("%Y-%m-%d %H:%M"),
                        o.undone_by.unwrap_or_default()
                    )
                })
                .unwrap_or_default(),
        })
        .collect();
    let (msg, msg_ok) = admin_msg(params.q.as_deref(), params.why.as_deref());
    Ok(Html(OpsTemplate { msg, msg_ok, ops }.render()?))
}

async fn ops_undo(
    State(state): State<SharedState>,
    Path(op_id): Path<i64>,
) -> Result<Response, AppError> {
    let result = l2o_core::admin_ops::undo_op(&state.pool, "admin", op_id).await;
    let uri = match result {
        Ok(_) => "/ops?q=undo_ok".to_string(),
        Err(e) => format!("/ops?q=err&why={}", err_code(&e)),
    };
    Ok(Redirect::to(&uri).into_response())
}

async fn moves_page(State(state): State<SharedState>) -> Result<Html<String>, AppError> {
    let rows = l2o_core::inventory::recent_moves(&state.pool, 200).await?;
    let moves = rows
        .into_iter()
        .map(|m| MoveView {
            when: m
                .moved_at
                .map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string())
                .unwrap_or_default(),
            actor: m.actor_login,
            char_id: m.char_id,
            char_name: m.char_name.unwrap_or_default(),
            item_name: state
                .items
                .get(&m.item_id)
                .map(|i| i.name.clone())
                .unwrap_or_else(|| format!("#{}", m.item_id)),
            count: m.count,
            enchant: if m.enchant_level > 0 {
                format!("+{}", m.enchant_level)
            } else {
                String::new()
            },
            from_loc: m.from_loc,
            to_loc: m.to_loc,
        })
        .collect();
    Ok(Html(MovesTemplate { moves }.render()?))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let config = l2o_core::AppConfig::from_env()?;
    let pool = l2o_core::init_pool(&config.database_url).await?;
    tracing::info!("connected to database");

    l2o_core::auth::ensure_schema(&pool).await?;
    l2o_core::portal::ensure_schema(&pool).await?;
    l2o_core::inventory::ensure_schema(&pool).await?;
    l2o_core::admin_ops::ensure_schema(&pool).await?;
    if let Err(e) = l2o_core::auth::purge_expired_sessions(&pool).await {
        tracing::warn!("session purge failed: {e:#}");
    }
    if let Err(e) = l2o_core::portal::purge_expired_sessions(&pool).await {
        tracing::warn!("player session purge failed: {e:#}");
    }
    if l2o_core::auth::count_users(&pool).await? == 0 {
        let password = std::env::var("ADMIN_PASSWORD").unwrap_or_else(|_| l2o_core::auth::random_password());
        l2o_core::auth::create_user(&pool, "admin", &password, "admin").await?;
        if std::env::var("ADMIN_PASSWORD").is_ok() {
            tracing::info!("created initial user 'admin' (password from ADMIN_PASSWORD)");
        } else {
            tracing::warn!("created initial user 'admin' with generated password: {password}");
        }
    }

    let limits = l2o_core::inventory::GameLimits::load(&config.player_ini, &config.offline_trade_ini);
    tracing::info!(
        "storage limits: inventory {}/{}/{} (normal/dwarf/gm), warehouse {}/{}, clan {}, clan withdraw by members: {}",
        limits.inv_no_dwarf,
        limits.inv_dwarf,
        limits.inv_gm,
        limits.wh_no_dwarf,
        limits.wh_dwarf,
        limits.wh_clan,
        limits.members_can_withdraw_from_clanwh
    );
    tracing::info!(
        "offline shops: trade={} craft={} restore={}; sell rate {}%",
        limits.offline_trade_enabled,
        limits.offline_craft_enabled,
        limits.restore_offliners,
        config.sell_rate_percent
    );

    let items = l2o_core::gamedata::load_items(&config.items_xml_dir).unwrap_or_else(|e| {
        tracing::warn!("failed to load item xmls from {:?}: {e:#}", config.items_xml_dir);
        HashMap::new()
    });
    tracing::info!("loaded {} item definitions", items.len());

    let recipes = l2o_core::gamedata::load_recipes(&config.recipes_file).unwrap_or_else(|e| {
        tracing::warn!(
            "failed to load recipes from {:?}: {e:#}",
            config.recipes_file
        );
        HashMap::new()
    });
    tracing::info!("loaded {} recipes", recipes.len());

    let mut materials = HashSet::new();
    for recipe in recipes.values() {
        for (ingredient, _) in &recipe.ingredients {
            materials.insert(*ingredient);
        }
    }
    tracing::info!("derived {} crafting material item ids", materials.len());
    let icons_dir = config.icons_dir.clone();
    if icons_dir.is_dir() {
        tracing::info!("serving item icons from {:?}", icons_dir);
    } else {
        tracing::warn!("icons dir {:?} missing (run: l2o-icons extract ...)", icons_dir);
    }

    let state = Arc::new(AppState {
        pool,
        items,
        recipes,
        materials,
        limits,
        gm_access_level: config.gm_access_level,
        sell_rate_percent: config.sell_rate_percent,
    });

    let protected = Router::new()
        .route("/", get(dashboard))
        .route("/accounts", get(accounts_page))
        .route("/account/{login}", get(account_detail_page))
        .route("/account/{login}/ban", post(account_ban))
        .route("/account/{login}/unban", post(account_unban))
        .route("/characters", get(characters_page))
        .route("/chars/{id}", get(char_detail))
        .route("/chars/{id}/edit", post(char_edit))
        .route("/chars/{id}/restore", post(char_restore))
        .route("/chars/{id}/grant", post(char_grant))
        .route("/chars/{id}/remove", post(char_remove_item))
        .route("/items", get(items_page))
        .route("/moves", get(moves_page))
        .route("/ops", get(ops_page))
        .route("/ops/{id}/undo", post(ops_undo))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    let portal = Router::new()
        .route("/my", get(portal_home))
        .route("/my/char/{id}", get(portal_overview))
        .route("/my/char/{id}/items", get(portal_items_page))
        .route("/my/char/{id}/craft", get(portal_craft_page))
        .route("/my/char/{id}/shop", get(portal_shop_page))
        .route("/my/char/{id}/batch", post(portal_batch))
        .route("/my/char/{id}/craft", post(portal_craft))
        .route("/my/char/{id}/shop", post(portal_shop_set))
        .route("/my/char/{id}/shop/delete", post(portal_shop_delete))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_player_auth));

    let public = Router::new()
        .route("/login", get(login_page).post(login_submit))
        .route("/logout", post(logout))
        .route("/my/login", get(portal_login_page).post(portal_login_submit))
        .route("/my/logout", post(portal_logout));

    let app = public
        .merge(protected)
        .merge(portal)
        .nest_service("/icons", ServeDir::new(&config.icons_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&config.bind_addr).await?;
    tracing::info!("listening on http://{}", config.bind_addr);
    axum::serve(listener, app).await?;

    Ok(())
}
