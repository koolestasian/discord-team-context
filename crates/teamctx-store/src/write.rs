use rusqlite::{Connection, params};
use teamctx_core::{id::*, record::*, repo::*, source::*, time::UnixMillis};

use crate::{
    Store, backend, invalid,
    read::{
        CHANNEL_COLUMNS, ENTRY_COLUMNS, MEMBER_COLUMNS, MESSAGE_COLUMNS, TOKEN_COLUMNS,
        channel_row, entry_row, member_row, message_row, query_all, query_optional, token_row,
    },
};

fn missing(what: &'static str) -> RepoError {
    RepoError::NotFound { what }
}

/// Whether the statement `sql`, which must select one boolean, returns true.
fn is_true(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> RepoResult<bool> {
    conn.query_row(sql, params, |r| r.get(0)).map_err(backend)
}

fn exists(conn: &Connection, table: &str, id: i64, what: &'static str) -> RepoResult<()> {
    // `table` is always an internal constant, never user input.
    let sql = format!(
        "SELECT EXISTS (
             SELECT 1
             FROM {table}
             WHERE id = ?
         )"
    );
    if is_true(conn, &sql, [id])? {
        Ok(())
    } else {
        Err(missing(what))
    }
}

/// Whether the message exists in an included channel.
fn visible_message_exists(conn: &Connection, id: MessageId) -> RepoResult<bool> {
    is_true(
        conn,
        "SELECT EXISTS (
             SELECT 1
             FROM messages m
             JOIN channels c ON c.id = m.channel_id
             WHERE m.id = ?
               AND c.is_included = 1
         )",
        [id.get()],
    )
}

fn find_channel(
    conn: &Connection,
    source: SourceId,
    id: Snowflake,
) -> RepoResult<Option<ChannelInfo>> {
    let sql = format!(
        "SELECT {CHANNEL_COLUMNS}
         FROM channels c
         WHERE c.source_id = ?
           AND c.external_id = ?"
    );
    query_optional(
        conn,
        &sql,
        params![source.get(), id.to_string()],
        channel_row,
    )
}

fn find_member(
    conn: &Connection,
    source: SourceId,
    id: Snowflake,
) -> RepoResult<Option<MemberInfo>> {
    let sql = format!(
        "SELECT {MEMBER_COLUMNS}
         FROM members mb
         WHERE mb.source_id = ?
           AND mb.external_id = ?"
    );
    query_optional(
        conn,
        &sql,
        params![source.get(), id.to_string()],
        member_row,
    )
}

fn raw_message(
    conn: &Connection,
    channel: ChannelId,
    id: Snowflake,
) -> RepoResult<Option<StoredMessage>> {
    let sql = format!(
        "SELECT {MESSAGE_COLUMNS}
         FROM messages m
         WHERE m.channel_id = ?
           AND m.external_id = ?"
    );
    query_optional(conn, &sql, params![channel.get(), id.to_string()], |r| {
        message_row(conn, r)
    })
}

fn raw_entry(conn: &Connection, id: EntryId) -> RepoResult<RecordEntry> {
    let sql = format!(
        "SELECT {ENTRY_COLUMNS}
         FROM record_entries e
         WHERE e.id = ?"
    );
    query_optional(conn, &sql, [id.get()], entry_row)?.ok_or_else(|| missing("entry"))
}

fn upsert_member(
    conn: &Connection,
    source: SourceId,
    member: &Member,
) -> RepoResult<(MemberId, bool)> {
    let old = find_member(conn, source, member.id)?;
    let changed = old
        .as_ref()
        .is_none_or(|m| m.display_name != member.display_name || m.is_bot != member.is_bot);
    conn.execute(
        "INSERT INTO members (source_id, external_id, display_name, is_bot)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (source_id, external_id) DO UPDATE
         SET display_name = excluded.display_name,
             is_bot = excluded.is_bot
         WHERE display_name IS NOT excluded.display_name
            OR is_bot IS NOT excluded.is_bot",
        params![
            source.get(),
            member.id.to_string(),
            member.display_name,
            member.is_bot
        ],
    )
    .map_err(backend)?;
    let member = find_member(conn, source, member.id)?.ok_or_else(|| missing("member"))?;
    Ok((member.id, changed))
}

fn audit(
    conn: &Connection,
    id: EntryId,
    actor: Option<MemberId>,
    action: RecordAction,
    at: UnixMillis,
) -> RepoResult<()> {
    conn.execute(
        "INSERT INTO record_events (entry_id, actor_member_id, action, detail, at)
         VALUES (?, ?, ?, ?, ?)",
        params![
            id.get(),
            actor.map(MemberId::get),
            action.action(),
            action.detail(),
            at.as_millis()
        ],
    )
    .map_err(backend)?;
    Ok(())
}

/// Deletes a message and applies the deletion rule (plan section 6.1) to the record.
fn delete_message(conn: &Connection, id: MessageId, now: UnixMillis) -> RepoResult<bool> {
    let entries = query_all(
        conn,
        "SELECT id
         FROM record_entries
         WHERE source_message_id = ?",
        [id.get()],
        |r| r.get::<_, i64>(0),
    )?;
    for raw in entries {
        let entry = EntryId::new(raw).ok_or_else(|| invalid("invalid entry id"))?;
        // A copied title came from the deleted message, so it goes with it.
        conn.execute(
            "UPDATE record_entries
             SET source_deleted = 1,
                 source_message_id = NULL,
                 title = CASE WHEN title_is_copied = 1 THEN ? ELSE title END,
                 updated_at = max(updated_at, ?)
             WHERE id = ?",
            params![SOURCE_DELETED_TITLE, now.as_millis(), raw],
        )
        .map_err(backend)?;
        audit(conn, entry, None, RecordAction::SourceDeleted, now)?;
    }
    // Losing an answer is a record change even when its original source survives.
    // Advance the catch-up watermark before the foreign key removes the reference.
    conn.execute(
        "UPDATE record_entries
         SET answered_by_message_id = NULL,
             updated_at = max(updated_at, ?)
         WHERE answered_by_message_id = ?",
        params![now.as_millis(), id.get()],
    )
    .map_err(backend)?;
    // Foreign keys clear mention rows; FTS triggers
    // remove both the message text and any copied record title in this transaction.
    let deleted = conn
        .execute(
            "DELETE FROM messages
             WHERE id = ?",
            [id.get()],
        )
        .map_err(backend)?;
    Ok(deleted > 0)
}

/// `root` and every channel below it.
fn subtree(conn: &Connection, root: ChannelId) -> RepoResult<Vec<ChannelId>> {
    query_all(
        conn,
        "WITH RECURSIVE descendants (id) AS (
             SELECT ?
             UNION
             SELECT c.id
             FROM channels c
             JOIN descendants d ON c.parent_id = d.id
         )
         SELECT id
         FROM descendants",
        [root.get()],
        |r| ChannelId::new(r.get(0)?).ok_or(rusqlite::Error::InvalidQuery),
    )
}

fn purge_channels(conn: &Connection, root: ChannelId, now: UnixMillis) -> RepoResult<()> {
    for channel in subtree(conn, root)? {
        purge_channel(conn, channel, now)?;
    }
    Ok(())
}

fn purge_channel(conn: &Connection, channel: ChannelId, now: UnixMillis) -> RepoResult<()> {
    let ids = query_all(
        conn,
        "SELECT id
         FROM messages
         WHERE channel_id = ?",
        [channel.get()],
        |r| r.get::<_, i64>(0),
    )?;
    for raw in ids {
        delete_message(
            conn,
            MessageId::new(raw).ok_or_else(|| invalid("invalid message id"))?,
            now,
        )?;
    }
    Ok(())
}

/// Recomputes effective visibility below `root` from what each channel requested, then
/// purges the messages of every channel that is now hidden.
fn refresh_visibility(conn: &Connection, root: ChannelId, now: UnixMillis) -> RepoResult<()> {
    conn.execute(
        "WITH RECURSIVE visibility (id, included) AS (
             SELECT c.id, c.requested_included AND coalesce(p.is_included, 1)
             FROM channels c
             LEFT JOIN channels p ON p.id = c.parent_id
             WHERE c.id = ?
             UNION ALL
             SELECT c.id, c.requested_included AND v.included
             FROM channels c
             JOIN visibility v ON c.parent_id = v.id
         )
         UPDATE channels
         SET is_included = (SELECT included FROM visibility WHERE visibility.id = channels.id)
         WHERE id IN (SELECT id FROM visibility)",
        [root.get()],
    )
    .map_err(backend)?;
    for channel in subtree(conn, root)? {
        let included = is_true(
            conn,
            "SELECT is_included
             FROM channels
             WHERE id = ?",
            [channel.get()],
        )?;
        if !included {
            purge_channel(conn, channel, now)?;
        }
    }
    Ok(())
}

fn store_message(
    conn: &Connection,
    source: SourceId,
    message: &Message,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, message.id.channel)? else {
        return Ok(ApplyOutcome::UnknownChannel(message.id.channel));
    };
    if !channel.included {
        return Ok(ApplyOutcome::Excluded);
    }
    let (author, member_changed) = if let Some(member) = &message.author {
        let (id, changed) = upsert_member(conn, source, member)?;
        (Some(id), changed)
    } else {
        (None, false)
    };
    let mut mentions = message.mentions.clone();
    mentions.sort_unstable();
    mentions.dedup();
    let old = raw_message(conn, channel.id, message.id.message)?;
    if old.as_ref().is_some_and(|old| {
        old.author == author
            && old.content == message.content
            && old.reply_to == message.reply_to
            && old.attachment_names == message.attachment_names
            && old.created_at == message.created_at
            && old.edited_at == message.edited_at
            && old.mentions == mentions
    }) {
        return Ok(if member_changed {
            ApplyOutcome::Applied
        } else {
            ApplyOutcome::Unchanged
        });
    }
    let names = serde_json::to_string(&message.attachment_names).map_err(backend)?;
    conn.execute(
        "INSERT INTO messages (channel_id, external_id, author_id, content, reply_to_ext,
                               has_attachments, attachment_names, created_at, edited_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (channel_id, external_id) DO UPDATE
         SET author_id = excluded.author_id,
             content = excluded.content,
             reply_to_ext = excluded.reply_to_ext,
             has_attachments = excluded.has_attachments,
             attachment_names = excluded.attachment_names,
             created_at = excluded.created_at,
             edited_at = excluded.edited_at",
        params![
            channel.id.get(),
            message.id.message.to_string(),
            author.map(MemberId::get),
            message.content,
            message.reply_to.map(|id| id.to_string()),
            !message.attachment_names.is_empty(),
            names,
            message.created_at.as_millis(),
            message.edited_at.map(UnixMillis::as_millis)
        ],
    )
    .map_err(backend)?;
    let saved =
        raw_message(conn, channel.id, message.id.message)?.ok_or_else(|| missing("message"))?;
    conn.execute(
        "DELETE FROM message_mentions
         WHERE message_id = ?",
        [saved.id.get()],
    )
    .map_err(backend)?;
    for mentioned in mentions {
        conn.execute(
            "INSERT INTO message_mentions (message_id, external_user_id)
             VALUES (?, ?)",
            params![saved.id.get(), mentioned.to_string()],
        )
        .map_err(backend)?;
    }
    Ok(ApplyOutcome::Applied)
}

fn create_entry(conn: &Connection, entry: &NewEntry) -> RepoResult<RecordEntry> {
    entry.validate()?;
    exists(conn, "members", entry.created_by.get(), "creator")?;
    if let Some(owner) = entry.owner {
        exists(conn, "members", owner.get(), "owner")?;
    }
    if let Some(message) = entry.source_message
        && !visible_message_exists(conn, message)?
    {
        return Err(missing("message"));
    }
    let replaced = entry.replaces.map(|id| raw_entry(conn, id)).transpose()?;
    if let Some(old) = &replaced {
        if old.kind != EntryKind::Decision {
            return Err(missing("decision"));
        }
        check_transition(old.kind, old.status, EntryStatus::Superseded)
            .map_err(RecordRuleError::from)?;
    }
    let tags = entry
        .tags
        .iter()
        .map(Tag::as_str)
        .collect::<Vec<_>>()
        .join(" ");
    conn.execute(
        "INSERT INTO record_entries (kind, status, title, title_is_copied, owner_member_id,
                                     due_at, source_message_id, created_by_member_id,
                                     created_via, tags, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            entry.kind.as_str(),
            entry.kind.initial_status().as_str(),
            entry.title.as_str(),
            entry.title_is_copied,
            entry.owner.map(MemberId::get),
            entry.due_at.map(UnixMillis::as_millis),
            entry.source_message.map(MessageId::get),
            entry.created_by.get(),
            entry.created_via.as_str(),
            tags,
            entry.created_at.as_millis(),
            entry.created_at.as_millis()
        ],
    )
    .map_err(backend)?;
    let id = EntryId::new(conn.last_insert_rowid())
        .ok_or_else(|| invalid("invalid generated entry id"))?;
    audit(
        conn,
        id,
        Some(entry.created_by),
        RecordAction::Created,
        entry.created_at,
    )?;
    if let Some(old) = replaced {
        change_status(
            conn,
            old.short_id(),
            StatusChange::Supersede { by: id },
            entry.created_by,
            entry.created_at,
        )?;
    }
    raw_entry(conn, id)
}

fn change_status(
    conn: &Connection,
    id: ShortId,
    change: StatusChange,
    actor: MemberId,
    at: UnixMillis,
) -> RepoResult<RecordEntry> {
    let entry = raw_entry(conn, id.id)?;
    if entry.kind != id.kind {
        return Err(missing("entry"));
    }
    change.check(entry.id, entry.kind, entry.status)?;
    exists(conn, "members", actor.get(), "actor")?;
    let mut by = None;
    let mut answer = None;
    match change {
        StatusChange::Supersede { by: target } => {
            let target = raw_entry(conn, target)?;
            if target.kind != EntryKind::Decision {
                return Err(missing("decision"));
            }
            by = Some(target.id.get());
        }
        StatusChange::Answer { message } => {
            if !visible_message_exists(conn, message)? {
                return Err(missing("message"));
            }
            answer = Some(message.get());
        }
        _ => {}
    }
    conn.execute(
        "UPDATE record_entries
         SET status = ?,
             superseded_by = ?,
             answered_by_message_id = ?,
             updated_at = max(updated_at, ?)
         WHERE id = ?",
        params![
            change.target().as_str(),
            by,
            answer,
            at.as_millis(),
            id.id.get()
        ],
    )
    .map_err(backend)?;
    audit(
        conn,
        id.id,
        Some(actor),
        RecordAction::StatusChanged {
            from: entry.status,
            to: change.target(),
        },
        at,
    )?;
    raw_entry(conn, id.id)
}

fn upsert_channel(
    conn: &Connection,
    source: SourceId,
    channel: &Channel,
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    channel.validate().map_err(backend)?;
    let parent = match channel.parent {
        Some(parent_ref) => match find_channel(conn, source, parent_ref)? {
            Some(parent) => Some(parent),
            None => return Ok(ApplyOutcome::UnknownChannel(parent_ref)),
        },
        None => None,
    };
    let old = find_channel(conn, source, channel.id)?;
    if let (Some(old), Some(parent)) = (&old, &parent)
        && subtree(conn, old.id)?.contains(&parent.id)
    {
        return Err(invalid("channel hierarchy would contain a cycle"));
    }
    let parent_id = parent.as_ref().map(|parent| parent.id);
    let included = channel.included && parent.as_ref().is_none_or(|parent| parent.included);
    let old_requested = match &old {
        Some(old) => Some(is_true(
            conn,
            "SELECT requested_included
             FROM channels
             WHERE id = ?",
            [old.id.get()],
        )?),
        None => None,
    };
    let unchanged = old.as_ref().is_some_and(|old| {
        old.parent == parent_id
            && old.kind == channel.kind
            && old.name == channel.name
            && old.topic == channel.topic
            && old.included == included
            && old.archived == channel.archived
            && old_requested == Some(channel.included)
    });
    if unchanged {
        return Ok(ApplyOutcome::Unchanged);
    }
    conn.execute(
        "INSERT INTO channels (source_id, external_id, parent_id, kind, name, topic,
                               is_included, archived, requested_included)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (source_id, external_id) DO UPDATE
         SET parent_id = excluded.parent_id,
             kind = excluded.kind,
             name = excluded.name,
             topic = excluded.topic,
             is_included = excluded.is_included,
             archived = excluded.archived,
             requested_included = excluded.requested_included",
        params![
            source.get(),
            channel.id.to_string(),
            parent_id.map(ChannelId::get),
            channel.kind.as_str(),
            channel.name,
            channel.topic,
            included,
            channel.archived,
            channel.included
        ],
    )
    .map_err(backend)?;
    let id = find_channel(conn, source, channel.id)?
        .ok_or_else(|| missing("channel"))?
        .id;
    refresh_visibility(conn, id, now)?;
    Ok(ApplyOutcome::Applied)
}

fn delete_channel(
    conn: &Connection,
    source: SourceId,
    channel: Snowflake,
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, channel)? else {
        return Ok(ApplyOutcome::Unchanged);
    };
    purge_channels(conn, channel.id, now)?;
    conn.execute(
        "DELETE FROM channels
         WHERE id = ?",
        [channel.id.get()],
    )
    .map_err(backend)?;
    Ok(ApplyOutcome::Applied)
}

/// Applies a partial edit to the stored message, then stores the result like an upsert.
fn update_message(
    conn: &Connection,
    source: SourceId,
    patch: &MessagePatch,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, patch.id.channel)? else {
        return Ok(ApplyOutcome::UnknownChannel(patch.id.channel));
    };
    if !channel.included {
        return Ok(ApplyOutcome::Excluded);
    }
    let Some(old) = raw_message(conn, channel.id, patch.id.message)? else {
        return Ok(ApplyOutcome::NeedsFetch(patch.id));
    };
    let author = match old.author {
        Some(id) => {
            let sql = format!(
                "SELECT {MEMBER_COLUMNS}
                 FROM members mb
                 WHERE mb.id = ?"
            );
            let member = conn
                .query_row(&sql, [id.get()], member_row)
                .map_err(backend)?;
            Some(Member {
                id: member.external_id,
                display_name: member.display_name,
                is_bot: member.is_bot,
            })
        }
        None => None,
    };
    let mut message = Message {
        id: patch.id,
        author,
        content: old.content,
        reply_to: old.reply_to,
        attachment_names: old.attachment_names,
        created_at: old.created_at,
        edited_at: old.edited_at,
        mentions: old.mentions,
    };
    patch.apply(&mut message).map_err(backend)?;
    store_message(conn, source, &message)
}

fn delete_one_message(
    conn: &Connection,
    source: SourceId,
    target: MessageRef,
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, target.channel)? else {
        return Ok(ApplyOutcome::Unchanged);
    };
    let Some(message) = raw_message(conn, channel.id, target.message)? else {
        return Ok(ApplyOutcome::Unchanged);
    };
    delete_message(conn, message.id, now)?;
    Ok(ApplyOutcome::Applied)
}

fn bulk_delete_messages(
    conn: &Connection,
    source: SourceId,
    channel: Snowflake,
    ids: &[Snowflake],
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, channel)? else {
        return Ok(ApplyOutcome::Unchanged);
    };
    let mut changed = false;
    for id in ids {
        if let Some(message) = raw_message(conn, channel.id, *id)? {
            changed |= delete_message(conn, message.id, now)?;
        }
    }
    Ok(if changed {
        ApplyOutcome::Applied
    } else {
        ApplyOutcome::Unchanged
    })
}

/// A decision, task or question marker (a reaction) on a stored message creates one entry.
fn add_marker(
    conn: &Connection,
    source: SourceId,
    marker: &Marker,
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    let Some(channel) = find_channel(conn, source, marker.message.channel)? else {
        return Ok(ApplyOutcome::UnknownChannel(marker.message.channel));
    };
    if !channel.included {
        return Ok(ApplyOutcome::Excluded);
    }
    let Some(message) = raw_message(conn, channel.id, marker.message.message)? else {
        return Ok(ApplyOutcome::NeedsFetch(marker.message));
    };
    let already_captured = is_true(
        conn,
        "SELECT EXISTS (
             SELECT 1
             FROM record_entries
             WHERE source_message_id = ?
               AND kind = ?
         )",
        params![message.id.get(), marker.kind.as_str()],
    )?;
    if already_captured {
        return Ok(ApplyOutcome::Unchanged);
    }
    let actor = find_member(conn, source, marker.actor)?.ok_or_else(|| missing("marker actor"))?;
    let title = EntryTitle::copy_from_message(&message.content)?;
    let entry = NewEntry {
        kind: marker.kind,
        title,
        title_is_copied: true,
        owner: None,
        due_at: None,
        source_message: Some(message.id),
        created_by: actor.id,
        created_via: CreatedVia::Reaction,
        tags: vec![],
        replaces: None,
        created_at: now,
    };
    create_entry(conn, &entry)?;
    Ok(ApplyOutcome::Applied)
}

fn apply_event(
    conn: &Connection,
    source: SourceId,
    event: &SourceEvent,
    now: UnixMillis,
) -> RepoResult<ApplyOutcome> {
    exists(conn, "sources", source.get(), "source")?;
    match event {
        SourceEvent::MemberUpsert(member) => {
            let (_, changed) = upsert_member(conn, source, member)?;
            Ok(if changed {
                ApplyOutcome::Applied
            } else {
                ApplyOutcome::Unchanged
            })
        }
        SourceEvent::ChannelUpsert(channel) => upsert_channel(conn, source, channel, now),
        SourceEvent::ChannelDelete { channel } => delete_channel(conn, source, *channel, now),
        SourceEvent::MessageUpsert(message) => store_message(conn, source, message),
        SourceEvent::MessageUpdate(patch) => update_message(conn, source, patch),
        SourceEvent::MessageDelete { message } => delete_one_message(conn, source, *message, now),
        SourceEvent::MessageBulkDelete { channel, ids } => {
            bulk_delete_messages(conn, source, *channel, ids, now)
        }
        SourceEvent::MarkerAdd(marker) => add_marker(conn, source, marker, now),
        SourceEvent::MarkerRemove(_) => Ok(ApplyOutcome::Unchanged),
    }
}

impl IngestWrite for Store {
    fn upsert_source(
        &mut self,
        kind: SourceKind,
        external_id: Snowflake,
        name: &str,
    ) -> RepoResult<SourceId> {
        self.transaction(|conn| {
            conn.execute(
                "INSERT INTO sources (kind, external_id, name)
                 VALUES (?, ?, ?)
                 ON CONFLICT (kind, external_id) DO UPDATE
                 SET name = excluded.name
                 WHERE name IS NOT excluded.name",
                params![kind.as_str(), external_id.to_string(), name],
            )
            .map_err(backend)?;
            let raw: i64 = conn
                .query_row(
                    "SELECT id
                     FROM sources
                     WHERE kind = ?
                       AND external_id = ?",
                    params![kind.as_str(), external_id.to_string()],
                    |r| r.get(0),
                )
                .map_err(backend)?;
            SourceId::new(raw).ok_or_else(|| invalid("invalid source id"))
        })
    }
    fn apply_event(
        &mut self,
        source: SourceId,
        event: &SourceEvent,
        now: UnixMillis,
    ) -> RepoResult<ApplyOutcome> {
        self.transaction(|conn| apply_event(conn, source, event, now))
    }
    fn set_cursor(
        &mut self,
        source: SourceId,
        channel: Snowflake,
        cursor: &SyncCursor,
    ) -> RepoResult<()> {
        self.transaction(|conn| {
            let channel = find_channel(conn, source, channel)?.ok_or_else(|| missing("channel"))?;
            conn.execute(
                "INSERT INTO sync_cursors (channel_id, newest_seen_ext, oldest_backfilled_ext,
                                           backfill_done, updated_at)
                 VALUES (?, ?, ?, ?, ?)
                 ON CONFLICT (channel_id) DO UPDATE
                 SET newest_seen_ext = excluded.newest_seen_ext,
                     oldest_backfilled_ext = excluded.oldest_backfilled_ext,
                     backfill_done = excluded.backfill_done,
                     updated_at = excluded.updated_at",
                params![
                    channel.id.get(),
                    cursor.newest_seen.map(|id| id.to_string()),
                    cursor.oldest_backfilled.map(|id| id.to_string()),
                    cursor.backfill_done,
                    cursor.updated_at.as_millis()
                ],
            )
            .map_err(backend)?;
            Ok(())
        })
    }
    fn purge_messages_before(&mut self, cutoff: UnixMillis, now: UnixMillis) -> RepoResult<u64> {
        self.transaction(|conn| {
            let ids = query_all(
                conn,
                "SELECT id
                 FROM messages
                 WHERE created_at < ?",
                [cutoff.as_millis()],
                |r| r.get::<_, i64>(0),
            )?;
            let count = ids.len() as u64;
            for id in ids {
                delete_message(
                    conn,
                    MessageId::new(id).ok_or_else(|| invalid("invalid message id"))?,
                    now,
                )?;
            }
            Ok(count)
        })
    }
}

impl RecordWrite for Store {
    fn create_entry(&mut self, entry: &NewEntry) -> RepoResult<RecordEntry> {
        self.transaction(|conn| create_entry(conn, entry))
    }
    fn change_status(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<RecordEntry> {
        self.transaction(|conn| change_status(conn, id, change, actor, at))
    }
    fn set_last_catchup(&mut self, member: MemberId, at: UnixMillis) -> RepoResult<()> {
        self.transaction(|conn| {
            exists(conn, "members", member.get(), "member")?;
            conn.execute(
                "INSERT INTO user_state (member_id, last_catchup_at)
                 VALUES (?, ?)
                 ON CONFLICT (member_id) DO UPDATE
                 SET last_catchup_at = excluded.last_catchup_at",
                params![member.get(), at.as_millis()],
            )
            .map_err(backend)?;
            Ok(())
        })
    }
}

impl TokenWrite for Store {
    fn create_token(
        &mut self,
        name: &str,
        hash: &TokenHash,
        scopes: &str,
        at: UnixMillis,
    ) -> RepoResult<AccessToken> {
        self.transaction(|conn| {
            if scopes != "read" {
                return Err(invalid("v0.1 accepts only read scope"));
            }
            let taken = is_true(
                conn,
                "SELECT EXISTS (
                     SELECT 1
                     FROM access_tokens
                     WHERE name = ?
                        OR token_sha256 = ?
                 )",
                params![name, hash.0.as_slice()],
            )?;
            if taken {
                return Err(RepoError::Conflict {
                    what: "token name or hash",
                });
            }
            conn.execute(
                "INSERT INTO access_tokens (name, token_sha256, scopes, created_at)
                 VALUES (?, ?, ?, ?)",
                params![name, hash.0.as_slice(), scopes, at.as_millis()],
            )
            .map_err(backend)?;
            let sql = format!(
                "SELECT {TOKEN_COLUMNS}
                 FROM access_tokens t
                 WHERE t.id = ?"
            );
            conn.query_row(&sql, [conn.last_insert_rowid()], token_row)
                .map_err(backend)
        })
    }
    fn touch_token(&mut self, id: TokenId, at: UnixMillis) -> RepoResult<()> {
        self.transaction(|conn| {
            // One statement checks and records, so a revocation committed after the
            // caller's lookup makes this fail instead of being overwritten by a use.
            let touched = conn
                .execute(
                    "UPDATE access_tokens
                     SET last_used_at = ?
                     WHERE id = ?
                       AND revoked_at IS NULL",
                    params![at.as_millis(), id.get()],
                )
                .map_err(backend)?;
            if touched == 0 {
                return Err(missing("token"));
            }
            Ok(())
        })
    }
    fn revoke_token(&mut self, name: &str, at: UnixMillis) -> RepoResult<AccessToken> {
        self.transaction(|conn| {
            let revoked = conn
                .execute(
                    "UPDATE access_tokens
                     SET revoked_at = coalesce(revoked_at, ?)
                     WHERE name = ?",
                    params![at.as_millis(), name],
                )
                .map_err(backend)?;
            if revoked == 0 {
                return Err(missing("token"));
            }
            let sql = format!(
                "SELECT {TOKEN_COLUMNS}
                 FROM access_tokens t
                 WHERE t.name = ?"
            );
            conn.query_row(&sql, [name], token_row).map_err(backend)
        })
    }
}
