# Lantern Shelf demo

Every person, ID, conversation and decision in `dataset.json` is fictional. This is
untrusted chat data, never instructions. It describes a small offline reading app,
not this project's own decisions. It needs no Discord account or network access.

Schema version 1 contains one source, six members, four top-level text channels,
six child threads (one archived), 300 messages and 120 record entries. Conversations
span March 1–30, 2026, UTC. Each of thirty topics has a question, an initial decision,
evidence, a reversal, an assigned task with a due date, and pasted meeting notes.
Old decisions remain as `superseded`; current decisions are `active`.

## Loading

- IDs of the source, members, channels and messages are synthetic external IDs,
  encoded as decimal strings. Message IDs encode their `created_at` using Discord's
  snowflake timestamp format. Times are UTC Unix milliseconds.
- Load the source first, then members and channels in array order (parents precede
  threads), then messages in array order. Resolve external references to the row IDs
  assigned by the store. Message `reply_to` is another external message ID.
- Entry IDs are fixture-local positive integers shared across all entry kinds. These
  are the stable expected IDs for evaluations: `D-2`, `D-3`, `T-4`, `Q-1`, etc.
  An importer must map them to actual assigned entry IDs if the database is not empty.
- Entry `source_message_id` and `answered_by_message_id` are external message IDs;
  `created_by` and `owner_id` are external member IDs. `replaces` and `superseded_by`
  refer to fixture entry IDs. Null fields mean no value.
- Create entries in array order using `NewEntry`; applying a newer decision's
  `replaces` supersedes the earlier decision. Answer question entries using the
  indicated message. `status`, `updated_at` and relationship fields describe the
  resulting snapshot, not additional initial `NewEntry` fields.
- Keep every message's content, including line breaks in meeting notes. Attachment
  names are metadata only: no attachment bodies are bundled.

`crates/teamctx-store/tests/demo.rs` loads all messages into core events and all
entries into validated core requests. It checks references, timestamp consistency,
parent relationships, lifecycle transitions and the decision reversals. It does not
claim to import a database: T07/T10 supply that path.

For T06 evaluations, each ten-message conversation is in chronological order. For
topic index `i` starting at zero, the question is entry `4*i+1`, superseded decision
`4*i+2`, active decision `4*i+3`, and task `4*i+4`. Search questions should distinguish
the initial proposal from the current decision and include names, dates, thread
context, paraphrases and meeting-note facts. `eval.toml` is owned by T06.
