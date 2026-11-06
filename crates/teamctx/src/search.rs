//! `teamctx search`: searches a database from the command line.
//!
//! The query uses the plan section 9 syntax (`from:`, `in:#`, `before:`, `after:`,
//! `"phrases"`). Messages and record entries are searched, ranked together with
//! `teamctx_core::rank`, and printed with their citation.
//!
//! Chat is untrusted data. Everything a person wrote (author and channel names, message
//! text, entry titles and tags) goes through `teamctx_core::framing::frame`, so it cannot
//! forge a line or a boundary in the output. Only values the program itself produces (ids,
//! status words, dates and numeric links) are printed outside a frame.
//!
//! Dates: `before:` and `after:` mean midnight in `--timezone`. No timezone database is
//! built in, so only `UTC` and fixed offsets such as `+02:00` are supported.

use std::ffi::OsString;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use teamctx_core::{
    citation::{CitationId, DiscordMessageLink},
    framing,
    query::{self, ParseQueryError},
    rank,
    record::RecordEntry,
    repo::{
        DirectoryRead, EntryHit, MessageHit, MessageRead, RecordRead, RepoError, StoredMessage,
    },
    time::UnixMillis,
};
use teamctx_store::{DateResolver, Store};

use crate::demo::{ArgError, EXIT_FAILURE, EXIT_USAGE, default_db_from_env, take_value};

/// Results printed when `--limit` is not given.
const DEFAULT_LIMIT: u32 = 5;
/// Most results `--limit` accepts.
const MAX_LIMIT: u32 = 50;
const MILLIS_PER_DAY: i64 = 86_400_000;

/// A fixed offset from UTC, the only kind of timezone this build understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixedOffset {
    seconds: i32,
}

const UTC: FixedOffset = FixedOffset { seconds: 0 };

impl FixedOffset {
    fn millis(self) -> i64 {
        i64::from(self.seconds) * 1000
    }
}

/// Parses `UTC`, `Z` or `+HH:MM` / `-HH:MM` (at most 14:00). A named zone such as
/// `America/Toronto` needs a timezone database, so it gets its own error.
fn parse_timezone(text: &str) -> Result<FixedOffset, ArgError> {
    const OPTION: &str = "--timezone";
    if text.eq_ignore_ascii_case("utc") || text == "Z" {
        return Ok(UTC);
    }
    if let Some(rest) = text.strip_prefix(['+', '-']) {
        let sign = if text.starts_with('-') { -1 } else { 1 };
        if let Some((hours, minutes)) = rest.split_once(':') {
            let digits = |part: &str| {
                (part.len() == 2 && part.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| part.parse::<i32>().ok())
                    .flatten()
            };
            if let (Some(hours), Some(minutes)) = (digits(hours), digits(minutes)) {
                let total = hours * 60 + minutes;
                if minutes < 60 && total <= 14 * 60 {
                    return Ok(FixedOffset {
                        seconds: sign * total * 60,
                    });
                }
            }
        }
        return Err(ArgError::InvalidValue(
            OPTION,
            "offset must look like +02:00 or -05:30 and be at most 14 hours",
        ));
    }
    let looks_named = text.starts_with(|c: char| c.is_ascii_alphabetic())
        && text.bytes().all(|b| b.is_ascii_graphic());
    if looks_named {
        return Err(ArgError::InvalidValue(
            OPTION,
            "names a time zone (like America/Toronto), but this build has no time zone \
             database; use UTC or a fixed offset such as +02:00",
        ));
    }
    Err(ArgError::InvalidValue(
        OPTION,
        "must be UTC or a fixed offset such as +02:00",
    ))
}

/// Options of `teamctx search`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SearchOptions {
    query: String,
    db: Option<PathBuf>,
    limit: u32,
    timezone: FixedOffset,
}

pub(crate) fn parse_args(
    args: impl IntoIterator<Item = OsString>,
) -> Result<SearchOptions, ArgError> {
    let mut query = None;
    let mut db = None;
    let mut limit = DEFAULT_LIMIT;
    let mut timezone = UTC;
    let mut args = args.into_iter();
    let mut options_ended = false;
    while let Some(arg) = args.next() {
        let flag = if options_ended { None } else { arg.to_str() };
        match flag {
            Some("--") => options_ended = true,
            Some("--db") => db = Some(take_value(&mut args, "--db")?.into()),
            Some("--limit") => {
                let value = take_value(&mut args, "--limit")?;
                limit = value
                    .to_str()
                    .and_then(|text| text.parse::<u32>().ok())
                    .filter(|n| (1..=MAX_LIMIT).contains(n))
                    .ok_or(ArgError::InvalidValue(
                        "--limit",
                        "must be a whole number from 1 to 50",
                    ))?;
            }
            Some("--timezone") => {
                let value = take_value(&mut args, "--timezone")?;
                timezone = parse_timezone(value.to_str().ok_or(ArgError::InvalidValue(
                    "--timezone",
                    "must be UTC or a fixed offset such as +02:00",
                ))?)?;
            }
            Some(option) if option.starts_with('-') && option.len() > 1 => {
                return Err(ArgError::UnknownOption(arg));
            }
            _ => {
                if query.is_some() {
                    return Err(ArgError::ExtraArgument(arg));
                }
                query = Some(
                    arg.into_string()
                        .map_err(|_| ArgError::InvalidValue("query", "must be UTF-8 text"))?,
                );
            }
        }
    }
    Ok(SearchOptions {
        query: query.ok_or(ArgError::MissingQuery)?,
        db,
        limit,
        timezone,
    })
}

/// Why a search could not run.
#[derive(Debug)]
enum SearchError {
    NoDatabase(PathBuf),
    BadQuery(ParseQueryError),
    Open,
    Store(RepoError),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDatabase(path) => write!(
                f,
                "no database at {}; run 'teamctx demo' to create the demo database, \
                 or pass --db PATH",
                path.display()
            ),
            Self::BadQuery(error) => write!(f, "bad query: {error}"),
            Self::Open => f.write_str(
                "cannot open the database: it is not a teamctx database, or it was made by \
                 another version",
            ),
            Self::Store(error) => write!(f, "search failed: {error}"),
        }
    }
}

impl SearchError {
    fn exit_code(&self) -> u8 {
        match self {
            Self::NoDatabase(_) | Self::BadQuery(_) => EXIT_USAGE,
            Self::Open | Self::Store(_) => EXIT_FAILURE,
        }
    }
}

/// Midnight in the given offset, for `before:` and `after:`.
fn date_resolver(offset: FixedOffset) -> DateResolver {
    Arc::new(move |date| {
        Ok(UnixMillis::from_millis(
            date.start_of_day_utc()
                .as_millis()
                .saturating_sub(offset.millis()),
        ))
    })
}

/// Formats an instant as `YYYY-MM-DD` in the given offset.
fn format_date(at: UnixMillis, offset: FixedOffset) -> String {
    let local = at.as_millis().saturating_add(offset.millis());
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
    let z = local.div_euclid(MILLIS_PER_DAY) + 719_468;
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
    format!("{year:04}-{month:02}-{day:02}")
}

fn now() -> UnixMillis {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    UnixMillis::from_millis(millis)
}

/// One candidate, before ranking.
enum Found {
    Entry(EntryHit),
    Message(MessageHit),
}

struct Ranked {
    score: f64,
    created_at: UnixMillis,
    found: Found,
}

impl Ranked {
    fn new(found: Found, now: UnixMillis) -> Self {
        let (bm25, created_at, is_record) = match &found {
            Found::Entry(hit) => (hit.bm25, hit.entry.created_at, true),
            Found::Message(hit) => (hit.bm25, hit.message.created_at, false),
        };
        Self {
            score: rank::score(bm25, created_at, now, is_record),
            created_at,
            found,
        }
    }

    /// Stable tie-break: records before messages, then the lower id.
    fn tie_break(&self) -> (u8, i64) {
        match &self.found {
            Found::Entry(hit) => (0, hit.entry.id.get()),
            Found::Message(hit) => (1, hit.message.id.get()),
        }
    }
}

/// Best score first, then newest, then the stable tie-break.
fn order(a: &Ranked, b: &Ranked) -> std::cmp::Ordering {
    b.score
        .total_cmp(&a.score)
        .then_with(|| b.created_at.cmp(&a.created_at))
        .then_with(|| a.tie_break().cmp(&b.tie_break()))
}

/// Who wrote it and where, plus a link when the message is known. All chat-derived.
struct Origin {
    author: String,
    channel: String,
    link: Option<DiscordMessageLink>,
}

fn origin(store: &Store, message: &StoredMessage) -> Result<Origin, RepoError> {
    let author = match message.author {
        Some(id) => store.member(id)?.map(|m| m.display_name),
        None => None,
    };
    let channel = store.channel(message.channel)?;
    let link = match &channel {
        Some(channel) => store
            .source(channel.source)?
            .map(|source| DiscordMessageLink {
                guild: source.external_id,
                channel: channel.external_id,
                message: message.external_id,
            }),
        None => None,
    };
    Ok(Origin {
        author: author.unwrap_or_else(|| "unknown".to_owned()),
        channel: channel.map_or_else(|| "unknown".to_owned(), |c| c.name),
        link,
    })
}

/// Indents a framed block under its result heading.
fn indent(block: &str) -> String {
    block
        .lines()
        .map(|line| {
            if line.is_empty() {
                String::new()
            } else {
                format!("   {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn cite_line(id: CitationId, link: Option<DiscordMessageLink>, missing: &str) -> String {
    match link {
        Some(link) => format!("{} {link}", id.marker()),
        None => format!("{} ({missing})", id.marker()),
    }
}

fn render_message(
    store: &Store,
    hit: &MessageHit,
    date: &str,
) -> Result<(String, String), RepoError> {
    let message = &hit.message;
    let origin = origin(store, message)?;
    let heading = format!("m:{}  message  {date}", message.id);
    let cite = cite_line(
        CitationId::Message(message.id),
        origin.link,
        "source unavailable",
    );
    let bundle = format!(
        "author: {}\nchannel: #{}\ntext: {}",
        origin.author, origin.channel, message.content
    );
    Ok((
        heading,
        format!("{cite}\n{}", indent(&framing::frame(&bundle))),
    ))
}

fn render_entry(
    store: &Store,
    entry: &RecordEntry,
    date: &str,
) -> Result<(String, String), RepoError> {
    let source = match entry.source_message {
        Some(id) => store.message(id)?,
        None => None,
    };
    let origin = match &source {
        Some(message) => Some(origin(store, message)?),
        None => None,
    };
    let (author, channel) = match &origin {
        Some(o) => (o.author.clone(), format!("#{}", o.channel)),
        None => {
            let creator = store
                .member(entry.created_by)?
                .map_or_else(|| "unknown".to_owned(), |m| m.display_name);
            (creator, "none".to_owned())
        }
    };
    let id = entry.short_id();
    let heading = format!(
        "{id}  {} ({})  {date}",
        entry.kind.as_str(),
        entry.status.as_str()
    );
    let missing = if entry.source_deleted {
        "source message was deleted"
    } else {
        "no source message"
    };
    let cite = cite_line(CitationId::Entry(id), origin.and_then(|o| o.link), missing);
    let tags = entry
        .tags
        .iter()
        .map(|tag| tag.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let bundle = format!(
        "author: {author}\nchannel: {channel}\ntitle: {}\ntags: {tags}",
        entry.title.as_str()
    );
    Ok((
        heading,
        format!("{cite}\n{}", indent(&framing::frame(&bundle))),
    ))
}

/// Runs the search and returns the text to print.
fn search(options: &SearchOptions) -> Result<String, SearchError> {
    let db = options.db.clone().unwrap_or_else(default_db_from_env);
    if !Path::new(&db).is_file() {
        return Err(SearchError::NoDatabase(db));
    }
    let parsed = query::parse(&options.query).map_err(SearchError::BadQuery)?;
    let mut store = Store::open_read_only(&db).map_err(|_| SearchError::Open)?;
    store.set_date_resolver(date_resolver(options.timezone));

    let now = now();
    let candidates = options.limit.saturating_mul(4).max(40);
    let mut ranked: Vec<Ranked> = Vec::new();
    for hit in store
        .search_entries(&parsed, candidates)
        .map_err(SearchError::Store)?
    {
        ranked.push(Ranked::new(Found::Entry(hit), now));
    }
    for hit in store
        .search_messages(&parsed, candidates)
        .map_err(SearchError::Store)?
    {
        ranked.push(Ranked::new(Found::Message(hit), now));
    }
    ranked.sort_by(order);
    ranked.truncate(options.limit as usize);

    if ranked.is_empty() {
        return Ok("No results.\n".to_owned());
    }
    let mut text = format!(
        "{} result(s), best first. Chat text is framed as data.\n",
        ranked.len()
    );
    for (index, item) in ranked.iter().enumerate() {
        let date = format_date(item.created_at, options.timezone);
        let (heading, body) = match &item.found {
            Found::Entry(hit) => render_entry(&store, &hit.entry, &date),
            Found::Message(hit) => render_message(&store, hit, &date),
        }
        .map_err(SearchError::Store)?;
        text.push_str(&format!("\n{}. {heading}\n   {body}\n", index + 1));
    }
    Ok(text)
}

/// Runs `teamctx search` and returns the exit code.
pub(crate) fn run(options: &SearchOptions, out: &mut impl Write, err: &mut impl Write) -> u8 {
    match search(options) {
        Ok(text) => u8::from(out.write_all(text.as_bytes()).is_err()),
        Err(error) => {
            let _ = writeln!(err, "error: {error}");
            error.exit_code()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use teamctx_core::time::CalendarDate;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn offset(text: &str) -> FixedOffset {
        parse_timezone(text).unwrap()
    }

    #[test]
    fn timezone_accepts_utc_and_fixed_offsets() {
        assert_eq!(offset("UTC"), UTC);
        assert_eq!(offset("utc"), UTC);
        assert_eq!(offset("Z"), UTC);
        assert_eq!(offset("+02:00").seconds, 7200);
        assert_eq!(offset("-05:30").seconds, -(5 * 3600 + 1800));
        assert_eq!(offset("+14:00").seconds, 14 * 3600);
        assert_eq!(offset("-00:00"), UTC);
    }

    #[test]
    fn named_timezones_get_a_specific_error() {
        for name in [
            "America/Toronto",
            "Europe/Berlin",
            "EST",
            "UTC+02:00",
            "GMT",
        ] {
            let message = parse_timezone(name).unwrap_err().to_string();
            assert!(
                message.contains("no time zone database"),
                "{name}: {message}"
            );
            assert!(
                message.contains("UTC or a fixed offset"),
                "{name}: {message}"
            );
        }
    }

    #[test]
    fn malformed_timezones_are_rejected() {
        for bad in [
            "", "+2", "+0200", "+02:0", "+02:60", "+15:00", "+14:30", "2", "+aa:bb", "☃",
        ] {
            assert!(parse_timezone(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn dates_resolve_to_midnight_in_the_offset() {
        let date: CalendarDate = "2026-04-01".parse().unwrap();
        let utc_midnight = date.start_of_day_utc().as_millis();
        for (zone, shift_hours) in [("UTC", 0), ("+02:00", -2), ("-05:00", 5)] {
            let resolve = date_resolver(offset(zone));
            let at = resolve(date).unwrap().as_millis();
            assert_eq!(at, utc_midnight + shift_hours * 3_600_000, "{zone}");
        }
    }

    #[test]
    fn format_date_handles_epoch_leap_days_and_offsets() {
        let at = |millis: i64| UnixMillis::from_millis(millis);
        assert_eq!(format_date(at(0), UTC), "1970-01-01");
        assert_eq!(format_date(at(-1), UTC), "1969-12-31");
        assert_eq!(format_date(at(1_700_000_000_000), UTC), "2023-11-14");
        let leap = "2024-02-29".parse::<CalendarDate>().unwrap();
        assert_eq!(format_date(leap.start_of_day_utc(), UTC), "2024-02-29");
        // 23:30 UTC on March 31 is already April 1 at +02:00.
        let late = "2026-03-31"
            .parse::<CalendarDate>()
            .unwrap()
            .start_of_day_utc();
        let at_2330 = at(late.as_millis() + 23 * 3_600_000 + 1_800_000);
        assert_eq!(format_date(at_2330, UTC), "2026-03-31");
        assert_eq!(format_date(at_2330, offset("+02:00")), "2026-04-01");
        // Extreme values must not panic.
        let _ = format_date(at(i64::MAX), offset("+14:00"));
        let _ = format_date(at(i64::MIN), offset("-05:00"));
    }

    #[test]
    fn arguments_parse_query_and_options() {
        let options = parse_args(args(&[
            "from:Wren beta",
            "--db",
            "x.db",
            "--limit",
            "7",
            "--timezone",
            "+01:00",
        ]))
        .unwrap();
        assert_eq!(options.query, "from:Wren beta");
        assert_eq!(options.db, Some(PathBuf::from("x.db")));
        assert_eq!(options.limit, 7);
        assert_eq!(options.timezone.seconds, 3600);
        let defaults = parse_args(args(&["beta"])).unwrap();
        assert_eq!(
            (defaults.limit, defaults.timezone, defaults.db),
            (5, UTC, None)
        );
    }

    #[test]
    fn arguments_reject_mistakes() {
        assert_eq!(parse_args(args(&[])), Err(ArgError::MissingQuery));
        assert_eq!(
            parse_args(args(&["a", "b"])),
            Err(ArgError::ExtraArgument(OsString::from("b")))
        );
        assert_eq!(
            parse_args(args(&["--bogus", "a"])),
            Err(ArgError::UnknownOption(OsString::from("--bogus")))
        );
        assert_eq!(
            parse_args(args(&["a", "--limit"])),
            Err(ArgError::MissingValue("--limit"))
        );
        for bad in ["0", "51", "-1", "x", ""] {
            assert!(
                matches!(
                    parse_args(args(&["a", "--limit", bad])),
                    Err(ArgError::InvalidValue("--limit", _))
                ),
                "{bad:?}"
            );
        }
        assert!(parse_args(args(&["a", "--timezone", "America/Toronto"])).is_err());
    }

    #[test]
    fn double_dash_allows_a_query_that_starts_with_a_dash() {
        let options = parse_args(args(&["--limit", "2", "--", "-foo"])).unwrap();
        assert_eq!(options.query, "-foo");
        assert_eq!(options.limit, 2);
    }

    #[test]
    fn indent_keeps_blank_lines_empty() {
        assert_eq!(indent("a\n\nb"), "   a\n\n   b");
    }
}
