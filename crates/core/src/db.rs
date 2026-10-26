use anyhow::Result;
use sqlx::mysql::{MySqlPool, MySqlPoolOptions};

pub async fn init_pool(database_url: &str) -> Result<MySqlPool> {
    let pool = MySqlPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;

    Ok(pool)
}
