//! Spike S4 (plan section 18, ADR 0003): the bundled SQLite supports FTS5 on every
//! target we ship, and an external-content index kept in sync by triggers behaves as the
//! plan's section 6 schema needs.
//!
//! The DDL below is a trimmed copy of the plan's `messages` / `messages_fts` shape. It is
//! deliberately local to this test: M2 writes the real migrations and their tests. Message
//! text is fictional.

use std::error::Error;

use rusqlite::{Connection, params};

type TestResult = Result<(), Box<dyn Error>>;

/// Lowest SQLite version the project accepts.
///
/// 3.50.2 is the release that fixed CVE-2025-6965 (memory corruption reachable from
/// crafted SQL). We run only our own parameterised SQL, but a floor at a patched release
/// keeps a stale vendored copy from slipping in through a dependency change. The features
/// the plan relies on are far older: FTS5 external content (3.9), UPSERT (3.24),
/// `RETURNING` (3.35) and the FTS5 `integrity-check` against external content (3.44).
const MIN_SQLITE_VERSION: i32 = 3_050_002;

const SCHEMA: &str = "
CREATE TABLE messages (
  id          INTEGER PRIMARY KEY,
  channel_id  INTEGER NOT NULL,
  external_id TEXT NOT NULL,
  content     TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  UNIQUE (channel_id, external_id)
);

CREATE VIRTUAL TABLE messages_fts USING fts5(
  content, content='messages', content_rowid='id',
  tokenize = 'porter unicode61 remove_diacritics 2'
);

CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, content)
    VALUES ('delete', old.id, old.content);
  INSERT INTO messages_fts(rowid, content) VALUES (new.id, new.content);
END;
";

/// Fictional messages: (id, content). Message 1 is a short, focused match for
/// "deploy"; message 2 mentions it once inside a long, unrelated text.
const FIXTURE: &[(i64, &str)] = &[
    (
        2,
        "Weekly sync notes: we talked about the roadmap, the new onboarding checklist, \
         the office plants, lunch options, the quarterly survey, and one more thing, \
         we should deploy on Friday if nothing else breaks",
    ),
    (1, "Deploy deploy deploy: the staging deploy is green"),
    (
        3,
        "Does anyone have the invoice template for the cafe order?",
    ),
    (4, "Reminder: the standup moved to 10:30"),
];

fn init(conn: &Connection) -> rusqlite::Result<()> {
    // Same connection settings as ADR 0003 / plan section 6.
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.execute_batch(SCHEMA)?;
    for (id, content) in FIXTURE {
        conn.execute(
            "INSERT INTO messages (id, channel_id, external_id, content, created_at) \
             VALUES (?1, 10, ?2, ?3, ?4)",
            params![id, format!("ext-{id}"), content, 1_700_000_000_000_i64 + id],
        )?;
    }
    Ok(())
}

/// Quote user text as one FTS5 phrase so operators and punctuation are plain text.
fn fts_phrase(text: &str) -> String {
    format!("\"{}\"", text.replace('"', "\"\""))
}

/// Message ids matching `query`, best `bm25()` first (lower is better in FTS5).
fn search(conn: &Connection, query: &str) -> rusqlite::Result<Vec<i64>> {
    let mut stmt = conn.prepare(
        "SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1 \
         ORDER BY bm25(messages_fts), rowid",
    )?;
    stmt.query_map([query], |row| row.get(0))?.collect()
}

fn integrity_check(conn: &Connection) -> rusqlite::Result<()> {
    // With rank = 1 the check also compares the index with the external content table.
    conn.execute(
        "INSERT INTO messages_fts(messages_fts, rank) VALUES ('integrity-check', 1)",
        [],
    )?;
    Ok(())
}

fn exercise(conn: &Connection) -> TestResult {
    init(conn)?;

    // MATCH finds rows, and only the matching ones.
    assert_eq!(search(conn, "invoice")?, vec![3]);
    assert!(search(conn, "nonexistentterm")?.is_empty());

    // bm25 puts the focused message ahead of the one that mentions the term once in a
    // long text. Both match; the order is the point.
    assert_eq!(search(conn, "deploy")?, vec![1, 2]);

    // Tokenizer: porter stemming and diacritics folding, as the plan's DDL requests.
    assert_eq!(search(conn, "deploying")?, vec![1, 2]);
    assert_eq!(search(conn, "café")?, vec![3]);

    // Escaping: hostile text bound as a quoted phrase is data, not FTS syntax.
    let hostile = r#"deploy" OR content:"standup"#;
    assert!(search(conn, &fts_phrase(hostile))?.is_empty());
    assert_eq!(search(conn, &fts_phrase("staging deploy"))?, vec![1]);

    integrity_check(conn)?;

    // Update is reflected: the old term disappears, the new one appears.
    conn.execute(
        "UPDATE messages SET content = ?1 WHERE id = 3",
        ["Does anyone have the contract template?"],
    )?;
    assert!(search(conn, "invoice")?.is_empty());
    assert_eq!(search(conn, "contract")?, vec![3]);
    integrity_check(conn)?;

    // Delete is reflected in the same statement, no separate cleanup.
    let removed = conn.execute("DELETE FROM messages WHERE id = 1", [])?;
    assert_eq!(removed, 1);
    assert_eq!(search(conn, "deploy")?, vec![2]);
    assert!(search(conn, "staging")?.is_empty());
    integrity_check(conn)?;

    // The delete trigger needs the old row text, so verify a second delete too.
    conn.execute("DELETE FROM messages WHERE id = 2", [])?;
    assert!(search(conn, "deploy")?.is_empty());
    integrity_check(conn)?;
    Ok(())
}

#[test]
fn sqlite_is_new_enough_and_has_fts5() -> TestResult {
    let conn = Connection::open_in_memory()?;

    let number = rusqlite::version_number();
    assert!(
        number >= MIN_SQLITE_VERSION,
        "SQLite {} is older than the floor {MIN_SQLITE_VERSION}",
        rusqlite::version()
    );

    for option in ["ENABLE_FTS5", "THREADSAFE=1"] {
        let used: bool = conn.query_row("SELECT sqlite_compileoption_used(?1)", [option], |r| {
            r.get(0)
        })?;
        assert!(used, "compiled SQLite lacks {option}");
    }

    // Belt and braces: the module is actually loadable, not just advertised.
    conn.execute_batch("CREATE VIRTUAL TABLE probe USING fts5(x);")?;

    // The library we linked is the one the Rust crate reports.
    let sql_version: String = conn.query_row("SELECT sqlite_version()", [], |r| r.get(0))?;
    assert_eq!(sql_version, rusqlite::version());
    Ok(())
}

#[test]
fn fts5_external_content_in_memory() -> TestResult {
    let conn = Connection::open_in_memory()?;
    exercise(&conn)
}

#[test]
fn fts5_external_content_in_temp_file_with_wal() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("teamctx-spike.db");
    let conn = Connection::open(&path)?;

    // WAL is the journal mode ADR 0003 chooses; confirm it sticks on this platform.
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    assert_eq!(mode.to_lowercase(), "wal");

    exercise(&conn)?;
    drop(conn);

    // The index survives a reopen: it is stored in the file, not rebuilt in memory.
    let reopened = Connection::open(&path)?;
    reopened.execute(
        "INSERT INTO messages (id, channel_id, external_id, content, created_at) \
         VALUES (9, 10, 'ext-9', 'Reopened database still searchable', 1)",
        [],
    )?;
    assert_eq!(search(&reopened, "searchable")?, vec![9]);
    assert_eq!(search(&reopened, "contract")?, vec![3]);
    integrity_check(&reopened)?;
    Ok(())
}
