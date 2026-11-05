# 0003. SQLite with FTS5

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D3, §5.2, §6, §9, §14

## Context

The program stores a local copy of channel messages (including edits and deletions),
the team record, access tokens and LLM usage (plan §6). It runs as one process on one
host (ADR 0004), has one logical writer, and must search messages without an LLM (§9).
Hosts should not have to install or operate a database server (US-1).

Deletions must propagate to search within a minute (US-9), so the search index has to
stay consistent with the message table through edits and deletes.

## Decision

Use SQLite through `rusqlite` with the bundled SQLite library, in WAL mode with
`foreign_keys=ON`, `busy_timeout=5000` and `synchronous=NORMAL` (§6). Full-text search
uses FTS5 external-content tables kept in sync by triggers, tokenised with
`porter unicode61 remove_diacritics 2`.

Concurrency follows §5.2: one dedicated thread owns the write connection and takes
typed commands over a channel, so writes are serialised and `SQLITE_BUSY` is avoided.
A small set of read-only connections serves queries through `spawn_blocking`.
Migrations are plain SQL files tracked with `PRAGMA user_version` (§6).

Ranking is `bm25()` combined with recency decay and a boost for record entries (§9).
Query text is parsed in `core` and compiled to parameterised SQL; FTS terms are
escaped.

Validated by spike S4 in M1: bundled SQLite with FTS5 builds and passes tests on
Linux, macOS and Windows in CI. If FTS5 is unavailable on any target, this ADR is
revisited before M2.

## Alternatives considered

- **PostgreSQL (or another server database).** Stronger concurrency and tooling, but
  it adds a service every host must install, secure and back up, which conflicts with
  the single-binary goal.
- **SQLite with `LIKE` queries.** No extra index to maintain, but no stemming or
  ranking, and poor behaviour as history grows.
- **A separate search engine** (an embedded index library or an external server).
  Richer relevance features, but a second store to keep consistent with deletes, and
  more dependencies (ADR 0011).
- **A vector database for semantic search.** Deferred on purpose: embeddings are added
  only if the retrieval eval shows a gap (ADR 0014). `sqlite-vec` is the named
  candidate, so SQLite does not close that door.

## Consequences

### Good

- The whole state is one file plus its WAL; backup, `purge` and the demo dataset are
  simple.
- Deleting a message row removes it from FTS in the same transaction.

### Bad

- A single writer thread limits write throughput. Acceptable for one team's chat.
- Bundling SQLite lengthens the first build and needs a C compiler on the build host.
- Stemming is tuned for English-like text; other languages are not addressed in v0.1.

### Follow-ups

- M1 S4 result recorded here; M2 writes the final DDL and the migration tests (§14).
