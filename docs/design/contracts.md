# Core contracts

The shared types and traits in `crates/teamctx-core` that the other crates build on.
Source of truth for behaviour is the [implementation plan](implementation-plan.md); this
page maps each contract to its plan section, states the invariants, and says which crate
or milestone builds on it. Rust doc comments hold the full contract of every item.

## Map

| Contract | File | Plan | Filled or used by |
|---|---|---|---|
| `Snowflake`, row ids (`SourceId`, `ChannelId`, `MemberId`, `MessageId`, `EntryId`, `TokenId`) | `src/id.rs` | §5.3, §6 | everyone |
| `UnixMillis`, `CalendarDate`, `Weekday` | `src/time.rs` | §5.2, §9 | everyone |
| `parse_due`, `parse_since`, `Zone`, `FixedOffset`, `DueDate`, `WhenError` | `src/when.rs` | §8 | M5 commands; `q whats-new` |
| `SourceKind`, `ChannelKind`, `Channel`, `Member`, `Message`, `MessageRef`, `MessagePatch`, `Marker`, `SourceEvent` | `src/source.rs` | §5.3, §6, §6.1, §7.2 | Discord mapping produces (M3); store applies |
| `EntryKind`, `EntryStatus`, `check_transition`, `StatusChange`, `ShortId`, `EntryTitle`, `Tag`, `CreatedVia`, `NewEntry`, `RecordEntry`, `RecordAction`, `RecordEvent` | `src/record.rs` | §6, §6.1, §8 | store; M4 MCP tools; M5 commands |
| `Query`, `Clause` (syntax tree only) | `src/query/ast.rs` | §9 | parser in `src/query/parser.rs`; store compiles it to SQL |
| Ranking | `src/rank.rs` | §9 | store search; tuned by the M2 eval |
| Untrusted framing, citation format | `src/framing.rs`, `src/citation.rs` | §10.1, §11 | M4 MCP tools, M6 `/ask` |
| `RecordEdit`, `EntryOutcome`, `RequestKey`, `EditField`, `check_edit` | `src/repo.rs`, `src/record.rs` | §6.1, §8 | M5 Discord commands call; store implements |
| Repository traits and their row types, `RepoError` | `src/repo.rs` | §5.2, §6, §6.1, §8, §9, §10.1 | `teamctx-store` implements; server, Discord connector and the binary call |
| `ParseEnumError` and the `text_enum!` macro | `src/text_enum.rs` | §6 (text columns) | internal |

Live connector contracts (in `crates/teamctx-discord`, not core):

| Contract | File | Plan | Notes |
|---|---|---|---|
| `Gateway`, `GatewayEvent`, `INTENTS`, `fatal_close_reason` | `src/gateway.rs` | §7.1, §7.2 | raw JSON frames; `TwilightGateway` is the production shard |
| `TwilightRest`, `Read` | `src/http.rs` | §7.2, §7.3 | the only `twilight_http` user; `GET` only |
| `live::ingest` | `src/live.rs` | §7.2 step 4 | configured guild only; unknown channel means "run a sync" |
| `Connector`, `ConnectorStatus`, `Timing` | `src/connector.rs` | §5.2, §7.2 | sync on `GUILD_CREATE` and `RESUMED`; drains on shutdown |
| `HostStore` | `src/store.rs` | §5.2 | writer client for writes, own read-only connection for reads |
| `doctor::diagnose`, `doctor::check` | `src/doctor.rs` | §7.1 | pure on payloads; five `GET`s |
| `status::HostStatus`, `StatusProvider` | `crates/teamctx-server/src/status.rs` | §10.2 | ids, times, counts, fixed text |

The §5.3 `Connector` trait with `EventSink` and `CursorStore` is not introduced yet:
with one connector, `Connector` is a concrete type over a `Gateway`, a `DiscordRest` and
a store factory. The trait follows when a second source (Slack, v0.2) needs it.

## Invariants (pinned by unit tests in each file)

- **Snowflake:** never zero; text form is canonical decimal (no sign, no leading zero,
  no whitespace), so `parse` then `to_string` returns the input. This matters because
  the store keeps external ids as `TEXT` and one id must have one text form.
  `timestamp()` uses the Discord epoch (2015-01-01).
- **Row ids:** positive `i64`, created only by the store.
- **Text enums:** the text of every variant equals the plan §6 `CHECK` list; parsing is
  exact (no trimming, no case folding); ordering is declaration order.
- **CalendarDate:** a real date in years 1 to 9999; text is exactly `YYYY-MM-DD`.
- **Channel:** `parent` is set exactly for `thread` and `forum_post`, and never the
  channel itself (`Channel::validate`).
- **MessagePatch:** per field, `None` keeps and `Some` replaces; author, reply target and
  creation time are not patchable; applying twice equals applying once; a patch for
  another `(channel, message)` is rejected and changes nothing.
- **Lifecycle (§8):** the only steps are decision `active → superseded`, task
  `open → done | dropped`, question `open → answered | dropped`. Same-status requests
  return `AlreadyInStatus` (callers may treat it as a no-op). No reopening. A decision
  cannot supersede itself.
- **ShortId (§8):** `[DTQ]-[1-9][0-9]*`, number fits `i64`, round-trips exactly. The
  number is the shared `record_entries` row id; a short id with the wrong prefix for a
  row is "not found".
- **Titles and tags (§6, §6.1):** titles are non-blank and at most 200 characters,
  kept as given; the deletion placeholder is `(source message deleted)`. Tags are
  single words, lowercased.
- **New entries:** only tasks have `due_at`; only decisions have `replaces`.
- **Audit log:** `RecordAction` encodes as `(action, detail)`: `("created", None)`,
  `("status", "<from> <to>")`, `("source_deleted", None)`, and decodes back exactly.
- **Repositories:** synchronous; writes take `&mut self` and run as one transaction;
  reads take `&self`; times are passed in; reads never return messages from excluded
  channels; `RepoError::Backend` displays a generic message so SQL and row data do not
  reach users.
- **Channel message pages (`MessageRead::channel_messages_since(channel, since, limit,
  cursor)`):** the messages of one channel row created at or after `since`, oldest first
  (`created_at`, then numeric external id; the same order `neighbours` uses), at most
  `limit`. `cursor` is a `MessageCursor` (time and external id of the last message of the
  previous page) and the next page starts strictly after that position, so paging is a
  keyset, not an offset: deletes between pages cannot skip or repeat a message. A page
  shorter than `limit` is the last. A thread or forum post is a channel row of its own, so
  a parent's pages never contain its threads' messages; excluded (directly or through the
  parent) and unknown channels return an empty page, as does `limit` 0. The query uses the
  `messages_channel_time` index (a unit test checks the query plan). The REST sync's
  deletion recheck pages through this instead of anchoring on a stored message, so the
  stored side of its window has no size cap.
- **Dates typed by people (§8, `core::when`):** `parse_due(text, now, &zone)` gives a
  `DueDate { date, at }`: the local day to echo back and the instant that day starts in the
  host's timezone (`at` is what `due_at` stores). `parse_since` gives an instant. Weeks run
  Monday to Sunday. Ambiguous input (`11/20`, a bare weekday on that weekday) and past due
  dates are errors; errors never echo the input. The caller supplies `now` and a `Zone`;
  core has no clock and no timezone database (`FixedOffset` covers UTC and `+02:00`).
- **Record edits (§6.1, §8, `RecordEdit`):** a separate trait from `RecordWrite`. Owner
  and due-date changes need a live entry (`active` or `open`); only tasks have a due date.
  Asking for the state an entry already has changes nothing and appends no event
  (`EntryOutcome::changed == false`). `capture_message` creates at most one entry per
  message and kind. `create_entry_once(key, ..)` creates once per `RequestKey` (use the
  interaction id). `change_status_once` treats a repeat with the same data as success.
  Audit encodings added: `("owner", "<from> <to>")` with member row ids and `("due", "<from>
  <to>")` with Unix milliseconds, `-` for none. Short ids stay `kind prefix + the shared
  row id` (`D-1`, `T-2`, `Q-3`, `D-4`), allocated by the database in the creating
  transaction.
- **Audit log:** `record_events` is append-only; the database rejects updates and deletes
  of its rows (migration 0002).
- **Core boundary:** no I/O, no async, no clock; `thiserror` is the only external
  dependency (`crates/teamctx/tests/architecture.rs` checks it).

## Decisions made while defining the contracts (each needs no ADR; revisit freely before M2 ends)

1. External ids are `Snowflake` (a non-zero `u64`), as the task brief asked. Slack
   (v0.2) uses string ids, so a second connector will need an external-id type change
   in core; the `TEXT` columns already allow it.
2. `SourceEvent` carries external refs only, and the store receives the `SourceId` as a
   parameter of `apply_event`. The §5.3 sketch's `ChannelRef` became a plain
   `channel: Snowflake`, and its `MessageId` in `MessageBulkDelete` became `Snowflake`,
   because `MessageId` names the store row id here.
3. `SourceEvent` adds `MessageUpdate(MessagePatch)` for the partial `MESSAGE_UPDATE` of
   §7.2 and `MemberUpsert(Member)` (requested in the brief) to the §5.3 sketch.
4. `Channel::included` is computed by the connector from the allowlist and denylist
   (§7.2 step 1); the store only records it.
5. `Marker` carries the `EntryKind`: the connector maps the configured emoji and drops
   every other reaction, so unrelated reactions never reach the store.
6. New/updated messages for excluded channels are dropped (`ApplyOutcome::Excluded`).
   Deletions still clean up, and reads hide excluded channels.
7. `StatusChange::Answer` requires the answering message, because `/answer` takes a
   required `message_link` (§7.3).
8. The store returns raw `bm25()` candidates (`MessageHit`, `EntryHit`); ranking runs in
   core (§9 puts the formula in `core::rank`).
9. The query tree is a flat, ordered list of AND clauses (including repeated filters).
10. Canonical short ids parse strictly; `ShortId::parse_user` accepts lowercase input.

## Maintainer clarifications (2026-11-05)

All thirteen draft questions were answered in the build continuation:

- Channel deletion purges messages and child threads using the message deletion rule.
- Reaction capture is unique per message/kind; removing the reaction keeps the entry.
- Copied titles normalize whitespace to one line and truncate at 200 Unicode scalars.
- Forum parents use kind `forum`; mentioned user IDs are stored separately from text.
- Retention and newly excluded channels purge history using the same deletion rule,
  setting `source_deleted` and appending its audit event. Deletion cleanup must run
  even when the channel is already excluded.
- Query clauses combine with AND. Dates mean midnight in `server.timezone`, with
  inclusive `after:` and exclusive `before:`; timezone conversion stays outside core.
- Human short-id input accepts lowercase prefixes; canonical display/storage is uppercase.
- v0.1 tokens have only the `read` scope, including access to `/ask`.
- `Connector`, `EventSink` and `CursorStore` belong in `teamctx-discord`.
- `record`'s `since` means last update.
- Member upserts come from message authors and interaction users, without a member intent.

Message contracts carry mention IDs separately; mapping populates them and storage
persists them. Contract types alone do not implement ingestion or retrieval.
