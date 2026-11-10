//! `teamctx demo`: loads the bundled fictional team into a SQLite file.
//!
//! The dataset is `fixtures/demo/dataset.json`, compiled into the binary. Everything in it
//! is fictional and, like any chat, untrusted data. The command needs no network, no
//! Discord account and no configuration.
//!
//! Safety rules for the target file:
//! - The default location is a `teamctx` folder in the OS data directory (or the temp
//!   directory if that cannot be found), never the current directory.
//! - A file that already holds data is only reused when it is a teamctx database that
//!   contains nothing but the demo source. Anything else is refused untouched, so a
//!   real database or an unrelated SQLite file is never overwritten.
//! - Running the command again changes nothing.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use teamctx_core::{id::Snowflake, repo::RepoError, source::SourceKind};
use teamctx_store::{
    Store,
    demo::{DemoError, holds_only_source},
};

/// The bundled dataset (fixtures/demo/dataset.json).
const DATASET: &str = include_str!("../../../fixtures/demo/dataset.json");
/// The demo source's external id and name, as the dataset defines them. A test compares
/// these with the dataset so they cannot drift apart.
const DEMO_GUILD_ID: u64 = 1;
const DEMO_SOURCE_NAME: &str = "Lantern Shelf (fictional demo)";

/// Exit code for a mistake in the command line or its inputs.
pub(crate) const EXIT_USAGE: u8 = 2;
/// Exit code for a failure while running (storage or I/O).
pub(crate) const EXIT_FAILURE: u8 = 1;

/// A mistake in the arguments of `demo` or `search`. Values are untrusted: they are
/// shown with `{:?}` escaping, never raw.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ArgError {
    UnknownOption(OsString),
    MissingValue(&'static str),
    InvalidValue(&'static str, &'static str),
    MissingQuery,
    ExtraArgument(OsString),
}

impl fmt::Display for ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownOption(arg) => write!(f, "unrecognised argument {arg:?}"),
            Self::MissingValue(option) => write!(f, "{option} needs a value"),
            Self::InvalidValue(option, reason) => write!(f, "{option} {reason}"),
            Self::MissingQuery => {
                f.write_str("search needs a query, for example: teamctx search \"beta date\"")
            }
            Self::ExtraArgument(arg) => write!(
                f,
                "unexpected extra argument {arg:?}; pass the whole query as one quoted argument"
            ),
        }
    }
}

/// Reads the value after an option.
pub(crate) fn take_value(
    args: &mut impl Iterator<Item = OsString>,
    option: &'static str,
) -> Result<OsString, ArgError> {
    args.next().ok_or(ArgError::MissingValue(option))
}

/// Options of `teamctx demo`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DemoOptions {
    pub(crate) db: Option<PathBuf>,
}

pub(crate) fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<DemoOptions, ArgError> {
    let mut options = DemoOptions::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--db") => options.db = Some(take_value(&mut args, "--db")?.into()),
            _ => return Err(ArgError::UnknownOption(arg)),
        }
    }
    Ok(options)
}

/// The default database file, shared by `demo` and `search`.
///
/// `var` reads an environment variable. A value that is not an absolute path is ignored.
pub(crate) fn default_db_path(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let absolute = |name: &str| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let data_dir = if cfg!(windows) {
        absolute("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        absolute("HOME").map(|home| home.join("Library").join("Application Support"))
    } else {
        absolute("XDG_DATA_HOME")
            .or_else(|| absolute("HOME").map(|home| home.join(".local").join("share")))
    };
    data_dir
        .unwrap_or_else(std::env::temp_dir)
        .join("teamctx")
        .join("demo.db")
}

/// The default database file for this process.
pub(crate) fn default_db_from_env() -> PathBuf {
    default_db_path(|name| std::env::var_os(name))
}

/// Why the target file cannot take the demo.
#[derive(Debug)]
enum TargetError {
    /// The file holds something other than a teamctx database with only the demo.
    NotDemo,
    Io(std::io::Error),
    Store(RepoError),
    Demo(DemoError),
}

impl fmt::Display for TargetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotDemo => f.write_str(
                "that file already holds data that is not the demo, so it was left untouched; \
                 pass a different --db path",
            ),
            Self::Io(error) => write!(f, "cannot prepare the database file: {error}"),
            Self::Store(error) => write!(f, "cannot open the database: {error}"),
            Self::Demo(error) => write!(f, "cannot load the demo: {error}"),
        }
    }
}

/// True if the existing file at `path` is a teamctx database holding only the demo
/// source. Opens read-only, so a foreign file is never migrated or changed.
fn holds_only_demo(path: &Path) -> bool {
    let Ok(store) = Store::open_read_only(path) else {
        return false;
    };
    let Some(guild) = Snowflake::new(DEMO_GUILD_ID) else {
        return false;
    };
    // Every source is checked, not just the first ids: a real source stored under any
    // id, or any member, channel or message of one, makes the file not a plain demo.
    matches!(
        holds_only_source(&store, SourceKind::Discord, guild, DEMO_SOURCE_NAME),
        Ok(true)
    )
}

/// Loads the demo into `db` and returns the message and entry counts.
fn load(db: &Path, create_parent_dir: bool) -> Result<(usize, usize), TargetError> {
    if create_parent_dir && let Some(parent) = db.parent() {
        std::fs::create_dir_all(parent).map_err(TargetError::Io)?;
    }
    let existed = match std::fs::metadata(db) {
        Ok(meta) => meta.len() > 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(TargetError::Io(error)),
    };
    if existed && !holds_only_demo(db) {
        return Err(TargetError::NotDemo);
    }
    let result = import(db);
    if result.is_err() && !existed {
        // A failed first import must not leave a half-built database behind. Cleanup is
        // best effort: the original error is the one to report.
        for suffix in ["", "-wal", "-shm"] {
            let mut name = db.as_os_str().to_owned();
            name.push(suffix);
            let _ = std::fs::remove_file(Path::new(&name));
        }
    }
    result
}

fn import(db: &Path) -> Result<(usize, usize), TargetError> {
    let mut store = Store::open(db).map_err(TargetError::Store)?;
    let map = teamctx_store::demo::import(&mut store, DATASET).map_err(TargetError::Demo)?;
    Ok((map.message_count(), map.entry_count()))
}

/// Runs `teamctx demo` and returns the exit code.
pub(crate) fn run(options: &DemoOptions, out: &mut impl Write, err: &mut impl Write) -> u8 {
    let (db, is_default) = match &options.db {
        Some(path) => (path.clone(), false),
        None => (default_db_from_env(), true),
    };
    let (messages, entries) = match load(&db, is_default) {
        Ok(counts) => counts,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            return match error {
                TargetError::NotDemo => EXIT_USAGE,
                _ => EXIT_FAILURE,
            };
        }
    };
    let db_flag = if is_default {
        String::new()
    } else {
        format!(" --db \"{}\"", db.display())
    };
    let written = (|| {
        writeln!(
            out,
            "Loaded the fictional demo team \"Lantern Shelf\" into {}",
            db.display()
        )?;
        writeln!(
            out,
            "{messages} messages and {entries} record entries. Everything in it is made up; \
             running this again changes nothing."
        )?;
        writeln!(out, "\nTry next:")?;
        writeln!(out, "  teamctx search \"beta\"{db_flag}")?;
        writeln!(out, "  teamctx search \"in:#engineering epub\"{db_flag}")?;
        writeln!(out, "  teamctx search \"after:2026-04-01 crash\"{db_flag}")?;
        writeln!(
            out,
            "\nSearch prints chat text framed as data, so the output is safe to paste into an agent."
        )
    })();
    u8::from(written.is_err())
}

#[cfg(test)]
mod tests {
    use super::*;
    use teamctx_core::{id::SourceId, repo::DirectoryRead};

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn demo_constants_match_the_dataset() {
        let mut store = Store::in_memory().unwrap();
        teamctx_store::demo::import(&mut store, DATASET).unwrap();
        let info = store.source(SourceId::new(1).unwrap()).unwrap().unwrap();
        assert_eq!(info.kind, SourceKind::Discord);
        assert_eq!(info.external_id.get(), DEMO_GUILD_ID);
        assert_eq!(info.name, DEMO_SOURCE_NAME);
        assert!(store.source(SourceId::new(2).unwrap()).unwrap().is_none());
    }

    #[test]
    fn arguments_parse_db_and_reject_the_rest() {
        assert_eq!(parse_args(args(&[])), Ok(DemoOptions { db: None }));
        assert_eq!(
            parse_args(args(&["--db", "x.db"])),
            Ok(DemoOptions {
                db: Some(PathBuf::from("x.db"))
            })
        );
        assert_eq!(
            parse_args(args(&["--db"])),
            Err(ArgError::MissingValue("--db"))
        );
        assert_eq!(
            parse_args(args(&["--bogus"])),
            Err(ArgError::UnknownOption(OsString::from("--bogus")))
        );
    }

    #[test]
    fn default_path_uses_the_data_dir_and_ignores_relative_values() {
        let tail = Path::new("teamctx").join("demo.db");
        let temp = std::env::temp_dir().join(&tail);
        let none = |_: &str| None;
        assert_eq!(default_db_path(none), temp);
        let relative = |_: &str| Some(OsString::from("relative"));
        assert_eq!(default_db_path(relative), temp);

        let root = std::env::temp_dir().join("data-root");
        let set = |_: &str| Some(root.clone().into_os_string());
        let found = default_db_path(set);
        assert!(found.starts_with(&root), "{found:?}");
        assert!(found.ends_with(&tail), "{found:?}");
    }
}
