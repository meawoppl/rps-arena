use anyhow::{Context, Result};
use diesel::pg::PgConnection;
use diesel::r2d2::{self, ConnectionManager, Pool};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use std::env;
use std::time::Duration;

pub type DbPool = Pool<ConnectionManager<PgConnection>>;

/// Embedded database migrations — compiled into the binary.
pub const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations");

const DEFAULT_POOL_MAX_SIZE: u32 = 16;
const DEFAULT_POOL_MIN_IDLE: u32 = 2;
const DEFAULT_POOL_CONNECTION_TIMEOUT_SECS: u64 = 5;
const DEFAULT_POOL_IDLE_TIMEOUT_SECS: u64 = 300;
const DEFAULT_POOL_MAX_LIFETIME_SECS: u64 = 1800;

pub fn create_pool() -> Result<DbPool> {
    let database_url = env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let config = PoolConfig::from_env()?;
    let manager = ConnectionManager::<PgConnection>::new(database_url);
    let pool = r2d2::Pool::builder()
        .max_size(config.max_size)
        .min_idle(Some(config.min_idle))
        .connection_timeout(Duration::from_secs(config.connection_timeout_secs))
        .idle_timeout(Some(Duration::from_secs(config.idle_timeout_secs)))
        .max_lifetime(Some(Duration::from_secs(config.max_lifetime_secs)))
        .build(manager)
        .context("failed to create database connection pool")?;
    Ok(pool)
}

/// Run pending database migrations. Returns the list of applied migration names.
pub fn run_migrations(pool: &DbPool) -> Result<Vec<String>> {
    let mut conn = pool.get()?;
    let applied: Vec<String> = conn
        .run_pending_migrations(MIGRATIONS)
        .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?
        .iter()
        .map(|m| m.to_string())
        .collect();
    Ok(applied)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PoolConfig {
    max_size: u32,
    min_idle: u32,
    connection_timeout_secs: u64,
    idle_timeout_secs: u64,
    max_lifetime_secs: u64,
}

impl PoolConfig {
    fn from_env() -> Result<Self> {
        let config = Self {
            max_size: env_u32("DATABASE_POOL_MAX_SIZE", DEFAULT_POOL_MAX_SIZE)?,
            min_idle: env_u32("DATABASE_POOL_MIN_IDLE", DEFAULT_POOL_MIN_IDLE)?,
            connection_timeout_secs: env_u64(
                "DATABASE_POOL_CONNECTION_TIMEOUT_SECS",
                DEFAULT_POOL_CONNECTION_TIMEOUT_SECS,
            )?,
            idle_timeout_secs: env_u64(
                "DATABASE_POOL_IDLE_TIMEOUT_SECS",
                DEFAULT_POOL_IDLE_TIMEOUT_SECS,
            )?,
            max_lifetime_secs: env_u64(
                "DATABASE_POOL_MAX_LIFETIME_SECS",
                DEFAULT_POOL_MAX_LIFETIME_SECS,
            )?,
        };
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            self.max_size > 0,
            "DATABASE_POOL_MAX_SIZE must be greater than 0"
        );
        anyhow::ensure!(
            self.min_idle <= self.max_size,
            "DATABASE_POOL_MIN_IDLE must be less than or equal to DATABASE_POOL_MAX_SIZE"
        );
        anyhow::ensure!(
            self.connection_timeout_secs > 0,
            "DATABASE_POOL_CONNECTION_TIMEOUT_SECS must be greater than 0"
        );
        anyhow::ensure!(
            self.idle_timeout_secs > 0,
            "DATABASE_POOL_IDLE_TIMEOUT_SECS must be greater than 0"
        );
        anyhow::ensure!(
            self.max_lifetime_secs > 0,
            "DATABASE_POOL_MAX_LIFETIME_SECS must be greater than 0"
        );
        Ok(())
    }
}

fn env_u32(name: &str, default: u32) -> Result<u32> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u32>()
            .with_context(|| format!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} must be valid UTF-8"),
    }
}

fn env_u64(name: &str, default: u64) -> Result<u64> {
    match env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .with_context(|| format!("{name} must be an unsigned integer")),
        Err(env::VarError::NotPresent) => Ok(default),
        Err(env::VarError::NotUnicode(_)) => anyhow::bail!("{name} must be valid UTF-8"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_pool_config_is_bounded() {
        let config = PoolConfig {
            max_size: DEFAULT_POOL_MAX_SIZE,
            min_idle: DEFAULT_POOL_MIN_IDLE,
            connection_timeout_secs: DEFAULT_POOL_CONNECTION_TIMEOUT_SECS,
            idle_timeout_secs: DEFAULT_POOL_IDLE_TIMEOUT_SECS,
            max_lifetime_secs: DEFAULT_POOL_MAX_LIFETIME_SECS,
        };

        assert_eq!(config.max_size, 16);
        assert_eq!(config.min_idle, 2);
        assert_eq!(config.connection_timeout_secs, 5);
        config.validate().unwrap();
    }

    #[test]
    fn min_idle_cannot_exceed_max_size() {
        let config = PoolConfig {
            max_size: 2,
            min_idle: 3,
            connection_timeout_secs: 5,
            idle_timeout_secs: 300,
            max_lifetime_secs: 1800,
        };

        assert!(config.validate().is_err());
    }
}
