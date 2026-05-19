use std::{fs, path::Path};

use anyhow::{Context as AnyhowContext, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serenity::all::{ChannelId, Member, UserId};

use crate::state::BotState;

pub(crate) struct ClosedSession {
    pub(crate) channel_id: Option<ChannelId>,
    pub(crate) joined_at: i64,
    pub(crate) duration_seconds: i64,
}

impl BotState {
    pub(crate) async fn upsert_member(&self, member: &Member, now: i64) -> Result<()> {
        let display_name = member.display_name().to_string();
        let user_id = member.user.id.get().to_string();
        let is_bot = i64::from(member.user.bot);

        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO members (user_id, display_name, is_bot, updated_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                display_name = excluded.display_name,
                is_bot = excluded.is_bot,
                updated_at = excluded.updated_at",
            params![user_id, display_name, is_bot, now],
        )?;

        Ok(())
    }

    pub(crate) async fn open_session(
        &self,
        user_id: UserId,
        channel_id: ChannelId,
        joined_at: i64,
    ) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO active_sessions (user_id, channel_id, joined_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET
                channel_id = excluded.channel_id,
                joined_at = excluded.joined_at",
            params![
                user_id.get().to_string(),
                channel_id.get().to_string(),
                joined_at
            ],
        )?;

        Ok(())
    }

    pub(crate) async fn close_session(
        &self,
        user_id: UserId,
        now: i64,
    ) -> Result<Option<ClosedSession>> {
        let user_id_text = user_id.get().to_string();
        let db = self.db.lock().await;

        let session = db
            .query_row(
                "SELECT channel_id, joined_at FROM active_sessions WHERE user_id = ?1",
                params![user_id_text],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;

        let Some((channel_id_text, joined_at)) = session else {
            return Ok(None);
        };

        let duration_seconds = (now - joined_at).max(0);
        db.execute(
            "INSERT INTO voice_totals (user_id, total_seconds, updated_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(user_id) DO UPDATE SET
                total_seconds = voice_totals.total_seconds + excluded.total_seconds,
                updated_at = excluded.updated_at",
            params![user_id_text, duration_seconds, now],
        )?;
        db.execute(
            "DELETE FROM active_sessions WHERE user_id = ?1",
            params![user_id.get().to_string()],
        )?;

        let channel_id = channel_id_text.parse::<u64>().ok().map(ChannelId::new);

        Ok(Some(ClosedSession {
            channel_id,
            joined_at,
            duration_seconds,
        }))
    }

    pub(crate) async fn close_all_stale_sessions(&self, now: i64) -> Result<usize> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare("SELECT user_id, joined_at FROM active_sessions")?;
        let sessions = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        for (user_id, joined_at) in &sessions {
            let duration_seconds = (now - *joined_at).max(0);
            db.execute(
                "INSERT INTO voice_totals (user_id, total_seconds, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(user_id) DO UPDATE SET
                    total_seconds = voice_totals.total_seconds + excluded.total_seconds,
                    updated_at = excluded.updated_at",
                params![user_id, duration_seconds, now],
            )?;
        }

        db.execute("DELETE FROM active_sessions", [])?;
        Ok(sessions.len())
    }

    pub(crate) async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let db = self.db.lock().await;
        let value = db
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        Ok(value)
    }

    pub(crate) async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT INTO settings (key, value)
             VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;

        Ok(())
    }

    pub(crate) async fn add_temp_channel(
        &self,
        channel_id: ChannelId,
        owner_user_id: UserId,
        created_at: i64,
    ) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "INSERT OR REPLACE INTO temp_channels (channel_id, owner_user_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![
                channel_id.get().to_string(),
                owner_user_id.get().to_string(),
                created_at
            ],
        )?;

        Ok(())
    }

    pub(crate) async fn remove_temp_channel(&self, channel_id: ChannelId) -> Result<()> {
        let db = self.db.lock().await;
        db.execute(
            "DELETE FROM temp_channels WHERE channel_id = ?1",
            params![channel_id.get().to_string()],
        )?;

        Ok(())
    }

    pub(crate) async fn is_temp_channel(&self, channel_id: ChannelId) -> Result<bool> {
        let db = self.db.lock().await;
        let exists = db
            .query_row(
                "SELECT 1 FROM temp_channels WHERE channel_id = ?1",
                params![channel_id.get().to_string()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        Ok(exists)
    }

    pub(crate) async fn temp_channels(&self) -> Result<Vec<ChannelId>> {
        let db = self.db.lock().await;
        let mut stmt = db.prepare("SELECT channel_id FROM temp_channels")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|id| id.parse::<u64>().ok().map(ChannelId::new))
            .collect();

        Ok(ids)
    }
}

pub(crate) fn init_db(db: &Connection) -> Result<()> {
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.pragma_update(None, "foreign_keys", "ON")?;
    db.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS members (
            user_id TEXT PRIMARY KEY,
            display_name TEXT NOT NULL,
            is_bot INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS voice_totals (
            user_id TEXT PRIMARY KEY,
            total_seconds INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS active_sessions (
            user_id TEXT PRIMARY KEY,
            channel_id TEXT NOT NULL,
            joined_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS temp_channels (
            channel_id TEXT PRIMARY KEY,
            owner_user_id TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        ",
    )?;

    Ok(())
}

pub(crate) fn prepare_database_path(database_path: &str) -> Result<()> {
    if let Some(parent) = Path::new(database_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create database directory {:?}", parent))?;
        }
    }

    Ok(())
}
