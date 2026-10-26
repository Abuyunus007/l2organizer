use anyhow::{bail, Context, Result};
use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use sqlx::MySqlPool;

const SESSION_TTL_MS: i64 = 12 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub id: i32,
    pub username: String,
    pub role: String,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub async fn ensure_schema(pool: &MySqlPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_users (
            id INT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            username VARCHAR(45) NOT NULL UNIQUE,
            password_hash VARCHAR(255) NOT NULL,
            role VARCHAR(20) NOT NULL DEFAULT 'admin',
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_sessions (
            token CHAR(64) NOT NULL PRIMARY KEY,
            user_id INT NOT NULL,
            created_at BIGINT NOT NULL,
            expires_at BIGINT NOT NULL,
            INDEX l2o_sessions_expires (expires_at)
        )
        "#,
    )
    .execute(pool)
    .await?;

    if let Err(e) = sqlx::query(
        "ALTER TABLE l2o_users ADD COLUMN IF NOT EXISTS source VARCHAR(10) NOT NULL DEFAULT 'local'",
    )
    .execute(pool)
    .await
    {
        tracing::warn!("could not ensure source column: {e}");
    }

    Ok(())
}

pub async fn count_users(pool: &MySqlPool) -> Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM l2o_users")
        .fetch_one(pool)
        .await?;
    Ok(row.0)
}

pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hashing failed: {e}"))?
        .to_string();
    Ok(hash)
}

pub async fn create_user(pool: &MySqlPool, username: &str, password: &str, role: &str) -> Result<()> {
    if username.trim().is_empty() {
        bail!("username must not be empty");
    }
    if password.len() < 8 {
        bail!("password must be at least 8 characters");
    }
    let hash = hash_password(password)?;
    sqlx::query("INSERT INTO l2o_users (username, password_hash, role) VALUES (?, ?, ?)")
        .bind(username)
        .bind(hash)
        .bind(role)
        .execute(pool)
        .await
        .context("inserting user")?;
    Ok(())
}

pub async fn verify_login(
    pool: &MySqlPool,
    username: &str,
    password: &str,
) -> Result<Option<AuthUser>> {
    let row: Option<(i32, String, String, String)> = sqlx::query_as(
        "SELECT id, username, password_hash, role FROM l2o_users WHERE username = ? AND source = 'local' LIMIT 1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;

    let Some((id, username, hash, role)) = row else {
        return Ok(None);
    };
    let parsed = match PasswordHash::new(&hash) {
        Ok(parsed) => parsed,
        Err(e) => {
            tracing::warn!("stored hash for '{username}' is invalid: {e}");
            return Ok(None);
        }
    };
    if Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
    {
        Ok(Some(AuthUser {
            id,
            username,
            role,
        }))
    } else {
        Ok(None)
    }
}

pub async fn find_or_create_game_gm(pool: &MySqlPool, username: &str) -> Result<i32> {
    let existing: Option<(i32,)> =
        sqlx::query_as("SELECT id FROM l2o_users WHERE username = ? AND source = 'game' LIMIT 1")
            .bind(username)
            .fetch_optional(pool)
            .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let result = sqlx::query(
        "INSERT INTO l2o_users (username, password_hash, role, source) VALUES (?, '!game-account!', 'gm', 'game')",
    )
    .bind(username)
    .execute(pool)
    .await?;
    Ok(result.last_insert_id() as i32)
}

pub fn random_password() -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut bytes = [0u8; 16];
    OsRng.fill_bytes(&mut bytes);
    bytes
        .iter()
        .map(|b| CHARS[*b as usize % CHARS.len()] as char)
        .collect()
}

pub async fn create_session(pool: &MySqlPool, user_id: i32) -> Result<String> {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let now = now_ms();
    sqlx::query("INSERT INTO l2o_sessions (token, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)")
        .bind(&token)
        .bind(user_id)
        .bind(now)
        .bind(now + SESSION_TTL_MS)
        .execute(pool)
        .await?;
    Ok(token)
}

pub async fn session_user(pool: &MySqlPool, token: &str) -> Result<Option<AuthUser>> {
    let row: Option<(i32, String, String)> = sqlx::query_as(
        r#"
        SELECT u.id, u.username, u.role
        FROM l2o_sessions s
        JOIN l2o_users u ON u.id = s.user_id
        WHERE s.token = ? AND s.expires_at > ?
        LIMIT 1
        "#,
    )
    .bind(token)
    .bind(now_ms())
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|(id, username, role)| AuthUser {
        id,
        username,
        role,
    }))
}

pub async fn delete_session(pool: &MySqlPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM l2o_sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn purge_expired_sessions(pool: &MySqlPool) -> Result<()> {
    sqlx::query("DELETE FROM l2o_sessions WHERE expires_at < ?")
        .bind(now_ms())
        .execute(pool)
        .await?;
    Ok(())
}
