use std::str::FromStr;

use rusqlite::{
    Connection, OptionalExtension, Params, Row, params, params_from_iter, types::Value,
};
use teamctx_core::{
    id::*,
    query::{Clause, Query, fts_expression, fts_expression_all},
    record::*,
    repo::*,
    source::*,
    time::UnixMillis,
};

use crate::{Store, backend};

// Column lists for the row mappers below. Each list assumes the table alias in its
// comment, and each mapper reads columns by name, so adding or reordering a table
// column cannot shift the fields a mapper reads.

/// Table alias `s`.
pub(crate) const SOURCE_COLUMNS: &str = "s.id, s.kind, s.external_id, s.name";

/// Table alias `c`.
pub(crate) const CHANNEL_COLUMNS: &str = "\
    c.id, c.source_id, c.external_id, c.parent_id, c.kind,
    c.name, c.topic, c.is_included, c.archived";

/// Table alias `mb`.
pub(crate) const MEMBER_COLUMNS: &str = "\
    mb.id, mb.source_id, mb.external_id, mb.display_name, mb.is_bot";

/// Table alias `m`.
pub(crate) const MESSAGE_COLUMNS: &str = "\
    m.id, m.channel_id, m.external_id, m.author_id, m.content, m.reply_to_ext,
    m.attachment_names, m.created_at, m.edited_at";

/// Table alias `e`.
pub(crate) const ENTRY_COLUMNS: &str = "\
    e.id, e.kind, e.status, e.title, e.title_is_copied, e.owner_member_id, e.due_at,
    e.source_message_id, e.source_deleted, e.superseded_by, e.answered_by_message_id,
    e.created_by_member_id, e.created_via, e.tags, e.created_at, e.updated_at";

/// Table alias `t`. The token hash is deliberately not selected.
pub(crate) const TOKEN_COLUMNS: &str = "\
    t.id, t.name, t.scopes, t.created_at, t.last_used_at, t.revoked_at";

/// Messages whose channel is included, with the optional author. Aliases `m`, `c`, `a`.
const VISIBLE_MESSAGES: &str = "\
FROM messages m
JOIN channels c ON c.id = m.channel_id
LEFT JOIN members a ON a.id = m.author_id
WHERE c.is_included = 1";

/// Joins that decide whether a record entry is visible. Aliases `m`, `c`, `a`.
const ENTRY_JOINS: &str = "\
LEFT JOIN messages m ON m.id = e.source_message_id
LEFT JOIN channels c ON c.id = m.channel_id
JOIN members a ON a.id = e.created_by_member_id";

// Messages sort by time, then by numeric snowflake: length first, then text.
const NEWEST_MESSAGE_FIRST: &str =
    "m.created_at DESC, length(m.external_id) DESC, m.external_id DESC";
const OLDEST_MESSAGE_FIRST: &str = "m.created_at, length(m.external_id), m.external_id";

fn bad_row() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}
fn parse<T: FromStr>(r: &Row<'_>, column: &str) -> rusqlite::Result<T> {
    r.get::<_, String>(column)?.parse().map_err(|_| bad_row())
}
fn parse_optional<T: FromStr>(r: &Row<'_>, column: &str) -> rusqlite::Result<Option<T>> {
    r.get::<_, Option<String>>(column)?
        .map(|text| text.parse().map_err(|_| bad_row()))
        .transpose()
}
fn millis(r: &Row<'_>, column: &str) -> rusqlite::Result<UnixMillis> {
    Ok(UnixMillis::from_millis(r.get(column)?))
}
fn millis_optional(r: &Row<'_>, column: &str) -> rusqlite::Result<Option<UnixMillis>> {
    Ok(r.get::<_, Option<i64>>(column)?
        .map(UnixMillis::from_millis))
}
macro_rules! row_id {
    ($r:expr, $column:expr, $t:ident) => {
        $t::new($r.get($column)?).ok_or_else(bad_row)?
    };
}
macro_rules! optional_id {
    ($r:expr, $column:expr, $t:ident) => {
        $r.get::<_, Option<i64>>($column)?
            .map(|v| $t::new(v).ok_or_else(bad_row))
            .transpose()?
    };
}

/// Runs `sql` and maps every row.
pub(crate) fn query_all<T>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> RepoResult<Vec<T>> {
    let mut statement = conn.prepare(sql).map_err(backend)?;
    let rows = statement.query_map(params, map).map_err(backend)?;
    rows.collect::<rusqlite::Result<Vec<_>>>().map_err(backend)
}

/// Runs `sql` and maps the first row, if any.
pub(crate) fn query_optional<T>(
    conn: &Connection,
    sql: &str,
    params: impl Params,
    map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> RepoResult<Option<T>> {
    conn.query_row(sql, params, map).optional().map_err(backend)
}

pub(crate) fn source_row(r: &Row<'_>) -> rusqlite::Result<SourceInfo> {
    Ok(SourceInfo {
        id: row_id!(r, "id", SourceId),
        kind: parse(r, "kind")?,
        external_id: parse(r, "external_id")?,
        name: r.get("name")?,
    })
}
pub(crate) fn channel_row(r: &Row<'_>) -> rusqlite::Result<ChannelInfo> {
    Ok(ChannelInfo {
        id: row_id!(r, "id", ChannelId),
        source: row_id!(r, "source_id", SourceId),
        external_id: parse(r, "external_id")?,
        parent: optional_id!(r, "parent_id", ChannelId),
        kind: parse(r, "kind")?,
        name: r.get("name")?,
        topic: r.get("topic")?,
        included: r.get("is_included")?,
        archived: r.get("archived")?,
    })
}
pub(crate) fn member_row(r: &Row<'_>) -> rusqlite::Result<MemberInfo> {
    Ok(MemberInfo {
        id: row_id!(r, "id", MemberId),
        source: row_id!(r, "source_id", SourceId),
        external_id: parse(r, "external_id")?,
        display_name: r.get("display_name")?,
        is_bot: r.get("is_bot")?,
    })
}
pub(crate) fn message_row(conn: &Connection, r: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    let id = row_id!(r, "id", MessageId);
    let mut statement = conn.prepare(
        "SELECT external_user_id
         FROM message_mentions
         WHERE message_id = ?
         ORDER BY length(external_user_id), external_user_id",
    )?;
    let mentions = statement
        .query_map([id.get()], |mention| parse(mention, "external_user_id"))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(StoredMessage {
        id,
        channel: row_id!(r, "channel_id", ChannelId),
        external_id: parse(r, "external_id")?,
        author: optional_id!(r, "author_id", MemberId),
        content: r.get("content")?,
        reply_to: parse_optional(r, "reply_to_ext")?,
        attachment_names: serde_json::from_str(&r.get::<_, String>("attachment_names")?)
            .map_err(|_| bad_row())?,
        created_at: millis(r, "created_at")?,
        edited_at: millis_optional(r, "edited_at")?,
        mentions,
    })
}
pub(crate) fn entry_row(r: &Row<'_>) -> rusqlite::Result<RecordEntry> {
    let tags = r
        .get::<_, String>("tags")?
        .split_whitespace()
        .map(|tag| Tag::new(tag).map_err(|_| bad_row()))
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(RecordEntry {
        id: row_id!(r, "id", EntryId),
        kind: parse(r, "kind")?,
        status: parse(r, "status")?,
        title: EntryTitle::new(r.get::<_, String>("title")?).map_err(|_| bad_row())?,
        title_is_copied: r.get("title_is_copied")?,
        owner: optional_id!(r, "owner_member_id", MemberId),
        due_at: millis_optional(r, "due_at")?,
        source_message: optional_id!(r, "source_message_id", MessageId),
        source_deleted: r.get("source_deleted")?,
        superseded_by: optional_id!(r, "superseded_by", EntryId),
        answered_by_message: optional_id!(r, "answered_by_message_id", MessageId),
        created_by: row_id!(r, "created_by_member_id", MemberId),
        created_via: parse(r, "created_via")?,
        tags,
        created_at: millis(r, "created_at")?,
        updated_at: millis(r, "updated_at")?,
    })
}
pub(crate) fn token_row(r: &Row<'_>) -> rusqlite::Result<AccessToken> {
    Ok(AccessToken {
        id: row_id!(r, "id", TokenId),
        name: r.get("name")?,
        scopes: r.get("scopes")?,
        created_at: millis(r, "created_at")?,
        last_used_at: millis_optional(r, "last_used_at")?,
        revoked_at: millis_optional(r, "revoked_at")?,
    })
}
fn cursor_row(r: &Row<'_>) -> rusqlite::Result<SyncCursor> {
    Ok(SyncCursor {
        newest_seen: parse_optional(r, "newest_seen_ext")?,
        oldest_backfilled: parse_optional(r, "oldest_backfilled_ext")?,
        backfill_done: r.get("backfill_done")?,
        updated_at: millis(r, "updated_at")?,
    })
}
fn activity_row(r: &Row<'_>) -> rusqlite::Result<ChannelActivity> {
    Ok(ChannelActivity {
        channel: row_id!(r, "channel_id", ChannelId),
        messages: u64::try_from(r.get::<_, i64>("message_count")?).map_err(|_| bad_row())?,
    })
}
fn event_row(r: &Row<'_>) -> rusqlite::Result<RecordEvent> {
    let action: String = r.get("action")?;
    let detail: Option<String> = r.get("detail")?;
    Ok(RecordEvent {
        entry: row_id!(r, "entry_id", EntryId),
        actor: optional_id!(r, "actor_member_id", MemberId),
        action: RecordAction::decode(&action, detail.as_deref()).map_err(|_| bad_row())?,
        at: millis(r, "at")?,
    })
}

impl DirectoryRead for Store {
    fn source(&self, id: SourceId) -> RepoResult<Option<SourceInfo>> {
        let sql = format!(
            "SELECT {SOURCE_COLUMNS}
             FROM sources s
             WHERE s.id = ?"
        );
        query_optional(&self.conn, &sql, [id.get()], source_row)
    }
    fn channel(&self, id: ChannelId) -> RepoResult<Option<ChannelInfo>> {
        let sql = format!(
            "SELECT {CHANNEL_COLUMNS}
             FROM channels c
             WHERE c.id = ?"
        );
        query_optional(&self.conn, &sql, [id.get()], channel_row)
    }
    fn channels(&self, source: SourceId) -> RepoResult<Vec<ChannelInfo>> {
        let sql = format!(
            "SELECT {CHANNEL_COLUMNS}
             FROM channels c
             WHERE c.source_id = ?
             ORDER BY c.id"
        );
        query_all(&self.conn, &sql, [source.get()], channel_row)
    }
    fn member(&self, id: MemberId) -> RepoResult<Option<MemberInfo>> {
        let sql = format!(
            "SELECT {MEMBER_COLUMNS}
             FROM members mb
             WHERE mb.id = ?"
        );
        query_optional(&self.conn, &sql, [id.get()], member_row)
    }
    fn member_by_external(
        &self,
        source: SourceId,
        external_id: Snowflake,
    ) -> RepoResult<Option<MemberInfo>> {
        let sql = format!(
            "SELECT {MEMBER_COLUMNS}
             FROM members mb
             WHERE mb.source_id = ?
               AND mb.external_id = ?"
        );
        let params = params![source.get(), external_id.to_string()];
        query_optional(&self.conn, &sql, params, member_row)
    }
    fn cursor(&self, source: SourceId, channel: Snowflake) -> RepoResult<Option<SyncCursor>> {
        let sql = "SELECT s.newest_seen_ext, s.oldest_backfilled_ext, s.backfill_done,
                          s.updated_at
                   FROM sync_cursors s
                   JOIN channels c ON c.id = s.channel_id
                   WHERE c.source_id = ?
                     AND c.external_id = ?";
        let params = params![source.get(), channel.to_string()];
        query_optional(&self.conn, sql, params, cursor_row)
    }
    fn last_catchup(&self, member: MemberId) -> RepoResult<Option<UnixMillis>> {
        let sql = "SELECT last_catchup_at
                   FROM user_state
                   WHERE member_id = ?";
        let at = query_optional(&self.conn, sql, [member.get()], |r| {
            r.get("last_catchup_at")
        })?;
        Ok(at.map(UnixMillis::from_millis))
    }
}

impl Store {
    /// The member whose display name equals `name`, ignoring ASCII case and surrounding
    /// whitespace. Names are not unique; the lowest member id wins, so the answer is
    /// stable. Parameterised SQL, so `name` may hold anything.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    pub fn member_by_display_name(&self, name: &str) -> RepoResult<Option<MemberInfo>> {
        let sql = format!(
            "SELECT {MEMBER_COLUMNS}
             FROM members mb
             WHERE mb.display_name = ? COLLATE NOCASE
             ORDER BY mb.id
             LIMIT 1"
        );
        query_optional(&self.conn, &sql, [name.trim()], member_row)
    }

    fn messages_sql(&self, sql: &str, values: Vec<Value>) -> RepoResult<Vec<StoredMessage>> {
        query_all(&self.conn, sql, params_from_iter(values), |r| {
            message_row(&self.conn, r)
        })
    }
    /// Appends the `from:`, `in:`, `before:` and `after:` clauses of `query` to a statement
    /// that already has a WHERE clause. `entry` selects the record-entry date column.
    fn filters(
        &self,
        query: &Query,
        entry: bool,
        sql: &mut String,
        values: &mut Vec<Value>,
    ) -> RepoResult<()> {
        let created_at = if entry {
            "e.created_at"
        } else {
            "m.created_at"
        };
        for clause in &query.clauses {
            match clause {
                Clause::From(name) => {
                    sql.push_str("\n  AND a.display_name = ? COLLATE NOCASE");
                    values.push(name.clone().into());
                }
                Clause::In(name) => {
                    sql.push_str("\n  AND c.name = ? COLLATE NOCASE");
                    values.push(name.clone().into());
                }
                Clause::Before(date) => {
                    sql.push_str(&format!("\n  AND {created_at} < ?"));
                    values.push(self.date(*date)?.as_millis().into());
                }
                Clause::After(date) => {
                    sql.push_str(&format!("\n  AND {created_at} >= ?"));
                    values.push(self.date(*date)?.as_millis().into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

impl MessageRead for Store {
    fn message(&self, id: MessageId) -> RepoResult<Option<StoredMessage>> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             {VISIBLE_MESSAGES}
               AND m.id = ?"
        );
        let found = self.messages_sql(&sql, vec![id.get().into()])?;
        Ok(found.into_iter().next())
    }
    fn message_by_ref(
        &self,
        source: SourceId,
        id: MessageRef,
    ) -> RepoResult<Option<StoredMessage>> {
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             {VISIBLE_MESSAGES}
               AND c.source_id = ?
               AND c.external_id = ?
               AND m.external_id = ?"
        );
        let values = vec![
            source.get().into(),
            id.channel.to_string().into(),
            id.message.to_string().into(),
        ];
        let found = self.messages_sql(&sql, values)?;
        Ok(found.into_iter().next())
    }
    fn neighbours(&self, id: MessageId, before: u32, after: u32) -> RepoResult<Vec<StoredMessage>> {
        let message = self
            .message(id)?
            .ok_or(RepoError::NotFound { what: "message" })?;
        let bounds = |limit: u32| {
            vec![
                message.channel.get().into(),
                message.created_at.as_millis().into(),
                message.external_id.to_string().into(),
                message.external_id.to_string().into(),
                i64::from(limit).into(),
            ]
        };
        let before_sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             {VISIBLE_MESSAGES}
               AND m.channel_id = ?
               AND (m.created_at, length(m.external_id), m.external_id) < (?, length(?), ?)
             ORDER BY {NEWEST_MESSAGE_FIRST}
             LIMIT ?"
        );
        let after_sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             {VISIBLE_MESSAGES}
               AND m.channel_id = ?
               AND (m.created_at, length(m.external_id), m.external_id) > (?, length(?), ?)
             ORDER BY {OLDEST_MESSAGE_FIRST}
             LIMIT ?"
        );
        let mut preceding = self.messages_sql(&before_sql, bounds(before))?;
        preceding.reverse();
        let following = self.messages_sql(&after_sql, bounds(after))?;
        preceding.push(message);
        preceding.extend(following);
        Ok(preceding)
    }
    fn search_messages(&self, query: &Query, limit: u32) -> RepoResult<Vec<MessageHit>> {
        if query.clauses.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        let mut sql = if let Some(expression) = fts_expression(query) {
            values.push(expression.into());
            format!(
                "SELECT {MESSAGE_COLUMNS}, bm25(messages_fts) AS score
                 FROM messages_fts
                 JOIN messages m ON m.id = messages_fts.rowid
                 JOIN channels c ON c.id = m.channel_id
                 LEFT JOIN members a ON a.id = m.author_id
                 WHERE c.is_included = 1
                   AND messages_fts MATCH ?"
            )
        } else {
            format!(
                "SELECT {MESSAGE_COLUMNS}, 0.0 AS score
                 {VISIBLE_MESSAGES}"
            )
        };
        self.filters(query, false, &mut sql, &mut values)?;
        sql.push_str(&format!(
            "\nORDER BY score, {NEWEST_MESSAGE_FIRST}\nLIMIT ?"
        ));
        values.push(i64::from(limit).into());
        query_all(&self.conn, &sql, params_from_iter(values), |r| {
            Ok(MessageHit {
                message: message_row(&self.conn, r)?,
                bm25: r.get("score")?,
            })
        })
    }
    fn channel_activity_since(
        &self,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<ChannelActivity>> {
        let sql = "SELECT m.channel_id, count(*) AS message_count
                   FROM messages m
                   JOIN channels c ON c.id = m.channel_id
                   WHERE c.is_included = 1
                     AND m.created_at >= ?
                   GROUP BY m.channel_id
                   ORDER BY count(*) DESC, m.channel_id
                   LIMIT ?";
        let params = params![since.as_millis(), i64::from(limit)];
        query_all(&self.conn, sql, params, activity_row)
    }
    fn mentions_since(
        &self,
        member: MemberId,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<StoredMessage>> {
        let member = self
            .member(member)?
            .ok_or(RepoError::NotFound { what: "member" })?;
        let sql = format!(
            "SELECT {MESSAGE_COLUMNS}
             {VISIBLE_MESSAGES}
               AND c.source_id = ?
               AND m.created_at >= ?
               AND EXISTS (
                   SELECT 1
                   FROM message_mentions mm
                   WHERE mm.message_id = m.id
                     AND mm.external_user_id = ?
               )
             ORDER BY {NEWEST_MESSAGE_FIRST}
             LIMIT ?"
        );
        let values = vec![
            member.source.get().into(),
            since.as_millis().into(),
            member.external_id.to_string().into(),
            i64::from(limit).into(),
        ];
        self.messages_sql(&sql, values)
    }
}

impl RecordRead for Store {
    fn entry(&self, id: ShortId) -> RepoResult<Option<RecordEntry>> {
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM record_entries e
             WHERE e.id = ?
               AND e.kind = ?"
        );
        let params = params![id.id.get(), id.kind.as_str()];
        query_optional(&self.conn, &sql, params, entry_row)
    }
    fn list_entries(
        &self,
        filter: &EntryFilter,
        page: PageRequest,
    ) -> RepoResult<Vec<RecordEntry>> {
        let mut conditions = Vec::new();
        let mut values = Vec::<Value>::new();
        if let Some(kind) = filter.kind {
            conditions.push("e.kind = ?");
            values.push(kind.as_str().to_owned().into());
        }
        if let Some(status) = filter.status {
            conditions.push("e.status = ?");
            values.push(status.as_str().to_owned().into());
        }
        if let Some(owner) = filter.owner {
            conditions.push("e.owner_member_id = ?");
            values.push(owner.get().into());
        }
        if let Some(at) = filter.updated_since {
            conditions.push("e.updated_at >= ?");
            values.push(at.as_millis().into());
        }
        if let Some(topic) = &filter.topic {
            let words = topic
                .split_whitespace()
                .map(|word| Clause::Word(word.to_owned()))
                .collect();
            // A topic narrows the list, so every word is required (unlike a search).
            if let Some(expression) = fts_expression_all(&Query { clauses: words }) {
                conditions.push("e.id IN (SELECT rowid FROM record_fts WHERE record_fts MATCH ?)");
                values.push(expression.into());
            }
        }
        let conditions = if conditions.is_empty() {
            "1 = 1".to_owned()
        } else {
            conditions.join("\n  AND ")
        };
        let sql = format!(
            "SELECT {ENTRY_COLUMNS}
             FROM record_entries e
             WHERE {conditions}
             ORDER BY e.updated_at DESC, e.id DESC
             LIMIT ? OFFSET ?"
        );
        values.push(i64::from(page.limit).into());
        values.push(i64::from(page.offset).into());
        query_all(&self.conn, &sql, params_from_iter(values), entry_row)
    }
    fn search_entries(&self, query: &Query, limit: u32) -> RepoResult<Vec<EntryHit>> {
        if query.clauses.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        let mut sql = if let Some(expression) = fts_expression(query) {
            values.push(expression.into());
            format!(
                "SELECT {ENTRY_COLUMNS}, bm25(record_fts) AS score
                 FROM record_fts
                 JOIN record_entries e ON e.id = record_fts.rowid
                 {ENTRY_JOINS}
                 WHERE (c.id IS NULL OR c.is_included = 1)
                   AND record_fts MATCH ?"
            )
        } else {
            format!(
                "SELECT {ENTRY_COLUMNS}, 0.0 AS score
                 FROM record_entries e
                 {ENTRY_JOINS}
                 WHERE (c.id IS NULL OR c.is_included = 1)"
            )
        };
        self.filters(query, true, &mut sql, &mut values)?;
        sql.push_str("\nORDER BY score, e.created_at DESC, e.id DESC\nLIMIT ?");
        values.push(i64::from(limit).into());
        query_all(&self.conn, &sql, params_from_iter(values), |r| {
            Ok(EntryHit {
                entry: entry_row(r)?,
                bm25: r.get("score")?,
            })
        })
    }
    fn entry_events(&self, id: EntryId) -> RepoResult<Vec<RecordEvent>> {
        let exists = query_optional(
            &self.conn,
            "SELECT EXISTS (
                 SELECT 1
                 FROM record_entries
                 WHERE id = ?
             )",
            [id.get()],
            |r| r.get::<_, bool>(0),
        )?;
        if exists != Some(true) {
            return Err(RepoError::NotFound { what: "entry" });
        }
        let sql = "SELECT entry_id, actor_member_id, action, detail, at
                   FROM record_events
                   WHERE entry_id = ?
                   ORDER BY at, id";
        query_all(&self.conn, sql, [id.get()], event_row)
    }
}

impl TokenRead for Store {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        let sql = format!(
            "SELECT {TOKEN_COLUMNS}
             FROM access_tokens t
             WHERE t.token_sha256 = ?
               AND t.revoked_at IS NULL"
        );
        query_optional(&self.conn, &sql, [hash.0.as_slice()], token_row)
    }
    fn tokens(&self) -> RepoResult<Vec<AccessToken>> {
        let sql = format!(
            "SELECT {TOKEN_COLUMNS}
             FROM access_tokens t
             ORDER BY t.name"
        );
        query_all(&self.conn, &sql, [], token_row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_with_members(names: &[&str]) -> Store {
        let mut db = Store::in_memory().unwrap();
        let source = db
            .upsert_source(SourceKind::Discord, Snowflake::new(1).unwrap(), "Fiction")
            .unwrap();
        for (n, name) in names.iter().enumerate() {
            let member = Member {
                id: Snowflake::new(100 + n as u64).unwrap(),
                display_name: (*name).to_owned(),
                is_bot: false,
            };
            db.apply_event(
                source,
                &SourceEvent::MemberUpsert(member),
                UnixMillis::from_millis(1),
            )
            .unwrap();
        }
        db
    }

    #[test]
    fn member_lookup_by_name_ignores_case_and_edge_whitespace() {
        let db = store_with_members(&["Piper Fiction", "Quinn Invented"]);
        let found = db
            .member_by_display_name("  quinn INVENTED ")
            .unwrap()
            .unwrap();
        assert_eq!(found.display_name, "Quinn Invented");
        assert!(db.member_by_display_name("Quinn").unwrap().is_none());
        assert!(db.member_by_display_name("").unwrap().is_none());
    }

    #[test]
    fn member_lookup_treats_the_name_as_data() {
        let db = store_with_members(&["Piper Fiction"]);
        for hostile in ["' OR 1=1 --", "%", "Piper%", "Piper_Fiction"] {
            assert!(db.member_by_display_name(hostile).unwrap().is_none());
        }
    }

    #[test]
    fn member_lookup_finds_ids_after_a_gap_and_far_from_the_start() {
        let names: Vec<String> = (0..700).map(|n| format!("Filler {n}")).collect();
        let mut refs: Vec<&str> = names.iter().map(String::as_str).collect();
        refs.push("Zed Lateman");
        let db = store_with_members(&refs);
        // Leave a hole of 300 ids in front of the late member.
        db.conn
            .execute("DELETE FROM members WHERE id BETWEEN 100 AND 400", [])
            .unwrap();
        let found = db.member_by_display_name("zed lateman").unwrap().unwrap();
        assert!(found.id.get() > 500, "{}", found.id);
    }

    #[test]
    fn duplicate_names_resolve_to_the_lowest_id() {
        let db = store_with_members(&["Same Name", "same name"]);
        let found = db.member_by_display_name("SAME NAME").unwrap().unwrap();
        assert_eq!(found.id.get(), 1);
    }
}
