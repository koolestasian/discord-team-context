//! `teamctx token create|list|revoke`: per-person access tokens (plan sections 10.1,
//! 10.3 and 13, ADR 0006).
//!
//! `create` prints the token exactly once, on stdout; only its SHA-256 is stored, so it
//! cannot be shown again. `list` shows name, created, last used and status, never a
//! token or a hash. `revoke` takes effect on the server's next request.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use teamctx_core::repo::{AccessToken, RepoError, TokenRead, TokenWrite};
use teamctx_core::time::UnixMillis;
use teamctx_server::auth::token;
use teamctx_store::Store;

/// Plan section 4: the database file next to the configuration.
const DEFAULT_DB: &str = "teamctx.db";

/// The only scope in v0.1.
const SCOPE: &str = "read";

const MAX_NAME_LEN: usize = 64;

const HELP: &str = "\
Usage: teamctx token <create NAME | list | revoke NAME> [--db PATH]

  create NAME   Create a token for one person or device and print it once
  list          Show names, created, last used and status (never the tokens)
  revoke NAME   Revoke a token; the server rejects it from the next request

Options:
  --db PATH     Database file (default teamctx.db)

NAME: 1-64 characters from a-z, 0-9, '.', '_' and '-', starting with a letter or
digit, for example robin-laptop.
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Help,
    Create(String),
    List,
    Revoke(String),
}

/// A parsed `teamctx token ...` command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Command {
    action: Action,
    db: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UsageError {
    MissingAction,
    MissingName(&'static str),
    MissingDbPath,
    Unknown(OsString),
    Extra(OsString),
    BadName,
}

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `{:?}` escapes control characters, so an argument cannot mangle the terminal.
        match self {
            Self::MissingAction => f.write_str("token needs one of: create, list, revoke"),
            Self::MissingName(action) => write!(f, "token {action} needs a NAME"),
            Self::MissingDbPath => f.write_str("--db needs a PATH"),
            Self::Unknown(arg) => write!(f, "unrecognised token argument {arg:?}"),
            Self::Extra(arg) => write!(f, "unexpected extra argument {arg:?}"),
            Self::BadName => write!(
                f,
                "token NAME must be 1-{MAX_NAME_LEN} characters from a-z, 0-9, '.', '_' and \
                 '-', starting with a letter or digit"
            ),
        }
    }
}

/// Allow-list for names: they appear in terminals, logs and the web view later.
fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_NAME_LEN
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(b))
}

/// Parses the arguments after `token`.
pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, UsageError> {
    let mut action = None;
    let mut name = None;
    let mut db = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => action = Some(Action::Help),
            Some("--db") => db = Some(PathBuf::from(args.next().ok_or(UsageError::MissingDbPath)?)),
            Some(word @ ("create" | "list" | "revoke")) if action.is_none() => {
                action = Some(match word {
                    "create" => Action::Create(String::new()),
                    "revoke" => Action::Revoke(String::new()),
                    _ => Action::List,
                });
            }
            Some(text)
                if name.is_none()
                    && !text.starts_with('-')
                    && matches!(action, Some(Action::Create(_) | Action::Revoke(_))) =>
            {
                if !valid_name(text) {
                    return Err(UsageError::BadName);
                }
                name = Some(text.to_owned());
            }
            _ if action.is_some() && !arg.to_string_lossy().starts_with('-') => {
                return Err(UsageError::Extra(arg));
            }
            _ => return Err(UsageError::Unknown(arg)),
        }
    }
    let action = match (action, name) {
        (None, _) => return Err(UsageError::MissingAction),
        (Some(Action::Help), _) => Action::Help,
        (Some(Action::Create(_)), Some(name)) => Action::Create(name),
        (Some(Action::Revoke(_)), Some(name)) => Action::Revoke(name),
        (Some(Action::Create(_)), None) => return Err(UsageError::MissingName("create")),
        (Some(Action::Revoke(_)), None) => return Err(UsageError::MissingName("revoke")),
        (Some(Action::List), _) => Action::List,
    };
    Ok(Command {
        action,
        db: db.unwrap_or_else(|| PathBuf::from(DEFAULT_DB)),
    })
}

/// Why a token command failed. Messages never contain a token or a hash.
#[derive(Debug)]
enum Failure {
    NoDatabase(PathBuf),
    Duplicate(String),
    NoSuchToken(String),
    Random,
    Storage(PathBuf),
    Output,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDatabase(path) => write!(
                f,
                "no database at {:?}; create a token first or pass --db PATH",
                path.display().to_string()
            ),
            Self::Duplicate(name) => write!(f, "a token named {name:?} already exists"),
            Self::NoSuchToken(name) => write!(f, "no token named {name:?}"),
            Self::Random => f.write_str("the operating system's random number generator failed"),
            Self::Storage(path) => write!(
                f,
                "could not use the database at {:?} (storage backend error)",
                path.display().to_string()
            ),
            Self::Output => f.write_str("could not write the output"),
        }
    }
}

/// Runs a token command and returns the process exit code.
pub(crate) fn run(command: &Command, out: &mut impl Write, err: &mut impl Write) -> u8 {
    match execute(command, now(), out) {
        Ok(()) => 0,
        Err(failure) => {
            // If stderr is gone there is nowhere left to report that.
            let _ = writeln!(err, "error: {failure}");
            1
        }
    }
}

fn now() -> UnixMillis {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    UnixMillis::from_millis(millis)
}

fn execute(command: &Command, now: UnixMillis, out: &mut impl Write) -> Result<(), Failure> {
    let db = command.db.as_path();
    let storage = |_: RepoError| Failure::Storage(db.to_owned());
    match &command.action {
        Action::Help => out.write_all(HELP.as_bytes()).map_err(|_| Failure::Output),
        Action::Create(name) => {
            let new = token::generate().map_err(|_| Failure::Random)?;
            let mut store = Store::open(db).map_err(storage)?;
            store
                .create_token(name, &new.hash, SCOPE, now)
                .map_err(|error| match error {
                    RepoError::Conflict { .. } => Failure::Duplicate(name.clone()),
                    other => storage(other),
                })?;
            write!(
                out,
                "Created token {name:?} (scope: {SCOPE}).\n\
                 Copy it now. It is shown only this once and cannot be recovered:\n\
                 \n\
                 {}\n\
                 \n\
                 Send it as the header: Authorization: Bearer <token>\n\
                 Revoke it with: teamctx token revoke {name}\n",
                new.plaintext.expose()
            )
            .map_err(|_| Failure::Output)
        }
        Action::List => {
            require_existing(db)?;
            let store = Store::open_read_only(db).map_err(storage)?;
            let tokens = store.tokens().map_err(storage)?;
            write_list(out, &tokens).map_err(|_| Failure::Output)
        }
        Action::Revoke(name) => {
            require_existing(db)?;
            let mut store = Store::open(db).map_err(storage)?;
            let token = store.revoke_token(name, now).map_err(|error| match error {
                RepoError::NotFound { .. } => Failure::NoSuchToken(name.clone()),
                other => storage(other),
            })?;
            let when = token.revoked_at.map_or_else(String::new, format_utc);
            let text = if token.revoked_at == Some(now) {
                format!("Revoked token {name:?} at {when}. Requests using it now get 401.\n")
            } else {
                format!("Token {name:?} was already revoked at {when}.\n")
            };
            out.write_all(text.as_bytes()).map_err(|_| Failure::Output)
        }
    }
}

/// `list` and `revoke` never create a database: a wrong `--db` would otherwise look
/// like "no tokens" or "no such token".
fn require_existing(db: &Path) -> Result<(), Failure> {
    if db.is_file() {
        Ok(())
    } else {
        Err(Failure::NoDatabase(db.to_owned()))
    }
}

fn write_list(out: &mut impl Write, tokens: &[AccessToken]) -> io::Result<()> {
    if tokens.is_empty() {
        return out.write_all(b"No tokens. Create one with: teamctx token create NAME\n");
    }
    let width = tokens
        .iter()
        .map(|t| t.name.chars().count())
        .max()
        .unwrap_or(0)
        .max("NAME".len());
    writeln!(
        out,
        "{:<width$}  {:<20}  {:<20}  STATUS",
        "NAME", "CREATED", "LAST USED"
    )?;
    for token in tokens {
        let status = token.revoked_at.map_or_else(
            || "active".to_owned(),
            |at| format!("revoked {}", format_utc(at)),
        );
        writeln!(
            out,
            "{:<width$}  {:<20}  {:<20}  {status}",
            printable(&token.name),
            format_utc(token.created_at),
            token
                .last_used_at
                .map_or_else(|| "never".to_owned(), format_utc),
        )?;
    }
    Ok(())
}

/// Names are validated on create, but the database may have been edited by hand:
/// replace control characters so a name cannot drive the terminal.
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// `YYYY-MM-DD HH:MM UTC`.
fn format_utc(at: UnixMillis) -> String {
    let seconds = at.as_millis().div_euclid(1000);
    let (days, second_of_day) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02} UTC",
        second_of_day / 3600,
        second_of_day % 3600 / 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Command, UsageError> {
        parse(list.iter().map(OsString::from))
    }

    struct Run {
        code: u8,
        out: String,
        err: String,
    }

    fn run_at(command: &Command, at: i64) -> Run {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = match execute(command, UnixMillis::from_millis(at), &mut out) {
            Ok(()) => 0,
            Err(failure) => {
                writeln!(err, "error: {failure}").unwrap();
                1
            }
        };
        Run {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
        }
    }

    fn cmd(action: Action, db: &Path) -> Command {
        Command {
            action,
            db: db.to_owned(),
        }
    }

    /// The one line of `create` output that is the token.
    fn token_in(output: &str) -> String {
        let lines: Vec<&str> = output.lines().filter(|l| l.starts_with("tctx_")).collect();
        assert_eq!(lines.len(), 1, "exactly one token line");
        lines[0].to_owned()
    }

    #[test]
    fn parses_actions_names_and_db() {
        let db = PathBuf::from(DEFAULT_DB);
        assert_eq!(
            args(&["create", "robin-laptop"]),
            Ok(Command {
                action: Action::Create("robin-laptop".to_owned()),
                db: db.clone()
            })
        );
        assert_eq!(
            args(&["--db", "x.db", "revoke", "sam.desk_2"]),
            Ok(Command {
                action: Action::Revoke("sam.desk_2".to_owned()),
                db: PathBuf::from("x.db")
            })
        );
        assert_eq!(
            args(&["list"]),
            Ok(Command {
                action: Action::List,
                db
            })
        );
        assert_eq!(
            args(&["list", "--help"]).map(|c| c.action),
            Ok(Action::Help)
        );
    }

    #[test]
    fn rejects_bad_command_lines() {
        assert_eq!(args(&[]), Err(UsageError::MissingAction));
        assert_eq!(args(&["create"]), Err(UsageError::MissingName("create")));
        assert_eq!(args(&["revoke"]), Err(UsageError::MissingName("revoke")));
        assert_eq!(args(&["list", "--db"]), Err(UsageError::MissingDbPath));
        assert_eq!(
            args(&["list", "extra"]),
            Err(UsageError::Extra(OsString::from("extra")))
        );
        assert_eq!(
            args(&["create", "a", "b"]),
            Err(UsageError::Extra(OsString::from("b")))
        );
        assert_eq!(
            args(&["delete"]),
            Err(UsageError::Unknown(OsString::from("delete")))
        );
        assert_eq!(
            args(&["create", "--force"]),
            Err(UsageError::Unknown(OsString::from("--force")))
        );
    }

    #[test]
    fn names_are_allow_listed() {
        for good in ["a", "robin-laptop", "ci.runner_2", "0day", &"x".repeat(64)] {
            assert!(valid_name(good), "{good:?}");
        }
        for bad in [
            "",
            "Robin",
            "-robin",
            ".robin",
            "robin laptop",
            "robin\u{1b}[31m",
            "röbin",
            "robin/laptop",
            &"x".repeat(65),
        ] {
            assert!(!valid_name(bad), "{bad:?}");
            // A leading '-' reads as an unknown option; anything else as a bad name.
            let expected = if bad.starts_with('-') {
                UsageError::Unknown(OsString::from(bad))
            } else {
                UsageError::BadName
            };
            assert_eq!(args(&["create", bad]).err(), Some(expected), "{bad:?}");
        }
    }

    #[test]
    fn create_prints_the_token_once_and_stores_only_its_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("teamctx.db");
        let run = run_at(&cmd(Action::Create("robin-laptop".to_owned()), &db), 1_000);
        assert_eq!(run.code, 0, "{}", run.err);
        let token = token_in(&run.out);
        assert!(token::is_well_formed(token.as_bytes()));
        assert_eq!(run.out.matches(token.as_str()).count(), 1);
        assert!(run.err.is_empty());

        // The store finds it by hash, with read scope.
        let store = Store::open_read_only(&db).unwrap();
        let row = store
            .active_token(&token::hash(token.as_bytes()))
            .unwrap()
            .expect("stored");
        assert_eq!(
            (row.name.as_str(), row.scopes.as_str()),
            ("robin-laptop", "read")
        );

        // Neither the token nor its hex body is anywhere in the database file.
        drop(store);
        let bytes = std::fs::read(&db).unwrap();
        let body = &token.as_bytes()[5..];
        assert!(!bytes.windows(body.len()).any(|w| w == body));
    }

    #[test]
    fn duplicate_names_are_rejected_without_printing_a_token() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("teamctx.db");
        let create = cmd(Action::Create("robin-laptop".to_owned()), &db);
        assert_eq!(run_at(&create, 1_000).code, 0);
        let again = run_at(&create, 2_000);
        assert_eq!(again.code, 1);
        assert!(again.out.is_empty(), "{}", again.out);
        assert_eq!(
            again.err,
            "error: a token named \"robin-laptop\" already exists\n"
        );
        assert!(!again.err.contains("tctx_"));
        let tokens = Store::open_read_only(&db).unwrap().tokens().unwrap();
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn list_shows_metadata_but_never_a_token_or_hash() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("teamctx.db");
        let first = run_at(&cmd(Action::Create("robin-laptop".to_owned()), &db), 0);
        let second = run_at(
            &cmd(Action::Create("sam-desktop".to_owned()), &db),
            86_400_000,
        );
        let tokens = [token_in(&first.out), token_in(&second.out)];
        run_at(
            &cmd(Action::Revoke("sam-desktop".to_owned()), &db),
            90_000_000,
        );

        let list = run_at(&cmd(Action::List, &db), 100_000_000);
        assert_eq!(list.code, 0, "{}", list.err);
        assert_eq!(
            list.out,
            "NAME          CREATED               LAST USED             STATUS\n\
             robin-laptop  1970-01-01 00:00 UTC  never                 active\n\
             sam-desktop   1970-01-02 00:00 UTC  never                 revoked 1970-01-02 01:00 UTC\n"
        );
        for token in &tokens {
            assert!(!list.out.contains(&token[5..]));
            let hash = token::hash(token.as_bytes());
            let hex: String = hash.0.iter().map(|b| format!("{b:02x}")).collect();
            assert!(!list.out.contains(&hex[..16]));
        }
        assert!(!list.out.contains("tctx_"));
    }

    #[test]
    fn revoke_makes_the_token_inactive_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("teamctx.db");
        let created = run_at(&cmd(Action::Create("robin-laptop".to_owned()), &db), 0);
        let token = token_in(&created.out);
        let revoke = cmd(Action::Revoke("robin-laptop".to_owned()), &db);

        let first = run_at(&revoke, 60_000);
        assert_eq!(first.code, 0);
        assert_eq!(
            first.out,
            "Revoked token \"robin-laptop\" at 1970-01-01 00:01 UTC. Requests using it now get 401.\n"
        );
        let store = Store::open_read_only(&db).unwrap();
        assert_eq!(
            store.active_token(&token::hash(token.as_bytes())).unwrap(),
            None
        );

        let second = run_at(&revoke, 120_000);
        assert_eq!(second.code, 0);
        assert_eq!(
            second.out,
            "Token \"robin-laptop\" was already revoked at 1970-01-01 00:01 UTC.\n"
        );

        let missing = run_at(&cmd(Action::Revoke("nobody".to_owned()), &db), 0);
        assert_eq!(missing.code, 1);
        assert_eq!(missing.err, "error: no token named \"nobody\"\n");
    }

    #[test]
    fn list_and_revoke_never_create_a_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("absent.db");
        for action in [Action::List, Action::Revoke("robin-laptop".to_owned())] {
            let run = run_at(&cmd(action, &db), 0);
            assert_eq!(run.code, 1);
            assert!(run.err.starts_with("error: no database at "), "{}", run.err);
        }
        assert!(!db.exists());
        let empty = dir.path().join("empty.db");
        drop(Store::open(&empty).unwrap());
        let list = run_at(&cmd(Action::List, &empty), 0);
        assert_eq!(
            list.out,
            "No tokens. Create one with: teamctx token create NAME\n"
        );
    }

    #[test]
    fn list_escapes_control_characters_in_stored_names() {
        let row = AccessToken {
            id: teamctx_core::id::TokenId::new(1).unwrap(),
            name: "evil\u{1b}[2Jname".to_owned(),
            scopes: "read".to_owned(),
            created_at: UnixMillis::from_millis(0),
            last_used_at: None,
            revoked_at: None,
        };
        let mut out = Vec::new();
        write_list(&mut out, &[row]).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.contains('\u{1b}'), "{text:?}");
        assert!(text.contains("evil?[2Jname"));
    }

    #[test]
    fn utc_formatting_matches_known_dates() {
        let cases = [
            (0, "1970-01-01 00:00 UTC"),
            (951_782_400_000, "2000-02-29 00:00 UTC"),
            (1_793_923_199_000, "2026-11-05 23:59 UTC"),
            (-1, "1969-12-31 23:59 UTC"),
        ];
        for (millis, text) in cases {
            assert_eq!(
                format_utc(UnixMillis::from_millis(millis)),
                text,
                "{millis}"
            );
        }
    }
}
