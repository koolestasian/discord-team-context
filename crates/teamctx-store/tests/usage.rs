//! The `llm_usage` ledger on real temp-file databases. All data is synthetic.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use std::path::Path;
use std::sync::Arc;

use teamctx_core::{
    id::{MemberId, Snowflake},
    repo::{DirectoryRead, IngestWrite, RepoError},
    source::{Member, SourceEvent, SourceKind},
    time::UnixMillis,
};
use teamctx_store::{
    Store,
    usage::{UsageDb, UsageRecord, UsageWindow},
};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 24 * HOUR;
const NOW: i64 = 10 * DAY;

#[derive(Debug)]
enum Denied {
    Refused,
    Repo(#[expect(dead_code, reason = "kept for assertion failure output")] RepoError),
}

impl From<RepoError> for Denied {
    fn from(error: RepoError) -> Self {
        Self::Repo(error)
    }
}

fn at(ms: i64) -> UnixMillis {
    UnixMillis::from_millis(ms)
}

/// Creates a file database holding one fictional member and returns the member's id.
fn database(path: &Path) -> MemberId {
    let mut store = Store::open(path).unwrap();
    let source = store
        .upsert_source(
            SourceKind::Discord,
            Snowflake::new(1).unwrap(),
            "Fiction Team",
        )
        .unwrap();
    let external = Snowflake::new(100).unwrap();
    store
        .apply_event(
            source,
            &SourceEvent::MemberUpsert(Member {
                id: external,
                display_name: "Piper Fiction".to_owned(),
                is_bot: false,
            }),
            at(1),
        )
        .unwrap();
    store
        .member_by_external(source, external)
        .unwrap()
        .unwrap()
        .id
}

fn row(member: MemberId, when: i64, input: u64, output: u64, ok: bool) -> UsageRecord {
    UsageRecord {
        at: at(when),
        member,
        provider: "fake".to_owned(),
        model: "m".to_owned(),
        input_tokens: input,
        output_tokens: output,
        ok,
    }
}

/// A budget check shaped like the real one: questions per hour, tokens per hour.
fn admit_with(
    now: i64,
    tokens: u64,
    max_questions: u32,
    max_tokens_hour: u64,
) -> impl FnOnce(&mut dyn FnMut(UnixMillis) -> Result<UsageWindow, RepoError>) -> Result<(), Denied>
{
    move |usage_since| {
        let hour = usage_since(at(now - HOUR))?;
        if hour.questions >= max_questions || hour.tokens + tokens > max_tokens_hour {
            return Err(Denied::Refused);
        }
        Ok(())
    }
}

#[test]
fn an_exhausted_budget_refuses_and_holds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    let db = UsageDb::open(&path).unwrap();

    db.reserve(member, at(NOW), 900, admit_with(NOW, 900, 10, 1_000))
        .unwrap();
    let refused = db.reserve(member, at(NOW), 101, admit_with(NOW, 101, 10, 1_000));
    assert!(matches!(refused, Err(Denied::Refused)));
    assert_eq!(
        db.usage_since(member, at(NOW - HOUR)).unwrap(),
        UsageWindow {
            questions: 1,
            tokens: 900
        }
    );
    // The question limit refuses too.
    let refused = db.reserve(member, at(NOW), 1, admit_with(NOW, 1, 1, 1_000));
    assert!(matches!(refused, Err(Denied::Refused)));
}

#[test]
fn concurrent_reservations_on_one_handle_never_exceed_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    let db = Arc::new(UsageDb::open(&path).unwrap());
    let granted: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let db = Arc::clone(&db);
                scope.spawn(move || {
                    db.reserve(member, at(NOW), 10, admit_with(NOW, 10, 3, 1_000))
                        .is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum()
    });
    assert_eq!(granted, 3);
    assert_eq!(db.usage_since(member, at(NOW - HOUR)).unwrap().questions, 3);
}

#[test]
fn concurrent_reservations_on_separate_connections_never_exceed_the_cap() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    // One connection per thread: SQLite's write lock, not a Rust mutex, must serialize
    // the check and the hold.
    let granted: usize = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                scope.spawn(move || {
                    let db = UsageDb::open(&path).unwrap();
                    db.reserve(member, at(NOW), 10, admit_with(NOW, 10, 2, 1_000))
                        .is_ok()
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| usize::from(h.join().unwrap()))
            .sum()
    });
    assert_eq!(granted, 2);
    let db = UsageDb::open(&path).unwrap();
    assert_eq!(db.usage_since(member, at(NOW - HOUR)).unwrap().questions, 2);
}

#[test]
fn settle_swaps_the_reservation_for_the_real_usage() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    let db = UsageDb::open(&path).unwrap();
    let hour_ago = at(NOW - HOUR);

    let held = db
        .reserve(member, at(NOW), 900, admit_with(NOW, 900, 10, 1_000))
        .unwrap();
    db.settle(held, &row(member, NOW, 30, 10, true)).unwrap();
    // One question, now at the real size: the freed tokens are available again.
    assert_eq!(
        db.usage_since(member, hour_ago).unwrap(),
        UsageWindow {
            questions: 1,
            tokens: 40
        }
    );
    db.reserve(member, at(NOW), 900, admit_with(NOW, 900, 10, 1_000))
        .unwrap();

    // Settling again, or an unknown reservation, appends instead of losing the call.
    db.settle(held, &row(member, NOW, 5, 5, false)).unwrap();
    assert_eq!(
        db.usage_since(member, hour_ago).unwrap(),
        UsageWindow {
            questions: 3,
            tokens: 40 + 900 + 10
        }
    );
}

#[test]
fn windows_roll_over_and_include_their_start() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    let db = UsageDb::open(&path).unwrap();
    db.record(&row(member, NOW - HOUR, 10, 0, true)).unwrap();
    db.record(&row(member, NOW - HOUR - 1, 20, 0, true))
        .unwrap();
    db.record(&row(member, NOW - DAY - 1, 40, 0, false))
        .unwrap();
    assert_eq!(
        db.usage_since(member, at(NOW - HOUR)).unwrap(),
        UsageWindow {
            questions: 1,
            tokens: 10
        }
    );
    assert_eq!(
        db.usage_since(member, at(NOW - DAY)).unwrap(),
        UsageWindow {
            questions: 2,
            tokens: 30
        }
    );

    // A reservation that has aged out of the hour no longer blocks the hour budget.
    db.reserve(member, at(NOW), 900, admit_with(NOW, 900, 10, 1_000))
        .unwrap();
    let later = NOW + HOUR + 1;
    db.reserve(member, at(later), 900, admit_with(later, 900, 10, 1_000))
        .unwrap();
}

#[test]
fn reservations_and_rows_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let member = database(&path);
    let held = {
        let db = UsageDb::open(&path).unwrap();
        db.record(&row(member, NOW - 1_000, 100, 0, true)).unwrap();
        db.reserve(member, at(NOW), 800, admit_with(NOW, 800, 10, 1_000))
            .unwrap()
    };

    // The process "crashed" with the call in flight: the reservation still counts in
    // full, so the hour budget is still spent.
    let db = UsageDb::open(&path).unwrap();
    assert_eq!(
        db.usage_since(member, at(NOW - HOUR)).unwrap(),
        UsageWindow {
            questions: 2,
            tokens: 900
        }
    );
    let refused = db.reserve(member, at(NOW), 101, admit_with(NOW, 101, 10, 1_000));
    assert!(matches!(refused, Err(Denied::Refused)));

    // It can still be settled after the restart.
    db.settle(held, &row(member, NOW, 20, 0, true)).unwrap();
    assert_eq!(
        db.usage_since(member, at(NOW - HOUR)).unwrap(),
        UsageWindow {
            questions: 2,
            tokens: 120
        }
    );
}
