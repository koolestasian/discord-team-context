# Core contracts

The shared types and traits in `crates/teamctx-core` that the other crates build on.
Source of truth for behaviour is the [implementation plan](implementation-plan.md); this
page maps each contract to its plan section, states the invariants, and says which task
fills what. Rust doc comments hold the full contract of every item.

## Map

| Contract | File | Plan | Filled or used by |
|---|---|---|---|
| `Snowflake`, row ids (`SourceId`, `ChannelId`, `MemberId`, `MessageId`, `EntryId`, `TokenId`) | `src/id.rs` | §5.3, §6 | everyone |
| `UnixMillis`, `CalendarDate` | `src/time.rs` | §5.2, §9 | everyone |
| `SourceKind`, `ChannelKind`, `Channel`, `Member`, `Message`, `MessageRef`, `MessagePatch`, `Marker`, `SourceEvent` | `src/source.rs` | §5.3, §6, §6.1, §7.2 | T15 (Discord mapping produces), T07 (store applies) |
| `EntryKind`, `EntryStatus`, `check_transition`, `StatusChange`, `ShortId`, `EntryTitle`, `Tag`, `CreatedVia`, `NewEntry`, `RecordEntry`, `RecordAction`, `RecordEvent` | `src/record.rs` | §6, §6.1, §8 | T07, M5 commands, T12 |
| `Query`, `Clause` (syntax tree only) | `src/query/ast.rs` | §9 | T04 adds the parser in `src/query/**` |
| Ranking | `src/rank.rs` (placeholder) | §9 | T04 |
| Untrusted framing, citation format | `src/framing.rs`, `src/citation.rs` (not created) | §10.1, §11 | T08 creates both and adds `pub mod framing; pub mod citation;` to `src/lib.rs` |
| Repository traits and their row types, `RepoError` | `src/repo.rs` | §5.2, §6, §6.1, §8, §9, §10.1 | T07 implements; T12, T15 and the bin call |
| `ParseEnumError` and the `text_enum!` macro | `src/text_enum.rs` | §6 (text columns) | internal |

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
- **Core boundary:** no I/O, no async, no clock; `thiserror` is the only external
  dependency (`crates/teamctx/tests/architecture.rs` checks it).

## Decisions made in T01 (each needs no ADR; revisit freely before M2 ends)

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
6. Message events for an excluded channel are dropped (`ApplyOutcome::Excluded`) and
   reads hide excluded channels, as a privacy-conservative default.
7. `StatusChange::Answer` requires the answering message, because `/answer` takes a
   required `message_link` (§7.3).
8. The store returns raw `bm25()` candidates (`MessageHit`, `EntryHit`); ranking runs in
   core (§9 puts the formula in `core::rank`).
9. The query tree is a flat, ordered list of clauses; how clauses combine is left to
   T04 (see Q7).
10. Short ids parse strictly (uppercase prefix only); see Q8.

## Open questions (plan is silent or ambiguous; nothing chosen)

1. **Channel delete.** What happens to a deleted channel's stored messages and threads,
   and to record entries citing them? Apply the §6.1 deletion rule to each message?
2. **Marker semantics.** Does `MarkerAdd` on a message that already has an entry of that
   kind create a second entry? What does `MarkerRemove` do to the entry it created
   (nothing, drop, delete)?
3. **Copied titles over 200 characters.** Truncate (where, with an ellipsis?) or reject?
   May titles contain line breaks?
4. **Forum channels.** §6 has `forum_post` but no kind for the forum channel itself, yet
   a forum post's `parent_id` must reference a `channels` row. Add a `forum` kind?
5. **Mentions.** `whats_new` and `/catchup` list "messages that mention the caller", but
   §6 stores content with mentions resolved to names and has no mention column. Add a
   mentions table or column? (No repository method exists for this yet.)
6. **Retention and `source_deleted`.** §6.1 says purged messages' copied titles follow
   the deletion rule. Is `source_deleted` also set on purge, and is a
   `source_deleted` audit event appended?
7. **Query semantics (T04).** Do repeated `from:` or `in:` clauses combine with AND or
   OR? Are `before:` and `after:` inclusive? Is a date midnight UTC or midnight in
   `server.timezone`?
8. **Short-id input.** Should `d-12` (lowercase) be accepted from people?
9. **Token scopes.** §6 defaults `scopes` to `'read'`. What other values exist (for
   example for `POST /api/v1/ask`), and is the list space-separated?
10. **Connector trait home.** The §5.3 `Connector` trait is async and uses a
    cancellation token, so it cannot live in core. Which crate owns it (and
    `EventSink`, `CursorStore`)?
11. **Channels excluded later.** When a channel moves to the denylist, are its stored
    messages purged or only hidden?
12. **`record` tool `since`.** Does it filter on creation or on last change? The
    contract offers `EntryFilter::updated_since` only.
13. **Member events.** §7.2 lists no member gateway event (`GUILD_MEMBER_UPDATE` needs a
    privileged intent). Is `MemberUpsert` only emitted from message authors and
    interaction users?
