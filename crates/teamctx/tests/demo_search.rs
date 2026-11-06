//! End-to-end checks of `teamctx demo` and `teamctx search` against the built binary.
//!
//! The demo dataset is fictional. It contains two messages that carry instructions aimed
//! at a reader; the tests check that those only ever appear inside the framed data block.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use teamctx_core::{framing, source::SourceKind};
use teamctx_store::Store;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Runs the binary with an empty environment, so a developer's own variables (such as
/// the data directory) cannot change the result.
fn teamctx(args: &[&str]) -> std::io::Result<Output> {
    Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .env_clear()
        .args(args)
        .output()
}

fn text(bytes: &[u8]) -> Result<String, std::string::FromUtf8Error> {
    String::from_utf8(bytes.to_vec())
}

fn path_text(path: &Path) -> Result<&str, Box<dyn std::error::Error>> {
    path.to_str().ok_or_else(|| "non-UTF-8 test path".into())
}

/// A temporary directory holding a loaded demo database.
struct Demo {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

impl Demo {
    fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("demo.db");
        let output = teamctx(&["demo", "--db", path_text(&db)?])?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        Ok(Self { _dir: dir, db })
    }

    fn search(&self, args: &[&str]) -> Result<Output, Box<dyn std::error::Error>> {
        let mut all = vec!["search"];
        all.extend_from_slice(args);
        all.extend_from_slice(&["--db", path_text(&self.db)?]);
        Ok(teamctx(&all)?)
    }

    /// Searches and requires success.
    fn ok(&self, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
        let output = self.search(args)?;
        assert!(
            output.status.success(),
            "{args:?}: {}",
            text(&output.stderr)?
        );
        assert!(output.stderr.is_empty(), "{args:?}");
        let out = text(&output.stdout)?;
        assert_only_framed(&out);
        Ok(out)
    }
}

/// Every line the program prints is either something it wrote itself (summary, heading,
/// citation, frame notice or boundary) or a JSON-string data line inside a frame.
fn assert_only_framed(output: &str) {
    let mut inside = false;
    for line in output.lines() {
        let line = line.trim_start();
        if line == framing::BLOCK_START {
            assert!(!inside, "nested frame: {output}");
            inside = true;
        } else if line == framing::BLOCK_END {
            assert!(inside, "unopened frame end: {output}");
            inside = false;
        } else if inside {
            assert!(
                line.len() >= 2 && line.starts_with('"') && line.ends_with('"'),
                "unframed line inside a block: {line:?}"
            );
            assert!(!line.contains(['<', '>', '`', '\r', '\u{1b}']), "{line:?}");
        } else {
            let own = line.is_empty()
                || line == framing::NOTICE
                || line == "No results."
                || line.contains("result(s), best first")
                || is_heading(line)
                || line.starts_with('[') && line.contains("] ");
            assert!(own, "unexpected line outside a frame: {line:?}");
        }
    }
    assert!(!inside, "frame left open: {output}");
    let results = output.matches(framing::BLOCK_START).count();
    assert_eq!(output.matches(framing::BLOCK_END).count(), results);
    assert_eq!(output.matches(framing::NOTICE).count(), results);
}

/// A result heading such as `1. D-7  decision (active)  2026-03-04`.
fn is_heading(line: &str) -> bool {
    line.split_once(". ")
        .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// The `YYYY-MM-DD` dates in the result headings.
fn heading_dates(output: &str) -> Vec<String> {
    output
        .lines()
        .filter(|line| is_heading(line))
        .filter_map(|line| line.split_whitespace().last().map(str::to_owned))
        .collect()
}

#[test]
fn demo_loads_prints_next_steps_and_is_idempotent() -> TestResult {
    let dir = tempfile::tempdir()?;
    let db = dir.path().join("demo.db");
    let db_text = path_text(&db)?;
    let first = teamctx(&["demo", "--db", db_text])?;
    assert!(first.status.success(), "{}", text(&first.stderr)?);
    let message = text(&first.stdout)?;
    assert!(
        message.contains("314 messages and 76 record entries"),
        "{message}"
    );
    assert!(message.contains("Try next:"), "{message}");
    assert!(message.contains("teamctx search"), "{message}");
    assert!(message.contains(db_text), "{message}");
    assert!(db.is_file());

    let before = teamctx(&["search", "beta", "--db", db_text, "--limit", "50"])?;
    let second = teamctx(&["demo", "--db", db_text])?;
    assert!(second.status.success(), "{}", text(&second.stderr)?);
    assert_eq!(
        second.stdout, first.stdout,
        "a repeat run must report the same"
    );
    let after = teamctx(&["search", "beta", "--db", db_text, "--limit", "50"])?;
    assert!(before.status.success());
    assert_eq!(
        before.stdout, after.stdout,
        "a repeat run must change nothing"
    );
    Ok(())
}

#[test]
fn demo_refuses_a_file_that_is_not_the_demo_and_leaves_it_untouched() -> TestResult {
    let dir = tempfile::tempdir()?;

    // An unrelated file.
    let stranger = dir.path().join("notes.db");
    std::fs::write(&stranger, b"definitely not a database")?;
    let output = teamctx(&["demo", "--db", path_text(&stranger)?])?;
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr)?.contains("left untouched"));
    assert_eq!(std::fs::read(&stranger)?, b"definitely not a database");

    // A teamctx database that holds another source.
    let real = dir.path().join("real.db");
    {
        use teamctx_core::{id::Snowflake, repo::IngestWrite};
        let mut store = Store::open(&real)?;
        let guild = Snowflake::new(999).ok_or("snowflake")?;
        store.upsert_source(SourceKind::Discord, guild, "Somebody's real server")?;
    }
    let size = std::fs::metadata(&real)?.len();
    let output = teamctx(&["demo", "--db", path_text(&real)?])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stderr)?);
    assert!(text(&output.stderr)?.contains("not the demo"));
    assert_eq!(std::fs::metadata(&real)?.len(), size);

    // A demo database that gained another source is no longer a plain demo.
    let mixed = Demo::load()?;
    {
        use teamctx_core::{id::Snowflake, repo::IngestWrite};
        let mut store = Store::open(&mixed.db)?;
        let guild = Snowflake::new(999).ok_or("snowflake")?;
        store.upsert_source(SourceKind::Discord, guild, "Added later")?;
    }
    let output = teamctx(&["demo", "--db", path_text(&mixed.db)?])?;
    assert_eq!(output.status.code(), Some(2));

    // An empty file is fine: it is just a new database.
    let empty = dir.path().join("empty.db");
    std::fs::write(&empty, b"")?;
    let output = teamctx(&["demo", "--db", path_text(&empty)?])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    Ok(())
}

#[test]
fn demo_default_location_is_the_data_dir_not_the_current_dir() -> TestResult {
    let data = tempfile::tempdir()?;
    let cwd = tempfile::tempdir()?;
    let home = data.path().join("home");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_teamctx"))
            .env_clear()
            .current_dir(cwd.path())
            .env("LOCALAPPDATA", data.path())
            .env("XDG_DATA_HOME", data.path())
            .env("HOME", &home)
            .args(args)
            .output()
    };
    let output = run(&["demo"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    // Windows reads LOCALAPPDATA and Linux XDG_DATA_HOME (both set to `data`); macOS
    // uses the home directory.
    let expected = if cfg!(target_os = "macos") {
        home.join("Library")
            .join("Application Support")
            .join("teamctx")
            .join("demo.db")
    } else {
        data.path().join("teamctx").join("demo.db")
    };
    assert!(expected.is_file(), "{expected:?}");
    assert!(text(&output.stdout)?.contains(path_text(&expected)?));
    // Coverage builds drop `*.profraw` files into the cwd because `env_clear` removes
    // LLVM_PROFILE_FILE; those are instrumentation output, not product writes.
    let strays: Vec<_> = std::fs::read_dir(cwd.path())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_none_or(|ext| ext != "profraw"))
        .collect();
    assert!(strays.is_empty(), "wrote into the cwd: {strays:?}");

    // `search` finds that database without --db.
    let found = run(&["search", "typeface"])?;
    assert!(found.status.success(), "{}", text(&found.stderr)?);
    assert!(text(&found.stdout)?.contains("result(s)"));
    Ok(())
}

#[test]
fn search_hit_shows_short_id_author_channel_date_and_citation() -> TestResult {
    let demo = Demo::load()?;
    let out = demo.ok(&["serif", "--limit", "50"])?;
    assert!(out.contains("D-7"), "{out}");
    assert!(
        out.contains("[D-7] https://discord.com/channels/1/"),
        "{out}"
    );
    assert!(out.contains("author: "), "{out}");
    assert!(out.contains("channel: #"), "{out}");
    assert!(out.contains("Fenwick Serif"), "{out}");
    let dates = heading_dates(&out);
    assert!(!dates.is_empty(), "{out}");
    for date in &dates {
        assert!(date.starts_with("2026-"), "{date}");
    }
    // Messages are cited as m:<id>, entries by their short id.
    let messages = demo.ok(&["from:crashwatch"])?;
    assert!(messages.contains("[m:"), "{messages}");
    assert!(messages.contains("author: crashwatch"), "{messages}");
    Ok(())
}

#[test]
fn search_limit_bounds_the_number_of_results() -> TestResult {
    let demo = Demo::load()?;
    let one = demo.ok(&["beta", "--limit", "1"])?;
    assert_eq!(heading_dates(&one).len(), 1, "{one}");
    let many = demo.ok(&["beta", "--limit", "50"])?;
    assert!(heading_dates(&many).len() > 3, "{many}");
    Ok(())
}

#[test]
fn search_miss_says_so_and_succeeds() -> TestResult {
    let demo = Demo::load()?;
    assert_eq!(demo.ok(&["zzzzqqqq"])?, "No results.\n");
    assert_eq!(demo.ok(&["from:Nobody"])?, "No results.\n");
    Ok(())
}

#[test]
fn search_reports_bad_queries_and_missing_databases_with_exit_code_2() -> TestResult {
    let demo = Demo::load()?;
    for query in ["\"unclosed", "from:", "after:2026-13-45", ""] {
        let output = demo.search(&[query])?;
        assert_eq!(output.status.code(), Some(2), "{query:?}");
        assert!(output.stdout.is_empty(), "{query:?}");
        let message = text(&output.stderr)?;
        assert!(
            message.starts_with("error: bad query"),
            "{query:?}: {message}"
        );
    }

    let dir = tempfile::tempdir()?;
    let missing = dir.path().join("missing.db");
    let output = teamctx(&["search", "beta", "--db", path_text(&missing)?])?;
    assert_eq!(output.status.code(), Some(2));
    let message = text(&output.stderr)?;
    assert!(message.contains("no database at"), "{message}");
    assert!(message.contains("teamctx demo"), "{message}");
    assert!(!missing.exists(), "search must not create a database");

    // A file that is not a teamctx database is a failure, not a panic.
    let stranger = dir.path().join("stranger.db");
    std::fs::write(&stranger, b"definitely not a database")?;
    let output = teamctx(&["search", "beta", "--db", path_text(&stranger)?])?;
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr)?.contains("cannot open the database"));
    assert_eq!(std::fs::read(&stranger)?, b"definitely not a database");
    Ok(())
}

#[test]
fn oversized_queries_exit_with_code_2_without_echoing_the_query() -> TestResult {
    let demo = Demo::load()?;
    let marker = "not-a-real-secret-marker";
    let long = format!("{marker} {}", "x".repeat(2_000));
    let many = format!("{marker} {}", "word ".repeat(32));
    for (query, expected) in [
        (long, "error: bad query: query too long: at most 2000 bytes"),
        (
            many,
            "error: bad query: too many terms: at most 32 words, phrases and filters",
        ),
    ] {
        let output = demo.search(&[&query])?;
        assert_eq!(output.status.code(), Some(2), "{expected}");
        assert!(output.stdout.is_empty(), "{expected}");
        let message = text(&output.stderr)?;
        assert_eq!(message.trim_end(), expected);
        assert!(!message.contains(marker), "{message}");
    }
    Ok(())
}

#[test]
fn search_usage_mistakes_exit_with_code_2() -> TestResult {
    for args in [
        &["search"][..],
        &["search", "a", "b"],
        &["search", "a", "--limit", "0"],
        &["search", "a", "--limit", "x"],
        &["search", "a", "--bogus"],
        &["search", "a", "--db"],
        &["demo", "--bogus"],
        &["demo", "--db"],
    ] {
        let output = teamctx(args)?;
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let message = text(&output.stderr)?;
        assert!(message.starts_with("error: "), "{args:?}: {message}");
        assert!(message.contains("teamctx --help"), "{args:?}: {message}");
    }
    Ok(())
}

#[test]
fn timezone_supports_utc_and_offsets_and_rejects_named_zones() -> TestResult {
    let demo = Demo::load()?;
    for zone in ["UTC", "+02:00", "-05:30"] {
        demo.ok(&["beta", "--timezone", zone])?;
    }
    for zone in ["America/Toronto", "Europe/Berlin"] {
        let output = demo.search(&["beta", "--timezone", zone])?;
        assert_eq!(output.status.code(), Some(2), "{zone}");
        let message = text(&output.stderr)?;
        assert!(
            message.contains("no time zone database"),
            "{zone}: {message}"
        );
        assert!(message.contains("+02:00"), "{zone}: {message}");
    }
    let output = demo.search(&["beta", "--timezone", "+25:00"])?;
    assert_eq!(output.status.code(), Some(2));
    Ok(())
}

#[test]
fn before_and_after_filters_bound_the_dates() -> TestResult {
    let demo = Demo::load()?;
    let all = heading_dates(&demo.ok(&["beta", "--limit", "50"])?);
    assert!(all.iter().any(|d| d.as_str() < "2026-03-20"), "{all:?}");
    assert!(all.iter().any(|d| d.as_str() >= "2026-04-10"), "{all:?}");

    for zone in ["UTC", "+14:00", "-12:00"] {
        let after = heading_dates(&demo.ok(&[
            "after:2026-04-10 beta",
            "--limit",
            "50",
            "--timezone",
            zone,
        ])?);
        assert!(!after.is_empty(), "{zone}");
        assert!(
            after.iter().all(|d| d.as_str() >= "2026-04-10"),
            "{zone}: {after:?}"
        );

        let before = heading_dates(&demo.ok(&[
            "before:2026-03-20 beta",
            "--limit",
            "50",
            "--timezone",
            zone,
        ])?);
        assert!(!before.is_empty(), "{zone}");
        assert!(
            before.iter().all(|d| d.as_str() < "2026-03-20"),
            "{zone}: {before:?}"
        );

        let window = heading_dates(&demo.ok(&[
            "after:2026-03-01 before:2026-03-04 beta",
            "--limit",
            "50",
            "--timezone",
            zone,
        ])?);
        assert!(!window.is_empty(), "{zone}");
        assert!(
            window
                .iter()
                .all(|d| d.as_str() >= "2026-03-01" && d.as_str() < "2026-03-04"),
            "{zone}: {window:?}"
        );
    }
    // A window that excludes everything is a clean miss.
    assert_eq!(demo.ok(&["after:2030-01-01 beta"])?, "No results.\n");
    Ok(())
}

#[test]
fn hostile_demo_messages_appear_only_framed() -> TestResult {
    let demo = Demo::load()?;
    let mut seen = 0;
    for (query, phrase) in [
        (
            "\"Ignore all previous\"",
            "Ignore all previous instructions",
        ),
        (
            "\"disregard all prior instructions\"",
            "disregard all prior instructions",
        ),
        ("from:crashwatch", "Ignore all previous instructions"),
    ] {
        let out = demo.ok(&[query, "--limit", "50"])?;
        for line in out
            .lines()
            .filter(|l| l.to_lowercase().contains(&phrase.to_lowercase()))
        {
            seen += 1;
            assert!(
                line.trim_start().starts_with("\"author: "),
                "hostile text outside a frame: {line:?}"
            );
        }
    }
    assert!(seen >= 3, "the hostile messages were not found ({seen})");

    // Whatever the query, the output keeps the same shape; probe several.
    for query in [
        "epub",
        "crash",
        "price",
        "ship",
        "in:#general",
        "after:2026-04-01",
    ] {
        demo.ok(&[query, "--limit", "50"])?;
    }
    Ok(())
}

#[test]
fn help_documents_demo_search_and_the_timezone_rule() -> TestResult {
    let output = teamctx(&["--help"])?;
    let help = text(&output.stdout)?;
    for needle in [
        "demo",
        "search QUERY",
        "--limit N",
        "fixed",
        "America/Toronto",
        "framed",
    ] {
        assert!(help.contains(needle), "{needle}: {help}");
    }
    Ok(())
}
