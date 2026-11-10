//! SQLite storage for teamctx: schema migrations, repositories and FTS5 queries.
//!
//! Boundary: this crate owns the database and is the only place that runs SQL. It
//! depends on `teamctx-core` and nothing else in the workspace. The connector, server and
//! LLM crates reach stored data only through it.

pub mod demo;
mod read;
pub mod usage;
mod write;
mod writer;

pub use writer::{Writer, WriterClient};

use std::{fmt, path::Path, sync::Arc};

use rusqlite::{Connection, OpenFlags};
use teamctx_core::{
    repo::{RepoError, RepoResult},
    time::{CalendarDate, UnixMillis},
};

/// Resolves a search date to midnight in the configured server timezone.
/// The host supplies this; storage never guesses a local timezone or reads a clock.
pub type DateResolver = Arc<dyn Fn(CalendarDate) -> RepoResult<UnixMillis> + Send + Sync>;

/// A SQLite connection. Move the writable instance into [`Writer`] at runtime;
/// create separate read-only instances for query workers.
pub struct Store {
    pub(crate) conn: Connection,
    pub(crate) date_resolver: Option<DateResolver>,
}

impl fmt::Debug for Store {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Store")
            .field("date_resolver_configured", &self.date_resolver.is_some())
            .finish_non_exhaustive()
    }
}

/// Schema migrations in order. Entry `n` takes the database from `user_version` `n` to
/// `n + 1`, so a new migration is one new line here.
const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/0001_init.sql"),
    include_str!("../migrations/0002_record_edits.sql"),
    include_str!("../migrations/0003_llm_usage_tokens.sql"),
];

/// Reads `PRAGMA user_version`, which counts the migrations already applied.
fn schema_version(conn: &Connection) -> RepoResult<usize> {
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(backend)?;
    usize::try_from(version).map_err(|_| invalid("database schema version is invalid"))
}

pub(crate) fn backend(error: impl std::error::Error + Send + Sync + 'static) -> RepoError {
    RepoError::Backend(Box::new(error))
}

pub(crate) fn invalid(message: &'static str) -> RepoError {
    RepoError::Backend(message.into())
}

impl Store {
    /// Opens a file and transactionally applies supported migrations.
    pub fn open(path: impl AsRef<Path>) -> RepoResult<Self> {
        Self::initialize(Connection::open(path).map_err(backend)?)
    }

    /// Opens an isolated in-memory database (journal mode is SQLite's `memory`).
    pub fn in_memory() -> RepoResult<Self> {
        Self::initialize(Connection::open_in_memory().map_err(backend)?)
    }

    fn initialize(mut conn: Connection) -> RepoResult<Self> {
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             PRAGMA busy_timeout=5000;
             PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;",
        )
        .map_err(backend)?;
        let applied = schema_version(&conn)?;
        if applied > MIGRATIONS.len() {
            return Err(invalid("database schema is newer than this binary"));
        }
        for (index, sql) in MIGRATIONS.iter().enumerate().skip(applied) {
            let tx = conn.transaction().map_err(backend)?;
            tx.execute_batch(sql).map_err(backend)?;
            let version = i64::try_from(index + 1).map_err(|_| invalid("too many migrations"))?;
            tx.pragma_update(None, "user_version", version)
                .map_err(backend)?;
            tx.commit().map_err(backend)?;
        }
        Ok(Self {
            conn,
            date_resolver: None,
        })
    }

    /// Opens an existing migrated file without write permission.
    pub fn open_read_only(path: impl AsRef<Path>) -> RepoResult<Self> {
        let conn =
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(backend)?;
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
            .map_err(backend)?;
        if schema_version(&conn)? != MIGRATIONS.len() {
            return Err(invalid("database requires migration by the writer"));
        }
        Ok(Self {
            conn,
            date_resolver: None,
        })
    }

    /// Configures calendar-to-instant conversion before executing date-filtered queries.
    /// Without it date filters return an error; UTC is not silently substituted.
    pub fn set_date_resolver(&mut self, resolver: DateResolver) {
        self.date_resolver = Some(resolver);
    }

    pub(crate) fn date(&self, date: CalendarDate) -> RepoResult<UnixMillis> {
        self.date_resolver.as_ref().ok_or_else(|| {
            invalid("configure server.timezone date resolver before searching dates")
        })?(date)
    }

    pub(crate) fn transaction<T>(
        &mut self,
        f: impl FnOnce(&Connection) -> RepoResult<T>,
    ) -> RepoResult<T> {
        let tx = self.conn.transaction().map_err(backend)?;
        let value = f(&tx)?;
        tx.commit().map_err(backend)?;
        Ok(value)
    }
}
