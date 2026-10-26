use anyhow::Result;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use sha1::{Digest, Sha1};
use sqlx::MySqlPool;

const SESSION_TTL_MS: i64 = 12 * 60 * 60 * 1000;

#[derive(Debug, Clone)]
pub struct PlayerSession {
    pub login: String,
    pub access_level: i8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GameAccount {
    pub login: String,
    pub access_level: i8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoginOutcome {
    Ok(GameAccount),
    BadCredentials,
    Banned,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn hash_game_password(password: &str) -> String {
    let mut bytes: Vec<u8> = password
        .chars()
        .map(|c| if (c as u32) <= 0xFF { c as u8 } else { b'?' })
        .collect();
    if bytes.len() > 16 {
        bytes.truncate(16);
    }
    let decoded = String::from_utf8_lossy(&bytes);
    let trimmed = decoded.trim_matches(|c: char| (c as u32) <= 0x20);
    STANDARD.encode(Sha1::digest(trimmed.as_bytes()))
}

async fn is_banned(pool: &MySqlPool, login: &str) -> Result<bool> {
    let row: (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM punishments
        WHERE `key` = ? AND type = 'BAN'
          AND (expiration = 0 OR expiration > ?)
        "#,
    )
    .bind(login)
    .bind(now_ms())
    .fetch_one(pool)
    .await?;
    if row.0 > 0 {
        return Ok(true);
    }

    let temp: Option<(String,)> = sqlx::query_as(
        "SELECT CAST(value AS CHAR) FROM account_data WHERE account_name = ? AND var = 'ban_temp' LIMIT 1",
    )
    .bind(login)
    .fetch_optional(pool)
    .await?;
    if let Some((value,)) = temp {
        if let Ok(end) = value.trim().parse::<i64>() {
            if end > 0 && now_ms() <= end {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub async fn verify_game_login(
    pool: &MySqlPool,
    login: &str,
    password: &str,
) -> Result<LoginOutcome> {
    let row: Option<(String, Option<String>, i8)> = sqlx::query_as(
        "SELECT login, password, accessLevel FROM accounts WHERE login = ? LIMIT 1",
    )
    .bind(login)
    .fetch_optional(pool)
    .await?;

    let Some((login, stored, access_level)) = row else {
        return Ok(LoginOutcome::BadCredentials);
    };
    let Some(stored) = stored else {
        return Ok(LoginOutcome::BadCredentials);
    };
    if stored != hash_game_password(password) {
        return Ok(LoginOutcome::BadCredentials);
    }
    if access_level < 0 {
        return Ok(LoginOutcome::Banned);
    }
    if is_banned(pool, &login).await? {
        return Ok(LoginOutcome::Banned);
    }
    Ok(LoginOutcome::Ok(GameAccount { login, access_level }))
}

pub async fn ensure_schema(pool: &MySqlPool) -> Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS l2o_player_sessions (
            token CHAR(64) NOT NULL PRIMARY KEY,
            login VARCHAR(45) NOT NULL,
            access_level TINYINT NOT NULL DEFAULT 0,
            created_at BIGINT NOT NULL,
            expires_at BIGINT NOT NULL,
            INDEX l2o_player_sessions_expires (expires_at)
        )
        "#,
    )
    .execute(pool)
    .await?;
    if let Err(e) = sqlx::query(
        "ALTER TABLE l2o_player_sessions ADD COLUMN IF NOT EXISTS access_level TINYINT NOT NULL DEFAULT 0",
    )
    .execute(pool)
    .await
    {
        tracing::warn!("could not ensure access_level column: {e}");
    }
    Ok(())
}

pub async fn create_session(pool: &MySqlPool, login: &str, access_level: i8) -> Result<String> {
    use argon2::password_hash::rand_core::{OsRng, RngCore};
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let now = now_ms();
    sqlx::query(
        "INSERT INTO l2o_player_sessions (token, login, access_level, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&token)
    .bind(login)
    .bind(access_level)
    .bind(now)
    .bind(now + SESSION_TTL_MS)
    .execute(pool)
    .await?;
    Ok(token)
}

pub async fn session_login(pool: &MySqlPool, token: &str) -> Result<Option<PlayerSession>> {
    let row: Option<(String, i8)> = sqlx::query_as(
        "SELECT login, access_level FROM l2o_player_sessions WHERE token = ? AND expires_at > ? LIMIT 1",
    )
    .bind(token)
    .bind(now_ms())
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(login, access_level)| PlayerSession {
        login,
        access_level,
    }))
}

pub async fn delete_session(pool: &MySqlPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM l2o_player_sessions WHERE token = ?")
        .bind(token)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn purge_expired_sessions(pool: &MySqlPool) -> Result<()> {
    sqlx::query("DELETE FROM l2o_player_sessions WHERE expires_at < ?")
        .bind(now_ms())
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::hash_game_password;

    #[test]
    fn ascii_password_matches_server_hash() {
        assert_eq!(hash_game_password("1234"), "cRDtpNCeBiql5KOQsKVyrA0sAiA=");
        assert_eq!(hash_game_password("test1234"), "m8NFSdVl2VBbKH3gzSCsd74dPyw=");
    }

    #[test]
    fn umlaut_password_matches_jvm_emulation() {
        assert_eq!(
            hash_game_password("m\u{fc}ll"),
            "C08On3BQh632+6ehIdVTwWmDO/8="
        );
    }

    #[test]
    fn long_password_is_truncated_to_16_bytes() {
        assert_eq!(
            hash_game_password("abcdefghijklmnopEXTRA"),
            hash_game_password("abcdefghijklmnop")
        );
    }
}
