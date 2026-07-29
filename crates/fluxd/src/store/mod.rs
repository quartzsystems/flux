//! Persistence: the Postgres connection pool, the migration runner, and one
//! repository module per aggregate.
//!
//! ## On compile-time query checking
//!
//! sqlx's `query!` macros verify SQL against a live database at compile time. We
//! use the runtime-checked `query_as::<_, T>()` form instead, for one reason:
//! `cargo build` must not require a running Postgres. Bootstrapping the appliance
//! image, CI, and a developer's first clone all happen before any database
//! exists, and a build that fails without one is a build nobody can start from.
//!
//! What we keep from the macro form is the part that matters — every query
//! deserialises into an explicit `FromRow` struct with domain types (`Role`,
//! `PciAddr`), so a schema/struct mismatch surfaces as a clear decode error on
//! the first query rather than as a silently wrong value. The integration tests
//! in `tests/` exercise every statement against a real database.
//!
//! To switch to compile-time checking later: point `DATABASE_URL` at a migrated
//! database, change `query_as` to `query_as!`, run `cargo sqlx prepare`, and
//! commit the generated `.sqlx/` directory.

use std::time::Duration;

use anyhow::Context;
use sqlx::postgres::{PgPool, PgPoolOptions};

pub mod flows;
pub mod models;
pub mod port_groups;
pub mod ports;
pub mod profiles;
pub mod reservations;
pub mod runs;
pub mod sessions;
pub mod settings;
pub mod tests;
pub mod users;

/// Embedded migrations, applied on every daemon start.
static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// A handle to the appliance database.
///
/// Cloning is cheap — the pool is internally reference-counted — so this is
/// stored directly in the shared application state rather than behind an `Arc`.
#[derive(Clone, Debug)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Opens the pool and waits for the database to answer.
    ///
    /// The pool is lazy by default; we force one connection here so a bad
    /// `DATABASE_URL` fails at startup with a clear message instead of surfacing
    /// as a confusing 500 on the first request.
    pub async fn connect(url: &str, max_connections: u32) -> anyhow::Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(Duration::from_secs(10))
            .connect(url)
            .await
            .context("connecting to Postgres")?;

        Ok(Self { pool })
    }

    /// Applies any migrations the database has not seen.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        MIGRATOR.run(&self.pool).await.context("running database migrations")?;
        tracing::info!("database migrations are up to date");
        Ok(())
    }

    /// Borrows the underlying pool for repository calls.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

/// Whether a database error is a unique-constraint violation.
///
/// Callers use this to turn a race on `INSERT` into a 409 rather than a 500:
/// checking for existence first and inserting second is a TOCTOU bug, so we
/// insert and interpret the failure.
pub fn is_unique_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23505"))
}

/// Whether a database error is a foreign-key violation.
///
/// Signals that a referenced row (a port, a user) disappeared between the
/// caller's read and its write.
pub fn is_foreign_key_violation(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("23503"))
}
