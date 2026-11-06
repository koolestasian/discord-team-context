//! The retrieval bundle (plan sections 9 and 11, step 1): record entries first, then
//! messages with their thread neighbours, bounded by item count and size.
//!
//! What never goes in: messages from excluded channels (the store never returns them),
//! messages written by bots, and entries whose title was copied from such a message.
//!
//! Every [`Source`] line contains chat-derived text (titles, names, channel names,
//! message content). [`Bundle::framed`] encodes all of it with
//! [`teamctx_core::framing::frame`], so there is no unframed path from chat to prompt.

use std::collections::{BTreeMap, HashMap, HashSet};

use teamctx_core::citation::{CitationId, DiscordMessageLink};
use teamctx_core::framing;
use teamctx_core::id::{ChannelId, MemberId, MessageId, Snowflake, SourceId};
use teamctx_core::query::ast::{Clause, Query};
use teamctx_core::query::context_query;
use teamctx_core::rank;
use teamctx_core::record::{RecordEntry, ShortId};
use teamctx_core::repo::{
    DirectoryRead, MemberInfo, MessageRead, RecordRead, RepoError, RepoResult, StoredMessage,
};
use teamctx_core::time::UnixMillis;

/// Everything the pipeline needs to read from the store. Implemented for every type
/// that implements the three read traits, such as `teamctx_store::Store`.
pub trait Retrieval: RecordRead + MessageRead + DirectoryRead {}

impl<T: RecordRead + MessageRead + DirectoryRead + ?Sized> Retrieval for T {}

/// The most single-word queries run after the all-terms query.
const MAX_TERM_QUERIES: usize = 6;

/// Bounds for one bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleLimits {
    /// Most sources (entries plus messages) in the bundle.
    pub max_items: usize,
    /// Most characters the sources take once framed (JSON escapes count). The default,
    /// 24 000, is about 6 000 tokens (plan section 11, step 1).
    pub max_context_chars: usize,
    /// Longest message or title text in characters; longer text is cut with `…`.
    pub max_source_chars: usize,
    /// Most record entries taken from search.
    pub max_entry_hits: u32,
    /// Most messages taken from search, before neighbours are added.
    pub max_message_hits: u32,
    /// Neighbours kept before each message hit (plan section 9: up to 3 in total).
    pub neighbours_before: u32,
    /// Neighbours kept after each message hit.
    pub neighbours_after: u32,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_items: 24,
            max_context_chars: 24_000,
            max_source_chars: 1_500,
            max_entry_hits: 8,
            max_message_hits: 12,
            neighbours_before: 2,
            neighbours_after: 1,
        }
    }
}

/// One cited item: a record entry or a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    id: CitationId,
    link: Option<DiscordMessageLink>,
    text: String,
}

impl Source {
    /// The id the model must cite, such as `m:123` or `D-12`.
    #[must_use]
    pub const fn id(&self) -> CitationId {
        self.id
    }

    /// The Discord jump link of the message, or of the message an entry cites. `None`
    /// when the entry has no source message or the guild is unknown.
    #[must_use]
    pub const fn link(&self) -> Option<DiscordMessageLink> {
        self.link
    }

    /// The line sent to the model. Untrusted: it contains chat text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The sources for one question, in prompt order: entries by rank, then messages grouped
/// by conversation in time order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bundle {
    sources: Vec<Source>,
}

impl Bundle {
    /// The sources, in prompt order.
    #[must_use]
    pub fn sources(&self) -> &[Source] {
        &self.sources
    }

    /// Whether nothing was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// The ids the model may cite.
    #[must_use]
    pub fn ids(&self) -> HashSet<CitationId> {
        self.sources.iter().map(Source::id).collect()
    }

    /// The whole bundle as one framed block: a fixed notice plus every source line
    /// encoded as a single JSON string between fixed boundaries.
    #[must_use]
    pub fn framed(&self) -> String {
        let lines: Vec<&str> = self.sources.iter().map(Source::text).collect();
        framing::frame(&lines.join("\n"))
    }

    /// Searches the store and assembles a bounded bundle.
    ///
    /// The question becomes keyword queries with [`context_query`]: one with all terms,
    /// then one per term. The store ORs bare words, so the first query ranks items that
    /// match more terms higher and each single-term query adds that term's hits. Hits from all
    /// queries are merged and ranked with [`rank::score`].
    ///
    /// # Errors
    ///
    /// [`RepoError`] if the store fails.
    pub fn retrieve<R: Retrieval + ?Sized>(
        reader: &R,
        question: &str,
        now: UnixMillis,
        limits: &BundleLimits,
    ) -> RepoResult<Self> {
        let queries = search_plan(question);
        let mut builder = Builder::new(limits);
        if queries.is_empty() {
            return Ok(builder.finish());
        }
        let mut lookup = Lookup::new(reader);

        for entry in ranked_entries(reader, &queries, now, limits)? {
            if builder.is_full() {
                break;
            }
            if let Some(source) = lookup.entry_source(&entry, limits)? {
                builder.add_entry(source);
            }
        }

        let hits = ranked_messages(reader, &queries, now, limits)?;
        let mut hit_ids = Vec::new();
        for message in &hits {
            if builder.is_full() {
                break;
            }
            if let Some((key, source)) = lookup.message_source(message, limits)?
                && builder.add_message(key, source)
            {
                hit_ids.push(message.id);
            }
        }

        // Neighbours come after every hit has had its chance, so context never crowds
        // out another answer.
        for id in hit_ids {
            if builder.is_full() {
                break;
            }
            let neighbours =
                match reader.neighbours(id, limits.neighbours_before, limits.neighbours_after) {
                    Ok(messages) => messages,
                    Err(RepoError::NotFound { .. }) => continue,
                    Err(error) => return Err(error),
                };
            for message in &neighbours {
                if let Some((key, source)) = lookup.message_source(message, limits)? {
                    builder.add_message(key, source);
                }
            }
        }
        Ok(builder.finish())
    }
}

/// The queries to run for a question: all terms together, then each term alone.
fn search_plan(question: &str) -> Vec<Query> {
    let terms: Vec<String> = context_query(question)
        .clauses
        .into_iter()
        .filter_map(|clause| match clause {
            Clause::Word(word) => Some(word),
            _ => None,
        })
        .take(MAX_TERM_QUERIES)
        .collect();
    let word_query = |words: &[String]| Query {
        clauses: words.iter().cloned().map(Clause::Word).collect(),
    };
    let mut queries = Vec::new();
    if terms.len() > 1 {
        queries.push(word_query(&terms));
    }
    queries.extend(terms.chunks(1).map(word_query));
    queries
}

fn ranked_entries<R: Retrieval + ?Sized>(
    reader: &R,
    queries: &[Query],
    now: UnixMillis,
    limits: &BundleLimits,
) -> RepoResult<Vec<RecordEntry>> {
    let mut best: BTreeMap<ShortId, (RecordEntry, f64)> = BTreeMap::new();
    for query in queries {
        for hit in reader.search_entries(query, limits.max_entry_hits)? {
            best.entry(hit.entry.short_id())
                .and_modify(|(_, bm25)| *bm25 = bm25.min(hit.bm25))
                .or_insert((hit.entry, hit.bm25));
        }
    }
    let mut ranked: Vec<(f64, RecordEntry)> = best
        .into_values()
        .map(|(entry, bm25)| (rank::score(bm25, entry.updated_at, now, true), entry))
        .collect();
    // BTreeMap order gives a stable tie-break (short id ascending).
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    ranked.truncate(limits.max_entry_hits as usize);
    Ok(ranked.into_iter().map(|(_, entry)| entry).collect())
}

fn ranked_messages<R: Retrieval + ?Sized>(
    reader: &R,
    queries: &[Query],
    now: UnixMillis,
    limits: &BundleLimits,
) -> RepoResult<Vec<StoredMessage>> {
    let mut best: BTreeMap<MessageId, (StoredMessage, f64)> = BTreeMap::new();
    for query in queries {
        for hit in reader.search_messages(query, limits.max_message_hits)? {
            best.entry(hit.message.id)
                .and_modify(|(_, bm25)| *bm25 = bm25.min(hit.bm25))
                .or_insert((hit.message, hit.bm25));
        }
    }
    let mut ranked: Vec<(f64, StoredMessage)> = best
        .into_values()
        .map(|(message, bm25)| (rank::score(bm25, message.created_at, now, false), message))
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    ranked.truncate(limits.max_message_hits as usize);
    Ok(ranked.into_iter().map(|(_, message)| message).collect())
}

/// Orders messages by conversation, then time.
type MessageKey = (ChannelId, UnixMillis, Snowflake);

/// Collects sources while enforcing the item and size limits.
struct Builder {
    max_items: usize,
    max_chars: usize,
    used_chars: usize,
    seen: HashSet<CitationId>,
    entries: Vec<Source>,
    messages: BTreeMap<MessageKey, Source>,
}

impl Builder {
    fn new(limits: &BundleLimits) -> Self {
        Self {
            max_items: limits.max_items,
            max_chars: limits.max_context_chars,
            used_chars: 0,
            seen: HashSet::new(),
            entries: Vec::new(),
            messages: BTreeMap::new(),
        }
    }

    fn is_full(&self) -> bool {
        self.seen.len() >= self.max_items
    }

    /// Reserves room for `source`; `false` if it is a duplicate or does not fit.
    fn reserve(&mut self, source: &Source) -> bool {
        if self.is_full() || self.seen.contains(&source.id) {
            return false;
        }
        let cost = framed_cost(&source.text);
        if self.used_chars.saturating_add(cost) > self.max_chars {
            return false;
        }
        self.used_chars += cost;
        self.seen.insert(source.id);
        true
    }

    fn add_entry(&mut self, source: Source) -> bool {
        let added = self.reserve(&source);
        if added {
            self.entries.push(source);
        }
        added
    }

    fn add_message(&mut self, key: MessageKey, source: Source) -> bool {
        let added = self.reserve(&source);
        if added {
            self.messages.insert(key, source);
        }
        added
    }

    fn finish(self) -> Bundle {
        let mut sources = self.entries;
        sources.extend(self.messages.into_values());
        Bundle { sources }
    }
}

/// How many characters one line adds to the framed block: its JSON-escaped length plus
/// the escaped newline that joins it to the next line. Encoding is per character, so
/// costs add up exactly.
fn framed_cost(line: &str) -> usize {
    framing::frame(&format!("{line}\n"))
        .len()
        .saturating_sub(framing::frame("").len())
}

struct ChannelContext {
    name: String,
    external: Snowflake,
    guild: Option<Snowflake>,
}

/// Cached lookups of members, channels and guilds while building one bundle.
struct Lookup<'a, R: ?Sized> {
    reader: &'a R,
    members: HashMap<MemberId, Option<MemberInfo>>,
    channels: HashMap<ChannelId, Option<ChannelContext>>,
    guilds: HashMap<SourceId, Option<Snowflake>>,
}

impl<'a, R: Retrieval + ?Sized> Lookup<'a, R> {
    fn new(reader: &'a R) -> Self {
        Self {
            reader,
            members: HashMap::new(),
            channels: HashMap::new(),
            guilds: HashMap::new(),
        }
    }

    fn member(&mut self, id: MemberId) -> RepoResult<Option<MemberInfo>> {
        if let Some(known) = self.members.get(&id) {
            return Ok(known.clone());
        }
        let member = self.reader.member(id)?;
        self.members.insert(id, member.clone());
        Ok(member)
    }

    /// The channel if it exists and is included.
    fn channel(&mut self, id: ChannelId) -> RepoResult<Option<&ChannelContext>> {
        if !self.channels.contains_key(&id) {
            let context = match self.reader.channel(id)? {
                Some(info) if info.included => {
                    let guild = self.guild(info.source)?;
                    Some(ChannelContext {
                        name: info.name,
                        external: info.external_id,
                        guild,
                    })
                }
                _ => None,
            };
            self.channels.insert(id, context);
        }
        Ok(self.channels.get(&id).and_then(Option::as_ref))
    }

    fn guild(&mut self, source: SourceId) -> RepoResult<Option<Snowflake>> {
        if let Some(known) = self.guilds.get(&source) {
            return Ok(*known);
        }
        let guild = self.reader.source(source)?.map(|info| info.external_id);
        self.guilds.insert(source, guild);
        Ok(guild)
    }

    /// A message as a source line. `None` for bot authors and unknown or excluded
    /// channels.
    fn message_source(
        &mut self,
        message: &StoredMessage,
        limits: &BundleLimits,
    ) -> RepoResult<Option<(MessageKey, Source)>> {
        let author = match message.author {
            Some(id) => self.member(id)?,
            None => None,
        };
        if author.as_ref().is_some_and(|a| a.is_bot) {
            return Ok(None);
        }
        let Some(channel) = self.channel(message.channel)? else {
            return Ok(None);
        };
        let id = CitationId::Message(message.id);
        let name = author
            .as_ref()
            .map_or("unknown", |a| a.display_name.as_str());
        let text = format!(
            "{} #{} {} {}: {}",
            id.marker(),
            channel.name,
            name,
            civil_date(message.created_at),
            truncate(&message.content, limits.max_source_chars),
        );
        let link = channel.guild.map(|guild| DiscordMessageLink {
            guild,
            channel: channel.external,
            message: message.external_id,
        });
        let key = (message.channel, message.created_at, message.external_id);
        Ok(Some((key, Source { id, link, text })))
    }

    /// An entry as a source line. `None` when its title was copied from a message that
    /// must not be sent (bot author, excluded channel).
    fn entry_source(
        &mut self,
        entry: &RecordEntry,
        limits: &BundleLimits,
    ) -> RepoResult<Option<Source>> {
        let mut link = None;
        if let Some(message_id) = entry.source_message {
            match self.reader.message(message_id)? {
                Some(message) => {
                    let author = match message.author {
                        Some(id) => self.member(id)?,
                        None => None,
                    };
                    if entry.title_is_copied && author.is_some_and(|a| a.is_bot) {
                        return Ok(None);
                    }
                    link = self.channel(message.channel)?.and_then(|channel| {
                        channel.guild.map(|guild| DiscordMessageLink {
                            guild,
                            channel: channel.external,
                            message: message.external_id,
                        })
                    });
                }
                // The cited message is not visible (excluded channel or gone). A copied
                // title would leak its text.
                None if entry.title_is_copied => return Ok(None),
                None => {}
            }
        }
        let id = CitationId::Entry(entry.short_id());
        let mut text = format!(
            "{} {}, {}, created {}",
            id.marker(),
            entry.kind,
            entry.status,
            civil_date(entry.created_at)
        );
        if let Some(owner) = entry.owner
            && let Some(member) = self.member(owner)?
        {
            text.push_str(&format!(", owner {}", member.display_name));
        }
        if let Some(due) = entry.due_at {
            text.push_str(&format!(", due {}", civil_date(due)));
        }
        if let Some(by) = entry.superseded_by {
            let replacement = ShortId {
                kind: entry.kind,
                id: by,
            };
            text.push_str(&format!(", superseded by {replacement}"));
        }
        if !entry.tags.is_empty() {
            let tags: Vec<&str> = entry.tags.iter().map(|t| t.as_str()).collect();
            text.push_str(&format!(", tags {}", tags.join(" ")));
        }
        text.push_str(&format!(
            ": {}",
            truncate(entry.title.as_str(), limits.max_source_chars)
        ));
        Ok(Some(Source { id, link, text }))
    }
}

/// Cuts `text` to `max` characters, adding `…` when something was removed.
fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

/// `YYYY-MM-DD` in UTC. (The server timezone is not known to this crate; the date is
/// context for the model, not a filter.)
fn civil_date(at: UnixMillis) -> String {
    let days = at.as_millis().div_euclid(86_400_000);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_instants() {
        for (millis, expected) in [
            (0, "1970-01-01"),
            (86_399_999, "1970-01-01"),
            (86_400_000, "1970-01-02"),
            (951_782_400_000, "2000-02-29"),
            (1_772_409_600_000, "2026-03-02"),
            (-86_400_000, "1969-12-31"),
        ] {
            assert_eq!(civil_date(UnixMillis::from_millis(millis)), expected);
        }
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        assert_eq!(truncate("abc", 3), "abc");
        assert_eq!(truncate("abcd", 3), "abc…");
        assert_eq!(truncate("雪雪雪雪", 2), "雪雪…");
    }

    #[test]
    fn framed_cost_adds_up_to_the_framed_block() {
        let lines = ["plain", "quote \" and <tag>\nnewline", "é雪"];
        let joined = lines.join("\n");
        let total: usize = lines.iter().map(|l| framed_cost(l)).sum();
        // The last line's escaped newline is not in the block.
        assert_eq!(
            framing::frame(&joined).len() + 2,
            framing::frame("").len() + total
        );
    }

    #[test]
    fn the_search_plan_runs_all_terms_then_each_term() {
        let words = |q: &Query| -> Vec<String> {
            q.clauses
                .iter()
                .filter_map(|c| match c {
                    Clause::Word(w) => Some(w.clone()),
                    _ => None,
                })
                .collect()
        };
        let plan = search_plan("Which typeface do we use for the reading text?");
        assert_eq!(words(&plan[0]), ["typeface", "use", "reading", "text"]);
        assert_eq!(plan.len(), 5);
        assert_eq!(words(&plan[1]), ["typeface"]);
        // One term: no duplicate all-terms query.
        assert_eq!(search_plan("typeface").len(), 1);
        assert!(search_plan("the of and").is_empty());
    }

    #[test]
    fn a_huge_question_gives_a_bounded_search_plan() {
        // `Bundle::retrieve` is public, so it must stay bounded even for text the
        // pipeline's question limit never saw: 100,000 distinct words, no panic.
        let question: String = (0..100_000).map(|i| format!("w{i} ")).collect();
        let plan = search_plan(&question);
        assert_eq!(plan.len(), MAX_TERM_QUERIES + 1);
        assert!(plan.iter().all(|q| q.clauses.len() <= MAX_TERM_QUERIES));
        assert_eq!(plan[0].clauses[0], Clause::Word("w0".to_owned()));
    }
}
