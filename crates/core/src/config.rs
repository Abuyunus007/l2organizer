use anyhow::{Context, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub bind_addr: String,
    pub items_xml_dir: PathBuf,
    pub icons_dir: PathBuf,
    pub player_ini: PathBuf,
    pub gm_access_level: i8,
    pub recipes_file: PathBuf,
    pub offline_trade_ini: PathBuf,
    pub sell_rate_percent: i64,
}

impl AppConfig {
    pub fn from_env() -> Result<Self> {
        dotenvy::dotenv().ok();

        Ok(Self {
            database_url: std::env::var("DATABASE_URL")
                .context("DATABASE_URL is not set (copy .env.example to .env)")?,
            bind_addr: std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:9001".to_string()),
            items_xml_dir: std::env::var("ITEMS_XML_DIR")
                .unwrap_or_else(|_| "C:/L2Server/game/data/stats/items".to_string())
                .into(),
            icons_dir: std::env::var("ICONS_DIR")
                .unwrap_or_else(|_| "assets/item-icons".to_string())
                .into(),
            player_ini: std::env::var("PLAYER_INI")
                .unwrap_or_else(|_| "C:/L2Server/game/config/Player.ini".to_string())
                .into(),
            gm_access_level: std::env::var("GM_ACCESS_LEVEL")
                .ok()
                .and_then(|v| v.parse::<i32>().ok())
                .map(|v| v.clamp(0, 127) as i8)
                .unwrap_or(50),
            recipes_file: std::env::var("RECIPES_FILE")
                .unwrap_or_else(|_| "C:/L2Server/game/data/Recipes.xml".to_string())
                .into(),
            offline_trade_ini: std::env::var("OFFLINE_TRADE_INI")
                .unwrap_or_else(|_| {
                    "C:/L2Server/game/config/Custom/OfflineTrade.ini".to_string()
                })
                .into(),
            sell_rate_percent: std::env::var("SELL_RATE_PERCENT")
                .ok()
                .and_then(|v| v.parse::<i64>().ok())
                .map(|v| v.clamp(0, 500))
                .unwrap_or(50),
        })
    }
}
