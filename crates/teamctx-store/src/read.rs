use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter, types::Value};
use teamctx_core::{
    id::*,
    query::{Clause, Query, fts_expression},
    record::*,
    repo::*,
    source::*,
    time::UnixMillis,
};

use crate::{Store, backend};

fn bad_row() -> rusqlite::Error {
    rusqlite::Error::InvalidQuery
}
pub(crate) fn parse<T: FromStr>(text: &str) -> rusqlite::Result<T> {
    text.parse().map_err(|_| bad_row())
}
macro_rules! row_id {
    ($r:expr,$n:expr,$t:ident) => {
        $t::new($r.get($n)?).ok_or_else(bad_row)?
    };
}
macro_rules! optional_id {
    ($r:expr,$n:expr,$t:ident) => {
        $r.get::<_, Option<i64>>($n)?
            .map(|v| $t::new(v).ok_or_else(bad_row))
            .transpose()?
    };
}

pub(crate) fn source_row(r: &Row<'_>) -> rusqlite::Result<SourceInfo> {
    Ok(SourceInfo {
        id: row_id!(r, 0, SourceId),
        kind: parse(&r.get::<_, String>(1)?)?,
        external_id: parse(&r.get::<_, String>(2)?)?,
        name: r.get(3)?,
    })
}
pub(crate) fn channel_row(r: &Row<'_>) -> rusqlite::Result<ChannelInfo> {
    Ok(ChannelInfo {
        id: row_id!(r, 0, ChannelId),
        source: row_id!(r, 1, SourceId),
        external_id: parse(&r.get::<_, String>(2)?)?,
        parent: optional_id!(r, 3, ChannelId),
        kind: parse(&r.get::<_, String>(4)?)?,
        name: r.get(5)?,
        topic: r.get(6)?,
        included: r.get(7)?,
        archived: r.get(8)?,
    })
}
pub(crate) fn member_row(r: &Row<'_>) -> rusqlite::Result<MemberInfo> {
    Ok(MemberInfo {
        id: row_id!(r, 0, MemberId),
        source: row_id!(r, 1, SourceId),
        external_id: parse(&r.get::<_, String>(2)?)?,
        display_name: r.get(3)?,
        is_bot: r.get(4)?,
    })
}
pub(crate) fn message_row(conn: &Connection, r: &Row<'_>) -> rusqlite::Result<StoredMessage> {
    let id = row_id!(r, 0, MessageId);
    let mut stmt=conn.prepare("SELECT external_user_id FROM message_mentions WHERE message_id=? ORDER BY length(external_user_id),external_user_id")?;
    let mentions = stmt
        .query_map([id.get()], |r| parse(&r.get::<_, String>(0)?))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(StoredMessage {
        id,
        channel: row_id!(r, 1, ChannelId),
        external_id: parse(&r.get::<_, String>(2)?)?,
        author: optional_id!(r, 3, MemberId),
        content: r.get(4)?,
        reply_to: r
            .get::<_, Option<String>>(5)?
            .map(|v| parse(&v))
            .transpose()?,
        attachment_names: serde_json::from_str(&r.get::<_, String>(7)?).map_err(|_| bad_row())?,
        created_at: UnixMillis::from_millis(r.get(8)?),
        edited_at: r.get::<_, Option<i64>>(9)?.map(UnixMillis::from_millis),
        mentions,
    })
}
pub(crate) fn entry_row(r: &Row<'_>) -> rusqlite::Result<RecordEntry> {
    Ok(RecordEntry {
        id: row_id!(r, 0, EntryId),
        kind: parse(&r.get::<_, String>(1)?)?,
        status: parse(&r.get::<_, String>(2)?)?,
        title: EntryTitle::new(r.get::<_, String>(3)?).map_err(|_| bad_row())?,
        title_is_copied: r.get(4)?,
        owner: optional_id!(r, 5, MemberId),
        due_at: r.get::<_, Option<i64>>(6)?.map(UnixMillis::from_millis),
        source_message: optional_id!(r, 7, MessageId),
        source_deleted: r.get(8)?,
        superseded_by: optional_id!(r, 9, EntryId),
        answered_by_message: optional_id!(r, 10, MessageId),
        created_by: row_id!(r, 11, MemberId),
        created_via: parse(&r.get::<_, String>(12)?)?,
        tags: r
            .get::<_, String>(13)?
            .split_whitespace()
            .map(|s| Tag::new(s).map_err(|_| bad_row()))
            .collect::<rusqlite::Result<Vec<_>>>()?,
        created_at: UnixMillis::from_millis(r.get(14)?),
        updated_at: UnixMillis::from_millis(r.get(15)?),
    })
}
pub(crate) fn token_row(r: &Row<'_>) -> rusqlite::Result<AccessToken> {
    Ok(AccessToken {
        id: row_id!(r, 0, TokenId),
        name: r.get(1)?,
        scopes: r.get(3)?,
        created_at: UnixMillis::from_millis(r.get(4)?),
        last_used_at: r.get::<_, Option<i64>>(5)?.map(UnixMillis::from_millis),
        revoked_at: r.get::<_, Option<i64>>(6)?.map(UnixMillis::from_millis),
    })
}

impl DirectoryRead for Store {
    fn source(&self, id: SourceId) -> RepoResult<Option<SourceInfo>> {
        self.conn
            .query_row("SELECT * FROM sources WHERE id=?", [id.get()], source_row)
            .optional()
            .map_err(backend)
    }
    fn channel(&self, id: ChannelId) -> RepoResult<Option<ChannelInfo>> {
        self.conn
            .query_row("SELECT * FROM channels WHERE id=?", [id.get()], channel_row)
            .optional()
            .map_err(backend)
    }
    fn channels(&self, source: SourceId) -> RepoResult<Vec<ChannelInfo>> {
        self.conn
            .prepare("SELECT * FROM channels WHERE source_id=? ORDER BY id")
            .map_err(backend)?
            .query_map([source.get()], channel_row)
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
    fn member(&self, id: MemberId) -> RepoResult<Option<MemberInfo>> {
        self.conn
            .query_row("SELECT * FROM members WHERE id=?", [id.get()], member_row)
            .optional()
            .map_err(backend)
    }
    fn member_by_external(
        &self,
        source: SourceId,
        external_id: Snowflake,
    ) -> RepoResult<Option<MemberInfo>> {
        self.conn
            .query_row(
                "SELECT * FROM members WHERE source_id=? AND external_id=?",
                params![source.get(), external_id.to_string()],
                member_row,
            )
            .optional()
            .map_err(backend)
    }
    fn cursor(&self, source: SourceId, channel: Snowflake) -> RepoResult<Option<SyncCursor>> {
        self.conn.query_row("SELECT s.newest_seen_ext,s.oldest_backfilled_ext,s.backfill_done,s.updated_at FROM sync_cursors s JOIN channels c ON c.id=s.channel_id WHERE c.source_id=? AND c.external_id=?",params![source.get(),channel.to_string()],|r|Ok(SyncCursor{newest_seen:r.get::<_,Option<String>>(0)?.map(|v|parse(&v)).transpose()?,oldest_backfilled:r.get::<_,Option<String>>(1)?.map(|v|parse(&v)).transpose()?,backfill_done:r.get(2)?,updated_at:UnixMillis::from_millis(r.get(3)?)})).optional().map_err(backend)
    }
    fn last_catchup(&self, member: MemberId) -> RepoResult<Option<UnixMillis>> {
        self.conn
            .query_row(
                "SELECT last_catchup_at FROM user_state WHERE member_id=?",
                [member.get()],
                |r| r.get(0),
            )
            .optional()
            .map(|v| v.map(UnixMillis::from_millis))
            .map_err(backend)
    }
}

pub(crate) const VISIBLE: &str = " FROM messages m JOIN channels c ON c.id=m.channel_id LEFT JOIN members a ON a.id=m.author_id WHERE c.is_included=1";

impl Store {
    fn messages_sql(&self, sql: &str, values: Vec<Value>) -> RepoResult<Vec<StoredMessage>> {
        self.conn
            .prepare(sql)
            .map_err(backend)?
            .query_map(params_from_iter(values), |r| message_row(&self.conn, r))
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
    fn filters(
        &self,
        query: &Query,
        entry: bool,
        sql: &mut String,
        values: &mut Vec<Value>,
    ) -> RepoResult<()> {
        for clause in &query.clauses {
            match clause {
                Clause::From(name) => {
                    sql.push_str(" AND a.display_name=? COLLATE NOCASE");
                    values.push(name.clone().into());
                }
                Clause::In(name) => {
                    sql.push_str(" AND c.name=? COLLATE NOCASE");
                    values.push(name.clone().into());
                }
                Clause::Before(date) | Clause::After(date) => {
                    let column = if entry {
                        "e.created_at"
                    } else {
                        "m.created_at"
                    };
                    sql.push_str(&format!(
                        " AND {column} {} ?",
                        if matches!(clause, Clause::Before(_)) {
                            "<"
                        } else {
                            ">="
                        }
                    ));
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
        Ok(self
            .messages_sql(
                &format!("SELECT m.*{VISIBLE} AND m.id=?"),
                vec![id.get().into()],
            )?
            .into_iter()
            .next())
    }
    fn message_by_ref(
        &self,
        source: SourceId,
        id: MessageRef,
    ) -> RepoResult<Option<StoredMessage>> {
        Ok(self
            .messages_sql(
                &format!(
                    "SELECT m.*{VISIBLE} AND c.source_id=? AND c.external_id=? AND m.external_id=?"
                ),
                vec![
                    source.get().into(),
                    id.channel.to_string().into(),
                    id.message.to_string().into(),
                ],
            )?
            .into_iter()
            .next())
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
        let mut neighbours=self.messages_sql(
            &format!("SELECT m.*{VISIBLE} AND m.channel_id=? AND (m.created_at,length(m.external_id),m.external_id)<(?,length(?),?) ORDER BY m.created_at DESC,length(m.external_id) DESC,m.external_id DESC LIMIT ?"),
            bounds(before),
        )?;
        neighbours.reverse();
        let following=self.messages_sql(
            &format!("SELECT m.*{VISIBLE} AND m.channel_id=? AND (m.created_at,length(m.external_id),m.external_id)>(?,length(?),?) ORDER BY m.created_at,length(m.external_id),m.external_id LIMIT ?"),
            bounds(after),
        )?;
        neighbours.push(message);
        neighbours.extend(following);
        Ok(neighbours)
    }
    fn search_messages(&self, query: &Query, limit: u32) -> RepoResult<Vec<MessageHit>> {
        if query.clauses.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let expression = fts_expression(query);
        let mut values = Vec::new();
        let mut sql = if let Some(expression) = expression {
            values.push(expression.into());
            "SELECT m.*,bm25(messages_fts) FROM messages_fts JOIN messages m ON m.id=messages_fts.rowid JOIN channels c ON c.id=m.channel_id LEFT JOIN members a ON a.id=m.author_id WHERE c.is_included=1 AND messages_fts MATCH ?".to_owned()
        } else {
            format!("SELECT m.*,0.0{VISIBLE}")
        };
        self.filters(query, false, &mut sql, &mut values)?;
        sql.push_str(
            " ORDER BY 11,m.created_at DESC,length(m.external_id) DESC,m.external_id DESC LIMIT ?",
        );
        values.push(i64::from(limit).into());
        self.conn
            .prepare(&sql)
            .map_err(backend)?
            .query_map(params_from_iter(values), |r| {
                Ok(MessageHit {
                    message: message_row(&self.conn, r)?,
                    bm25: r.get(10)?,
                })
            })
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
    fn channel_activity_since(
        &self,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<ChannelActivity>> {
        self.conn.prepare("SELECT m.channel_id,count(*) FROM messages m JOIN channels c ON c.id=m.channel_id WHERE c.is_included=1 AND m.created_at>=? GROUP BY m.channel_id ORDER BY count(*) DESC,m.channel_id LIMIT ?").map_err(backend)?.query_map(params![since.as_millis(),i64::from(limit)],|r|Ok(ChannelActivity{channel:row_id!(r,0,ChannelId),messages:u64::try_from(r.get::<_,i64>(1)?).map_err(|_|bad_row())?})).map_err(backend)?.collect::<rusqlite::Result<_>>().map_err(backend)
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
        self.messages_sql(&format!("SELECT m.*{VISIBLE} AND c.source_id=? AND m.created_at>=? AND EXISTS(SELECT 1 FROM message_mentions mm WHERE mm.message_id=m.id AND mm.external_user_id=?) ORDER BY m.created_at DESC,length(m.external_id) DESC,m.external_id DESC LIMIT ?"),vec![member.source.get().into(),since.as_millis().into(),member.external_id.to_string().into(),i64::from(limit).into()])
    }
}

impl RecordRead for Store {
    fn entry(&self, id: ShortId) -> RepoResult<Option<RecordEntry>> {
        self.conn
            .query_row(
                "SELECT * FROM record_entries WHERE id=? AND kind=?",
                params![id.id.get(), id.kind.as_str()],
                entry_row,
            )
            .optional()
            .map_err(backend)
    }
    fn list_entries(
        &self,
        filter: &EntryFilter,
        page: PageRequest,
    ) -> RepoResult<Vec<RecordEntry>> {
        let mut sql = "SELECT e.* FROM record_entries e WHERE 1=1".to_owned();
        let mut values = Vec::<Value>::new();
        if let Some(kind) = filter.kind {
            sql.push_str(" AND e.kind=?");
            values.push(kind.as_str().to_owned().into());
        }
        if let Some(status) = filter.status {
            sql.push_str(" AND e.status=?");
            values.push(status.as_str().to_owned().into());
        }
        if let Some(owner) = filter.owner {
            sql.push_str(" AND e.owner_member_id=?");
            values.push(owner.get().into());
        }
        if let Some(at) = filter.updated_since {
            sql.push_str(" AND e.updated_at>=?");
            values.push(at.as_millis().into());
        }
        if let Some(topic) = &filter.topic {
            let query = Query {
                clauses: topic
                    .split_whitespace()
                    .map(|s| Clause::Word(s.to_owned()))
                    .collect(),
            };
            if let Some(expression) = fts_expression(&query) {
                sql.push_str(
                    " AND e.id IN (SELECT rowid FROM record_fts WHERE record_fts MATCH ?)",
                );
                values.push(expression.into());
            }
        }
        sql.push_str(" ORDER BY e.updated_at DESC,e.id DESC LIMIT ? OFFSET ?");
        values.push(i64::from(page.limit).into());
        values.push(i64::from(page.offset).into());
        self.conn
            .prepare(&sql)
            .map_err(backend)?
            .query_map(params_from_iter(values), entry_row)
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
    fn search_entries(&self, query: &Query, limit: u32) -> RepoResult<Vec<EntryHit>> {
        if query.clauses.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let mut values = Vec::new();
        let expression = fts_expression(query);
        let mut sql = if let Some(expression) = expression {
            values.push(expression.into());
            "SELECT e.*,bm25(record_fts) FROM record_fts JOIN record_entries e ON e.id=record_fts.rowid".to_owned()
        } else {
            "SELECT e.*,0.0 FROM record_entries e".to_owned()
        };
        sql.push_str(" LEFT JOIN messages m ON m.id=e.source_message_id LEFT JOIN channels c ON c.id=m.channel_id JOIN members a ON a.id=e.created_by_member_id WHERE (c.id IS NULL OR c.is_included=1)");
        if !values.is_empty() {
            sql.push_str(" AND record_fts MATCH ?");
        }
        self.filters(query, true, &mut sql, &mut values)?;
        sql.push_str(" ORDER BY 17,e.created_at DESC,e.id DESC LIMIT ?");
        values.push(i64::from(limit).into());
        self.conn
            .prepare(&sql)
            .map_err(backend)?
            .query_map(params_from_iter(values), |r| {
                Ok(EntryHit {
                    entry: entry_row(r)?,
                    bm25: r.get(16)?,
                })
            })
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
    fn entry_events(&self, id: EntryId) -> RepoResult<Vec<RecordEvent>> {
        if !self
            .conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM record_entries WHERE id=?)",
                [id.get()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(backend)?
        {
            return Err(RepoError::NotFound { what: "entry" });
        }
        self.conn.prepare("SELECT entry_id,actor_member_id,action,detail,at FROM record_events WHERE entry_id=? ORDER BY at,id").map_err(backend)?.query_map([id.get()],|r|{let action:String=r.get(2)?;let detail:Option<String>=r.get(3)?;Ok(RecordEvent{entry:row_id!(r,0,EntryId),actor:optional_id!(r,1,MemberId),action:RecordAction::decode(&action,detail.as_deref()).map_err(|_|bad_row())?,at:UnixMillis::from_millis(r.get(4)?)})}).map_err(backend)?.collect::<rusqlite::Result<_>>().map_err(backend)
    }
}

impl TokenRead for Store {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        self.conn
            .query_row(
                "SELECT * FROM access_tokens WHERE token_sha256=? AND revoked_at IS NULL",
                [hash.0.as_slice()],
                token_row,
            )
            .optional()
            .map_err(backend)
    }
    fn tokens(&self) -> RepoResult<Vec<AccessToken>> {
        self.conn
            .prepare("SELECT * FROM access_tokens ORDER BY name")
            .map_err(backend)?
            .query_map([], token_row)
            .map_err(backend)?
            .collect::<rusqlite::Result<_>>()
            .map_err(backend)
    }
}
