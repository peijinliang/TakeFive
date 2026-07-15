use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

use crate::{PersistenceError, Result};

static MIGRATOR: Migrator = sqlx::migrate!("./migrations");

#[derive(Clone, Debug)]
pub struct StoreOptions {
    pub max_connections: u32,
    pub busy_timeout: Duration,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            max_connections: 5,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with_options(path, StoreOptions::default()).await
    }

    pub async fn open_with_options(
        path: impl AsRef<Path>,
        store_options: StoreOptions,
    ) -> Result<Self> {
        let connect_options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        Self::connect_with_options(connect_options, store_options).await
    }

    pub async fn connect_url(database_url: &str) -> Result<Self> {
        let connect_options = SqliteConnectOptions::from_str(database_url)?;
        Self::connect_with_options(connect_options, StoreOptions::default()).await
    }

    async fn connect_with_options(
        connect_options: SqliteConnectOptions,
        store_options: StoreOptions,
    ) -> Result<Self> {
        let connect_options = connect_options
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(store_options.busy_timeout);

        let pool = SqlitePoolOptions::new()
            .max_connections(store_options.max_connections)
            .connect_with(connect_options)
            .await?;

        if let Err(error) = MIGRATOR.run(&pool).await {
            pool.close().await;
            return Err(PersistenceError::Migration(error));
        }

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub async fn schema_version(&self) -> Result<i64> {
        let value: String =
            sqlx::query_scalar("SELECT value FROM schema_meta WHERE key = 'schema_version'")
                .fetch_one(&self.pool)
                .await?;

        value
            .parse()
            .map_err(|error| PersistenceError::Database(sqlx::Error::Decode(Box::new(error))))
    }

    pub async fn quick_check(&self) -> Result<bool> {
        let result: String = sqlx::query_scalar("PRAGMA quick_check")
            .fetch_one(&self.pool)
            .await?;
        Ok(result == "ok")
    }

    /// Reads a JSON-encoded application setting by its stable key.
    pub async fn get_setting_json(&self, key: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query_scalar("SELECT value_json FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Inserts or updates a JSON-encoded application setting.
    ///
    /// The `settings.value_json` CHECK constraint remains the final validation
    /// boundary for callers, while the upsert preserves the revision history
    /// expected by the settings schema.
    pub async fn set_setting_json(
        &self,
        key: &str,
        value_json: &str,
        updated_at_utc: i64,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO settings (key, value_json, revision, updated_at_utc) \
             VALUES (?, ?, 1, ?) \
             ON CONFLICT(key) DO UPDATE SET \
             value_json = excluded.value_json, \
             revision = settings.revision + 1, \
             updated_at_utc = excluded.updated_at_utc",
        )
        .bind(key)
        .bind(value_json)
        .bind(updated_at_utc)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn close(self) {
        self.pool.close().await;
    }
}
