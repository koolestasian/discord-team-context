//! `teamctx q`: the command-line client of the host's JSON API (plan section 10.4).
//!
//! `teamctx q <context|whats-new|record|search|read> ... [--json]` asks a running host the
//! same questions an agent asks over MCP, and the host answers from the same retrieval
//! layer, so the entries and ids are identical (ADR 0012).
//!
//! Connection settings:
//! - the host URL comes from `--url` or `TEAMCTX_URL`;
//! - the access token comes from `--token-file`, then `TEAMCTX_TOKEN`, then the file named
//!   by `TEAMCTX_TOKEN_FILE`, then `teamctx/token` in the OS data directory when that file
//!   exists. A token file must be a regular file readable only by its owner (mode 0600 on
//!   Unix); a looser file is refused. Windows access control lists cannot be checked
//!   through the standard library, so nothing is checked there.
//!
//! The token is held in a [`Secret`], is sent only as the `Authorization` header, and is
//! never printed or put in an error message. The client does not follow redirects, so the
//! header cannot be sent anywhere the user did not name.
//!
//! Output: the framed text the host returns (what an agent would read), or with `--json`
//! the host's JSON body. Terminal control characters in the text are replaced, so chat
//! cannot rewrite the screen.

use std::ffi::OsString;
use std::fmt;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use teamctx_server::secret::Secret;

use crate::demo::{EXIT_FAILURE, EXIT_USAGE, default_db_path};

const URL_VAR: &str = "TEAMCTX_URL";
const TOKEN_VAR: &str = "TEAMCTX_TOKEN";
const TOKEN_FILE_VAR: &str = "TEAMCTX_TOKEN_FILE";
/// Longest token file read, in bytes. Real tokens are about 40 characters.
const MAX_TOKEN_FILE_BYTES: u64 = 4096;
/// Longest token accepted, in characters.
const MAX_TOKEN_CHARS: usize = 512;
/// Longest response body read. The host keeps results near 8 KB; this only bounds a
/// misbehaving server.
const MAX_BODY_BYTES: usize = 1 << 20;
const TIMEOUT: Duration = Duration::from_secs(30);

/// What to ask the host.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Context {
        text: String,
        limit: Option<u32>,
    },
    WhatsNew {
        since: String,
    },
    Record {
        kind: Option<String>,
        status: Option<String>,
        topic: Option<String>,
        owner: Option<String>,
        since: Option<String>,
        cursor: Option<String>,
    },
    Search {
        query: String,
        cursor: Option<String>,
    },
    Read {
        id: String,
        before: Option<u32>,
        after: Option<u32>,
    },
}

impl Operation {
    /// The request target on the host: path and query string, percent-encoded.
    fn target(&self) -> String {
        let mut query = Query::default();
        let path = match self {
            Self::Context { text, limit } => {
                query.add("text", text);
                query.add_number("limit", *limit);
                "context"
            }
            Self::WhatsNew { since } => {
                query.add("since", since);
                "whats-new"
            }
            Self::Record {
                kind,
                status,
                topic,
                owner,
                since,
                cursor,
            } => {
                query.add_opt("kind", kind.as_deref());
                query.add_opt("status", status.as_deref());
                query.add_opt("q", topic.as_deref());
                query.add_opt("owner", owner.as_deref());
                query.add_opt("since", since.as_deref());
                query.add_opt("cursor", cursor.as_deref());
                "record"
            }
            Self::Search {
                query: text,
                cursor,
            } => {
                query.add("q", text);
                query.add_opt("cursor", cursor.as_deref());
                "search"
            }
            Self::Read { id, before, after } => {
                query.add("id", id);
                query.add_number("before", *before);
                query.add_number("after", *after);
                "read"
            }
        };
        format!("/api/v1/{path}{}", query.finish())
    }
}

/// A query string under construction.
#[derive(Default)]
struct Query(String);

impl Query {
    fn add(&mut self, name: &str, value: &str) {
        self.0.push(if self.0.is_empty() { '?' } else { '&' });
        self.0.push_str(name);
        self.0.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
                self.0.push(char::from(byte));
            } else {
                self.0.push_str(&format!("%{byte:02X}"));
            }
        }
    }

    fn add_opt(&mut self, name: &str, value: Option<&str>) {
        if let Some(value) = value {
            self.add(name, value);
        }
    }

    fn add_number(&mut self, name: &str, value: Option<u32>) {
        if let Some(value) = value {
            self.add(name, &value.to_string());
        }
    }

    fn finish(self) -> String {
        self.0
    }
}

/// Options of `teamctx q`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QOptions {
    operation: Operation,
    json: bool,
    url: Option<String>,
    token_file: Option<PathBuf>,
}

/// A mistake in the arguments of `q`. Values are untrusted and shown with `{:?}`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum QArgError {
    MissingCommand,
    UnknownCommand(OsString),
    UnknownOption(OsString),
    MissingValue(&'static str),
    InvalidValue(&'static str, &'static str),
    MissingArgument(&'static str),
    Extra(OsString),
}

impl fmt::Display for QArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingCommand => f.write_str(
                "q needs a command: context, whats-new, record, search or read \
                 (see 'teamctx --help')",
            ),
            Self::UnknownCommand(arg) => write!(f, "unknown q command {arg:?}"),
            Self::UnknownOption(arg) => write!(f, "unrecognised argument {arg:?}"),
            Self::MissingValue(option) => write!(f, "{option} needs a value"),
            Self::InvalidValue(option, reason) => write!(f, "{option} {reason}"),
            Self::MissingArgument(what) => write!(f, "this command needs {what}"),
            Self::Extra(arg) => write!(
                f,
                "unexpected extra argument {arg:?}; pass the whole value as one quoted argument"
            ),
        }
    }
}

/// Which command, and so which options it takes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Context,
    WhatsNew,
    Record,
    Search,
    Read,
}

/// Raw values collected while parsing, before they become an [`Operation`].
#[derive(Default)]
struct Collected {
    positional: Option<String>,
    limit: Option<u32>,
    before: Option<u32>,
    after: Option<u32>,
    kind: Option<String>,
    status: Option<String>,
    topic: Option<String>,
    owner: Option<String>,
    since: Option<String>,
    cursor: Option<String>,
}

fn text_value(
    args: &mut impl Iterator<Item = OsString>,
    option: &'static str,
) -> Result<String, QArgError> {
    args.next()
        .ok_or(QArgError::MissingValue(option))?
        .into_string()
        .map_err(|_| QArgError::InvalidValue(option, "must be UTF-8 text"))
}

fn number_value(
    args: &mut impl Iterator<Item = OsString>,
    option: &'static str,
) -> Result<u32, QArgError> {
    text_value(args, option)?
        .parse()
        .map_err(|_| QArgError::InvalidValue(option, "must be a whole number"))
}

pub(crate) fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<QOptions, QArgError> {
    let mut args = args.into_iter();
    let name = args.next().ok_or(QArgError::MissingCommand)?;
    let command = match name.to_str() {
        Some("context") => Command::Context,
        Some("whats-new") => Command::WhatsNew,
        Some("record") => Command::Record,
        Some("search") => Command::Search,
        Some("read") => Command::Read,
        _ => return Err(QArgError::UnknownCommand(name)),
    };
    let mut json = false;
    let mut url = None;
    let mut token_file = None;
    let mut got = Collected::default();
    let mut options_ended = false;
    while let Some(arg) = args.next() {
        let flag = if options_ended { None } else { arg.to_str() };
        match (flag, command) {
            (Some("--"), _) => options_ended = true,
            (Some("--json"), _) => json = true,
            (Some("--url"), _) => url = Some(text_value(&mut args, "--url")?),
            (Some("--token-file"), _) => {
                token_file = Some(PathBuf::from(
                    args.next().ok_or(QArgError::MissingValue("--token-file"))?,
                ));
            }
            (Some("--limit"), Command::Context) => {
                got.limit = Some(number_value(&mut args, "--limit")?);
            }
            (Some("--before"), Command::Read) => {
                got.before = Some(number_value(&mut args, "--before")?);
            }
            (Some("--after"), Command::Read) => {
                got.after = Some(number_value(&mut args, "--after")?);
            }
            (Some("--kind"), Command::Record) => got.kind = Some(text_value(&mut args, "--kind")?),
            (Some("--status"), Command::Record) => {
                got.status = Some(text_value(&mut args, "--status")?);
            }
            (Some("--topic"), Command::Record) => {
                got.topic = Some(text_value(&mut args, "--topic")?);
            }
            (Some("--owner"), Command::Record) => {
                got.owner = Some(text_value(&mut args, "--owner")?);
            }
            (Some("--since"), Command::Record) => {
                got.since = Some(text_value(&mut args, "--since")?);
            }
            (Some("--cursor"), Command::Record | Command::Search) => {
                got.cursor = Some(text_value(&mut args, "--cursor")?);
            }
            (Some(option), _) if option.starts_with('-') && option.len() > 1 => {
                return Err(QArgError::UnknownOption(arg));
            }
            _ => {
                if command == Command::Record || got.positional.is_some() {
                    return Err(QArgError::Extra(arg));
                }
                got.positional = Some(
                    arg.into_string()
                        .map_err(|_| QArgError::InvalidValue("argument", "must be UTF-8 text"))?,
                );
            }
        }
    }
    let operation = match command {
        Command::Context => Operation::Context {
            text: need(got.positional, "the branch name or task description")?,
            limit: got.limit,
        },
        Command::WhatsNew => Operation::WhatsNew {
            since: need(
                got.positional,
                "a date like 2026-04-01 or a window like 24h",
            )?,
        },
        Command::Record => Operation::Record {
            kind: got.kind,
            status: got.status,
            topic: got.topic,
            owner: got.owner,
            since: got.since,
            cursor: got.cursor,
        },
        Command::Search => Operation::Search {
            query: need(
                got.positional,
                "a query, for example: teamctx q search \"beta date\"",
            )?,
            cursor: got.cursor,
        },
        Command::Read => Operation::Read {
            id: need(got.positional, "an id such as m:123 or D-12")?,
            before: got.before,
            after: got.after,
        },
    };
    Ok(QOptions {
        operation,
        json,
        url,
        token_file,
    })
}

fn need(value: Option<String>, what: &'static str) -> Result<String, QArgError> {
    value.ok_or(QArgError::MissingArgument(what))
}

/// Why the client could not run. Messages never contain the token.
#[derive(Debug)]
enum ClientError {
    NoUrl,
    BadUrl(&'static str),
    NoToken,
    BadToken(&'static str),
    TokenFile(PathBuf, TokenFileProblem),
    Runtime,
    Connect(String),
    TimedOut,
    Request,
    Unauthorized,
    RateLimited,
    Refused(String),
    Status(u16),
    BadBody,
    TooLarge,
}

#[derive(Debug)]
enum TokenFileProblem {
    Missing,
    Unreadable,
    NotRegular,
    #[cfg_attr(not(unix), expect(dead_code, reason = "only Unix reports file modes"))]
    TooOpen(u32),
    TooLarge,
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoUrl => write!(f, "no host URL; set {URL_VAR} or pass --url"),
            Self::BadUrl(reason) => write!(f, "the host URL {reason}"),
            Self::NoToken => write!(
                f,
                "no access token; set {TOKEN_VAR}, set {TOKEN_FILE_VAR}, or pass --token-file \
                 (create one with 'teamctx token create')"
            ),
            Self::BadToken(reason) => write!(f, "the access token {reason}"),
            Self::TokenFile(path, problem) => {
                let path = path.display().to_string().escape_debug().to_string();
                match problem {
                    TokenFileProblem::Missing => write!(f, "token file \"{path}\" does not exist"),
                    TokenFileProblem::Unreadable => {
                        write!(f, "token file \"{path}\" cannot be read")
                    }
                    TokenFileProblem::NotRegular => {
                        write!(f, "token file \"{path}\" is not a regular file")
                    }
                    TokenFileProblem::TooOpen(mode) => write!(
                        f,
                        "token file \"{path}\" has mode {mode:04o}, so other users could read \
                         it; run: chmod 600 \"{path}\""
                    ),
                    TokenFileProblem::TooLarge => {
                        write!(f, "token file \"{path}\" is too large to be a token")
                    }
                }
            }
            Self::Runtime => f.write_str("cannot start the network runtime"),
            Self::Connect(host) => write!(f, "cannot connect to {host}"),
            Self::TimedOut => f.write_str("the host did not answer in time"),
            Self::Request => f.write_str("the request failed"),
            Self::Unauthorized => f.write_str(
                "the host rejected the token (it may be wrong or revoked); ask the host for a \
                 new one",
            ),
            Self::RateLimited => {
                f.write_str("the host is limiting requests after failed attempts; wait a minute")
            }
            Self::Refused(message) => write!(f, "the host refused the request: {message}"),
            Self::Status(code) => write!(f, "the host answered HTTP {code}"),
            Self::BadBody => f.write_str("the host's answer was not the expected JSON"),
            Self::TooLarge => f.write_str("the host's answer was unexpectedly large"),
        }
    }
}

impl ClientError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::NoUrl | Self::BadUrl(_) | Self::NoToken | Self::BadToken(_) => EXIT_USAGE,
            Self::TokenFile(..) => EXIT_USAGE,
            _ => EXIT_FAILURE,
        }
    }
}

/// Where to reach the host, and what to present.
struct Connection {
    base: String,
    token: Secret<String>,
    /// The URL is plain HTTP to a non-loopback host.
    cleartext_remote: bool,
}

/// Checks and normalises the host URL: `http(s)://host[:port][/prefix]`, no credentials,
/// query or fragment, no trailing slash. Returns the base and whether the token would
/// travel unencrypted to another machine.
fn parse_base_url(text: &str) -> Result<(String, bool), ClientError> {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    let (secure, rest) = if let Some(rest) = lower.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = lower.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(ClientError::BadUrl("must start with http:// or https://"));
    };
    if text.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ClientError::BadUrl(
            "must not contain spaces or control characters",
        ));
    }
    if rest.contains(['?', '#']) {
        return Err(ClientError::BadUrl("must not contain a query or fragment"));
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() {
        return Err(ClientError::BadUrl("needs a host name"));
    }
    if authority.contains('@') {
        return Err(ClientError::BadUrl(
            "must not contain a user name or password; the token is sent as a header",
        ));
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let loopback = host == "localhost" || host == "::1" || host.starts_with("127.");
    Ok((text.trim_end_matches('/').to_owned(), !secure && !loopback))
}

/// Reads a token file: a regular file, readable only by its owner on Unix.
fn read_token_file(path: &Path) -> Result<String, ClientError> {
    let fail = |problem| ClientError::TokenFile(path.to_path_buf(), problem);
    let file = std::fs::File::open(path).map_err(|error| {
        fail(if error.kind() == std::io::ErrorKind::NotFound {
            TokenFileProblem::Missing
        } else {
            TokenFileProblem::Unreadable
        })
    })?;
    // Inspect the open file, not the path, so the file cannot change between the check
    // and the read.
    let metadata = file
        .metadata()
        .map_err(|_| fail(TokenFileProblem::Unreadable))?;
    if !metadata.is_file() {
        return Err(fail(TokenFileProblem::NotRegular));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return Err(fail(TokenFileProblem::TooOpen(mode)));
        }
    }
    let mut text = String::new();
    let read = file
        .take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| fail(TokenFileProblem::Unreadable))?;
    if read as u64 > MAX_TOKEN_FILE_BYTES {
        return Err(fail(TokenFileProblem::TooLarge));
    }
    Ok(text)
}

/// A token is one visible ASCII word, so it cannot carry a header break or other text.
fn check_token(text: &str) -> Result<Secret<String>, ClientError> {
    let token = text.trim();
    if token.is_empty() {
        return Err(ClientError::BadToken("is empty"));
    }
    if token.chars().count() > MAX_TOKEN_CHARS || !token.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ClientError::BadToken(
            "must be a single word of printable ASCII characters",
        ));
    }
    Ok(Secret::new(token.to_owned()))
}

/// Works out the connection from the options and the environment (`var` reads one).
fn resolve(
    options: &QOptions,
    var: impl Fn(&str) -> Option<OsString>,
) -> Result<Connection, ClientError> {
    let text_var = |name: &str| {
        var(name)
            .and_then(|v| v.into_string().ok())
            .filter(|v| !v.trim().is_empty())
    };
    let url = options
        .url
        .clone()
        .or_else(|| text_var(URL_VAR))
        .ok_or(ClientError::NoUrl)?;
    let (base, cleartext_remote) = parse_base_url(&url)?;

    let raw = if let Some(path) = &options.token_file {
        read_token_file(path)?
    } else if let Some(token) = text_var(TOKEN_VAR) {
        token
    } else if let Some(path) = var(TOKEN_FILE_VAR).filter(|v| !v.is_empty()) {
        read_token_file(Path::new(&path))?
    } else {
        let default = default_db_path(&var).with_file_name("token");
        match read_token_file(&default) {
            Ok(text) => text,
            Err(ClientError::TokenFile(_, TokenFileProblem::Missing)) => {
                return Err(ClientError::NoToken);
            }
            Err(other) => return Err(other),
        }
    };
    Ok(Connection {
        base,
        token: check_token(&raw)?,
        cleartext_remote,
    })
}

/// Replaces control characters other than line breaks and tabs, so server text cannot
/// move the cursor, recolour the screen or hide output.
fn printable(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\n' || c == '\t' || !c.is_control() {
                c
            } else {
                '\u{FFFD}'
            }
        })
        .collect()
}

/// Asks the host and returns the JSON body of a `200` answer.
async fn fetch(connection: &Connection, target: &str) -> Result<Value, ClientError> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .user_agent(concat!("teamctx/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| ClientError::Request)?;
    let mut response = client
        .get(format!("{}{target}", connection.base))
        .bearer_auth(connection.token.expose())
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                ClientError::TimedOut
            } else if error.is_connect() {
                ClientError::Connect(connection.base.clone())
            } else {
                ClientError::Request
            }
        })?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ClientError::Request)? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(ClientError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    match status.as_u16() {
        200 => serde_json::from_slice(&body).map_err(|_| ClientError::BadBody),
        401 => Err(ClientError::Unauthorized),
        429 => Err(ClientError::RateLimited),
        400 => {
            let message = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .map(|m| printable(&m).chars().take(300).collect::<String>());
            Err(message.map_or(ClientError::Status(400), ClientError::Refused))
        }
        code => Err(ClientError::Status(code)),
    }
}

fn execute(
    options: &QOptions,
    var: impl Fn(&str) -> Option<OsString>,
    out: &mut impl Write,
    err: &mut impl Write,
) -> Result<(), ClientError> {
    let connection = resolve(options, var)?;
    if connection.cleartext_remote {
        // Best effort: if stderr is gone there is nowhere to say it.
        let _ = writeln!(
            err,
            "warning: {} is plain HTTP to another machine, so the token is not encrypted in \
             transit; use HTTPS or a private network",
            connection.base
        );
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| ClientError::Runtime)?;
    let body = runtime.block_on(fetch(&connection, &options.operation.target()))?;
    let written = if options.json {
        serde_json::to_string(&body)
            .map_err(|_| ClientError::BadBody)
            .map(|json| format!("{json}\n"))
    } else {
        let text = body
            .get("text")
            .and_then(Value::as_str)
            .ok_or(ClientError::BadBody)?;
        Ok(format!("{}\n", printable(text)))
    }?;
    // A closed pipe (such as `| head`) is not an error worth reporting.
    let _ = out.write_all(written.as_bytes());
    Ok(())
}

/// Runs `teamctx q` and returns the exit code.
pub(crate) fn run(options: &QOptions, out: &mut impl Write, err: &mut impl Write) -> u8 {
    match execute(options, |name| std::env::var_os(name), out, err) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<QOptions, QArgError> {
        parse_args(args.iter().map(OsString::from))
    }

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn parses_each_command_and_its_options() {
        let options = parse(&["context", "fix/login", "--limit", "3", "--json"]).unwrap();
        assert!(options.json);
        assert_eq!(
            options.operation,
            Operation::Context {
                text: "fix/login".into(),
                limit: Some(3)
            }
        );
        assert_eq!(
            parse(&["whats-new", "24h"]).unwrap().operation,
            Operation::WhatsNew {
                since: "24h".into()
            }
        );
        assert_eq!(
            parse(&[
                "record", "--kind", "task", "--status", "open", "--topic", "beta", "--owner",
                "Ann", "--since", "7d", "--cursor", "abc"
            ])
            .unwrap()
            .operation,
            Operation::Record {
                kind: Some("task".into()),
                status: Some("open".into()),
                topic: Some("beta".into()),
                owner: Some("Ann".into()),
                since: Some("7d".into()),
                cursor: Some("abc".into()),
            }
        );
        assert_eq!(
            parse(&["search", "--cursor", "c", "a b"])
                .unwrap()
                .operation,
            Operation::Search {
                query: "a b".into(),
                cursor: Some("c".into())
            }
        );
        assert_eq!(
            parse(&["read", "m:5", "--before", "1", "--after", "9"])
                .unwrap()
                .operation,
            Operation::Read {
                id: "m:5".into(),
                before: Some(1),
                after: Some(9)
            }
        );
        let connection = parse(&["search", "x", "--url", "http://h", "--token-file", "t"]).unwrap();
        assert_eq!(connection.url.as_deref(), Some("http://h"));
        assert_eq!(connection.token_file, Some(PathBuf::from("t")));
    }

    #[test]
    fn rejects_bad_command_lines() {
        assert_eq!(parse(&[]), Err(QArgError::MissingCommand));
        assert!(matches!(
            parse(&["nope"]),
            Err(QArgError::UnknownCommand(_))
        ));
        assert!(matches!(
            parse(&["search"]),
            Err(QArgError::MissingArgument(_))
        ));
        assert!(matches!(
            parse(&["search", "a", "b"]),
            Err(QArgError::Extra(_))
        ));
        assert!(matches!(parse(&["record", "x"]), Err(QArgError::Extra(_))));
        // An option of another command is not accepted.
        assert!(matches!(
            parse(&["search", "a", "--limit", "1"]),
            Err(QArgError::UnknownOption(_))
        ));
        assert!(matches!(
            parse(&["read", "m:1", "--kind", "x"]),
            Err(QArgError::UnknownOption(_))
        ));
        assert_eq!(
            parse(&["context", "x", "--limit"]),
            Err(QArgError::MissingValue("--limit"))
        );
        assert!(matches!(
            parse(&["context", "x", "--limit", "many"]),
            Err(QArgError::InvalidValue("--limit", _))
        ));
        assert!(matches!(
            parse(&["context", "x", "--limit", "-1"]),
            Err(QArgError::InvalidValue(..))
        ));
        // `--` ends options, so a query may start with a dash.
        assert_eq!(
            parse(&["search", "--", "-draft"]).unwrap().operation,
            Operation::Search {
                query: "-draft".into(),
                cursor: None
            }
        );
    }

    #[test]
    fn arg_errors_escape_control_characters() {
        let message = QArgError::UnknownOption(OsString::from("-\u{1b}[31m")).to_string();
        assert!(!message.contains('\u{1b}'), "{message:?}");
    }

    #[test]
    fn targets_are_percent_encoded() {
        let target = Operation::Search {
            query: "from:ann \"a&b=c\" #x".into(),
            cursor: Some("MTA".into()),
        }
        .target();
        assert_eq!(
            target,
            "/api/v1/search?q=from%3Aann%20%22a%26b%3Dc%22%20%23x&cursor=MTA"
        );
        let target = Operation::Record {
            kind: None,
            status: None,
            topic: Some("é…".into()),
            owner: None,
            since: None,
            cursor: None,
        }
        .target();
        assert_eq!(target, "/api/v1/record?q=%C3%A9%E2%80%A6");
        assert_eq!(
            Operation::Record {
                kind: None,
                status: None,
                topic: None,
                owner: None,
                since: None,
                cursor: None
            }
            .target(),
            "/api/v1/record"
        );
        assert_eq!(
            Operation::Read {
                id: "D-1".into(),
                before: None,
                after: Some(4)
            }
            .target(),
            "/api/v1/read?id=D-1&after=4"
        );
    }

    #[test]
    fn urls_are_checked_and_cleartext_remote_hosts_are_flagged() {
        let ok = |text: &str| parse_base_url(text).unwrap();
        assert_eq!(
            ok("http://127.0.0.1:7420/"),
            ("http://127.0.0.1:7420".into(), false)
        );
        assert_eq!(
            ok("http://localhost:1"),
            ("http://localhost:1".into(), false)
        );
        assert_eq!(ok("http://[::1]:1"), ("http://[::1]:1".into(), false));
        assert_eq!(
            ok("https://team.example/teamctx/"),
            ("https://team.example/teamctx".into(), false)
        );
        assert_eq!(
            ok("http://team.example"),
            ("http://team.example".into(), true)
        );
        for bad in [
            "team.example",
            "ftp://team.example",
            "http://",
            "http://user:pw@host",
            "http://host/?a=1",
            "http://host/#x",
            "http://ho st",
            "http://ho\nst",
        ] {
            assert!(
                matches!(parse_base_url(bad), Err(ClientError::BadUrl(_))),
                "{bad:?}"
            );
        }
        // Whatever is wrong, the message never repeats the URL.
        let message = parse_base_url("http://user:secret-pw@host")
            .unwrap_err()
            .to_string();
        assert!(!message.contains("secret-pw"));
    }

    #[test]
    fn tokens_must_be_one_printable_word() {
        assert!(check_token(" tctx_abc123\n").is_ok());
        for bad in [
            "",
            "  \n",
            "two words",
            "tab\there",
            "ünï",
            "a\u{1b}b",
            &"x".repeat(600),
        ] {
            let error = check_token(bad).unwrap_err().to_string();
            assert!(error.starts_with("the access token"), "{error}");
            assert!(!error.contains("ünï"));
        }
    }

    #[test]
    fn the_token_comes_from_flag_then_env_then_file() {
        let dir = tempfile::tempdir().unwrap();
        let flag_file = dir.path().join("flag");
        let env_file = dir.path().join("env");
        std::fs::write(&flag_file, "tctx_from_flag_file\n").unwrap();
        std::fs::write(&env_file, "tctx_from_env_file\n").unwrap();
        secure(&flag_file);
        secure(&env_file);
        let env_file_text = env_file.to_str().unwrap();
        let vars = [
            (URL_VAR, "http://127.0.0.1:1/"),
            (TOKEN_VAR, "tctx_from_env"),
            (TOKEN_FILE_VAR, env_file_text),
        ];
        let mut options = parse(&["search", "x"]).unwrap();
        let token = |options: &QOptions, vars: &[(&str, &str)]| {
            resolve(options, env(vars)).map(|c| c.token.expose().clone())
        };
        assert_eq!(token(&options, &vars).unwrap(), "tctx_from_env");
        assert_eq!(
            token(
                &options,
                &vars[..1]
                    .iter()
                    .chain(&vars[2..])
                    .copied()
                    .collect::<Vec<_>>()
            )
            .unwrap(),
            "tctx_from_env_file"
        );
        options.token_file = Some(flag_file);
        assert_eq!(token(&options, &vars).unwrap(), "tctx_from_flag_file");
        assert_eq!(
            resolve(&options, env(&vars)).unwrap().base,
            "http://127.0.0.1:1"
        );
    }

    #[test]
    fn missing_settings_are_usage_errors() {
        let options = parse(&["search", "x"]).unwrap();
        assert!(matches!(
            resolve(&options, env(&[])),
            Err(ClientError::NoUrl)
        ));
        // An empty data directory has no default token file either.
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let vars = [
            (URL_VAR, "http://127.0.0.1:1"),
            ("LOCALAPPDATA", home),
            ("HOME", home),
            ("XDG_DATA_HOME", home),
        ];
        let error = resolve(&options, env(&vars)).err().unwrap();
        assert!(matches!(error, ClientError::NoToken), "{error}");
        assert_eq!(error.exit_code(), EXIT_USAGE);
        // A named token file that is missing is reported as such.
        let named = [
            (URL_VAR, "http://127.0.0.1:1"),
            (TOKEN_FILE_VAR, "/no/such/token"),
        ];
        let error = resolve(&options, env(&named)).err().unwrap();
        assert!(error.to_string().contains("does not exist"), "{error}");
    }

    #[test]
    fn a_default_token_file_is_used_when_it_exists() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().to_str().unwrap();
        let vars = [
            (URL_VAR, "http://127.0.0.1:1"),
            ("LOCALAPPDATA", data),
            ("HOME", data),
            ("XDG_DATA_HOME", data),
        ];
        let options = parse(&["search", "x"]).unwrap();
        let path = default_db_path(env(&vars)).with_file_name("token");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "tctx_default_file").unwrap();
        secure(&path);
        assert_eq!(
            resolve(&options, env(&vars)).unwrap().token.expose(),
            "tctx_default_file"
        );
    }

    #[test]
    fn a_directory_or_oversized_file_is_not_a_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let error = read_token_file(dir.path()).unwrap_err();
        // Opening a directory fails on Windows and succeeds as a non-file on Unix.
        assert!(matches!(error, ClientError::TokenFile(..)), "{error}");
        let big = dir.path().join("big");
        std::fs::write(&big, "x".repeat(5000)).unwrap();
        secure(&big);
        assert!(
            read_token_file(&big)
                .unwrap_err()
                .to_string()
                .contains("too large")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_token_file_open_to_others_is_refused_with_the_fix_and_without_the_token() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "tctx_secret_value_in_file\n").unwrap();
        for mode in [0o644, 0o640, 0o604, 0o666, 0o660] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            let error = read_token_file(&path).unwrap_err().to_string();
            assert!(error.contains("chmod 600"), "{mode:o}: {error}");
            assert!(!error.contains("secret_value"), "{error}");
        }
        for mode in [0o600, 0o400] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert_eq!(
                read_token_file(&path).unwrap().trim(),
                "tctx_secret_value_in_file"
            );
        }
    }

    #[test]
    fn printable_replaces_terminal_controls_but_keeps_lines() {
        assert_eq!(printable("a\nb\tc"), "a\nb\tc");
        assert_eq!(
            printable("x\u{1b}[2Jy\r\u{7}z"),
            "x\u{FFFD}[2Jy\u{FFFD}\u{FFFD}z"
        );
        assert_eq!(printable("… é 日本"), "… é 日本");
    }

    /// Makes a test file owner-only where that can be checked.
    fn secure(path: &Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        #[cfg(not(unix))]
        let _ = path;
    }
}
