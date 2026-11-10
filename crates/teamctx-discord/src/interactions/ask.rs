//! `/ask` and `/catchup summarize:true`: the two commands that use a language model
//! (plan sections 7.3 and 11).
//!
//! This crate does not depend on `teamctx-llm` (plan section 5.1). The host injects an
//! [`AskBackend`] that runs the `/ask` pipeline; without one, `/ask` and the `summarize`
//! option are not registered and a stale invocation gets a fixed note.
//!
//! The split of work:
//!
//! - the handler ([`super::handler`]) resolves the member, computes the channels they
//!   may read (and, for a `public:true` reply, that `@everyone` may read) and builds an
//!   [`LlmTask`]; for a catch-up it also builds the usual reply and lists its items;
//! - the service defers the interaction, calls the backend once and edits the original
//!   response with [`render`];
//! - the backend retrieves only from that [`LlmRequest::scope`], calls the model once
//!   without tools, validates the citations and removes every URL from the text.
//!
//! What reaches Discord: the validated answer, escaped line by line with
//! [`render::inline`](super::render) (no markdown, mentions, links or invisible
//! characters survive), a list of the cited sources with jump links built from numeric
//! ids, and fixed notes for refusals and failures. Never the provider's own error text,
//! a key, or the question echoed back.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;

use teamctx_core::citation::{CitationId, DiscordMessageLink};
use teamctx_core::id::{ChannelId, MemberId, MessageId};
use teamctx_core::record::ShortId;
use teamctx_core::time::UnixMillis;

use super::handler::Reply;
use super::render::{self, MAX_DESCRIPTION, MAX_ITEMS};

/// What to ask the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmJob {
    /// `/ask question`. The question is untrusted and never logged or echoed.
    Ask {
        /// As typed.
        question: String,
    },
    /// `/catchup summarize:true`: summarise these items, all visible to the member.
    Summarize {
        /// Record entries the catch-up lists.
        entries: Vec<ShortId>,
        /// Messages the catch-up lists (mentions of the member).
        messages: Vec<MessageId>,
    },
}

/// One request to the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRequest {
    /// Who asked; budgets are charged to them.
    pub member: MemberId,
    /// Now, from the service clock.
    pub now: UnixMillis,
    /// The only channels (store ids, threads included) sources may come from.
    pub scope: HashSet<ChannelId>,
    /// What to do.
    pub job: LlmJob,
}

/// A cited source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cited {
    /// `m:12` or `D-3`.
    pub id: CitationId,
    /// Its jump link, if it has one.
    pub link: Option<DiscordMessageLink>,
}

/// Why the backend gave no answer. Each is shown as fixed text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The member's hourly question limit is used up.
    QuestionsPerHour,
    /// The member's hourly token limit is used up.
    TokensPerHour,
    /// The member's daily token limit is used up.
    TokensPerDay,
    /// The question is empty or too long.
    InvalidQuestion,
    /// The provider failed or timed out.
    Provider,
    /// Anything else on the host (storage). Details are in the host's log.
    Host,
}

/// What the backend returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmOutcome {
    /// A validated answer: every citation is a source the model was given, every URL is
    /// removed. Still untrusted text: it is escaped before it is shown.
    Answer {
        /// The text.
        text: String,
        /// The cited sources, in order of first use.
        citations: Vec<Cited>,
    },
    /// Nothing supported an answer; the best search hits instead.
    NotFound {
        /// Up to a few sources to look at.
        top_hits: Vec<Cited>,
    },
    /// No answer, for a reason shown as fixed text.
    Refused(Refusal),
}

/// The future an [`AskBackend`] returns.
pub type AskFuture<'a> = Pin<Box<dyn Future<Output = LlmOutcome> + Send + 'a>>;

/// The `/ask` pipeline as the host provides it.
pub trait AskBackend: Send + Sync + 'static {
    /// Answers one request. Never fails: every problem is a [`Refusal`].
    fn ask(&self, request: LlmRequest) -> AskFuture<'_>;
}

/// What the handler hands to the service for the model step.
#[derive(Debug, Clone, PartialEq)]
pub struct LlmTask {
    /// The request.
    pub request: LlmRequest,
    /// Reply publicly (`public:true`).
    pub public: bool,
    /// The catch-up reply the summary is added to; `None` for `/ask`.
    pub base: Option<Reply>,
    /// A fixed access note for the last embed, if any.
    pub footer: Option<String>,
}

/// Fixed texts.
pub(crate) const ASK_DISABLED: &str = "/ask is not enabled on this host: it needs a language model, which the host has not configured.";
pub(crate) const SUMMARY_DISABLED: &str =
    "Summaries need a language model, which this host has not configured.";
const ANSWER_HEAD: &str =
    "Answer generated by a language model from the team's messages. Check the sources:";
pub(crate) const RETRY: &str = "teamctx could not answer in time; please try again.";
const NOT_FOUND: &str = "I couldn't find this in the team's messages.";
const NOTHING_TO_SUMMARISE: &str = "Nothing to summarise.";
const NO_HITS: &str = "No messages matched.";

/// Longest answer shown, in UTF-16 units (Discord's content limit is 2,000).
const ANSWER_MAX: usize = 1_800;

impl Refusal {
    /// The fixed text shown for it.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Self::QuestionsPerHour => {
                "You have reached the hourly limit for questions. Please try again later."
            }
            Self::TokensPerHour => {
                "You have reached the hourly limit for model usage. Please try again later."
            }
            Self::TokensPerDay => {
                "You have reached the daily limit for model usage. Please try again tomorrow."
            }
            Self::InvalidQuestion => "Please ask a question of at most 500 characters.",
            Self::Provider => {
                "The language model could not answer right now. Please try again later."
            }
            Self::Host => "Something went wrong on the teamctx host. The host's log has details.",
        }
    }

    /// A short fixed name for logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::QuestionsPerHour => "questions-per-hour",
            Self::TokensPerHour => "tokens-per-hour",
            Self::TokensPerDay => "tokens-per-day",
            Self::InvalidQuestion => "invalid-question",
            Self::Provider => "provider",
            Self::Host => "host",
        }
    }
}

/// Model text for Discord: each line through [`render::inline`], so nothing in it can
/// format, link, mention or hide; blank lines kept as paragraph breaks; clipped to `max`.
fn answer_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut blank = false;
    for line in text.lines() {
        let used = render::discord_len(&out);
        // Room for up to two separator newlines and at least one character.
        if used + 3 >= max {
            break;
        }
        let escaped = render::inline(line, max - used - 2);
        if escaped.is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
            if blank {
                out.push('\n');
            }
        }
        blank = false;
        out.push_str(&escaped);
    }
    out
}

/// One source line: the escaped id and a jump link built from numbers.
fn source_line(cited: &Cited) -> String {
    let id = render::inline(&cited.id.to_string(), 40);
    match cited.link {
        Some(link) => format!("**{id}** \u{b7} {}", render::jump(link)),
        None => format!("**{id}**"),
    }
}

fn sources(cited: &[Cited]) -> String {
    let lines: Vec<String> = cited.iter().take(MAX_ITEMS).map(source_line).collect();
    let more = cited.len().saturating_sub(MAX_ITEMS);
    render::clip_lines(&lines, MAX_DESCRIPTION, more)
}

/// The reply for a finished task.
#[must_use]
pub fn render(task: LlmTask, outcome: &LlmOutcome) -> Reply {
    match task.base {
        None => render_answer(task.public, task.footer, outcome),
        Some(base) => render_summary(base, task.footer, outcome),
    }
}

fn render_answer(public: bool, footer: Option<String>, outcome: &LlmOutcome) -> Reply {
    let (content, embeds) = match outcome {
        LlmOutcome::Answer { text, citations } => (
            format!("{ANSWER_HEAD}\n{}", answer_text(text, ANSWER_MAX)),
            vec![render::embed("Sources", &sources(citations), footer)],
        ),
        LlmOutcome::NotFound { top_hits } => {
            let hits = if top_hits.is_empty() {
                NO_HITS.to_owned()
            } else {
                sources(top_hits)
            };
            (
                NOT_FOUND.to_owned(),
                vec![render::embed("Closest search results", &hits, footer)],
            )
        }
        LlmOutcome::Refused(refusal) => (refusal.text().to_owned(), Vec::new()),
    };
    Reply {
        content,
        embeds,
        public,
    }
}

fn render_summary(mut base: Reply, footer: Option<String>, outcome: &LlmOutcome) -> Reply {
    let description = match outcome {
        LlmOutcome::Answer { text, citations } => {
            format!("{}\n\n{}", answer_text(text, 2_000), sources(citations))
        }
        LlmOutcome::NotFound { .. } => NOTHING_TO_SUMMARISE.to_owned(),
        LlmOutcome::Refused(refusal) => refusal.text().to_owned(),
    };
    // The access note moves to the summary, the last embed.
    if let Some(last) = base.embeds.last_mut() {
        last.footer = None;
    }
    base.embeds.push(render::embed(
        "Summary (generated by a language model; check the sources)",
        &description,
        footer,
    ));
    base
}

#[cfg(test)]
mod tests {
    use teamctx_core::id::Snowflake;

    use super::*;

    fn sf(raw: u64) -> Snowflake {
        Snowflake::new(raw).unwrap()
    }

    fn task(base: Option<Reply>) -> LlmTask {
        LlmTask {
            request: LlmRequest {
                member: MemberId::new(1).unwrap(),
                now: UnixMillis::from_millis(1),
                scope: HashSet::new(),
                job: LlmJob::Ask {
                    question: "q".to_owned(),
                },
            },
            public: false,
            base,
            footer: None,
        }
    }

    #[test]
    fn model_text_is_escaped_line_by_line() {
        let text = "We ship **Friday** @everyone <@1100000000000000201> [x](https://evil.example)\n\n# Heading\n> quote\n\u{202e}hidden [m:12]";
        let out = answer_text(text, 1_800);
        assert!(!out.contains("**Friday**"));
        assert!(!out.contains("@everyone"));
        assert!(
            out.contains("\\<@\u{200b}1100"),
            "mention markup is escaped: {out:?}"
        );
        assert!(!out.contains("](http"));
        assert!(!out.contains('\u{202e}'));
        assert!(out.contains("\\# Heading"));
        assert!(out.contains("\n\n"), "paragraphs survive: {out:?}");
        let long = "x".repeat(5_000);
        assert!(render::discord_len(&answer_text(&long, 1_800)) <= 1_800);
    }

    #[test]
    fn answers_list_sources_with_numeric_jump_links() {
        let link = DiscordMessageLink {
            guild: sf(1),
            channel: sf(2),
            message: sf(3),
        };
        let outcome = LlmOutcome::Answer {
            text: "Friday [D-3]".to_owned(),
            citations: vec![
                Cited {
                    id: "D-3".parse().unwrap(),
                    link: Some(link),
                },
                Cited {
                    id: "m:9".parse().unwrap(),
                    link: None,
                },
            ],
        };
        let reply = render(task(None), &outcome);
        assert!(!reply.public);
        assert!(reply.content.starts_with(ANSWER_HEAD));
        let sources = reply.embeds[0].description.as_deref().unwrap();
        assert!(sources.contains("[jump](https://discord.com/channels/1/2/3)"));
        assert!(sources.contains("D\\-3"));
    }

    #[test]
    fn refusals_and_not_found_are_fixed_text() {
        for refusal in [
            Refusal::QuestionsPerHour,
            Refusal::TokensPerHour,
            Refusal::TokensPerDay,
            Refusal::InvalidQuestion,
            Refusal::Provider,
            Refusal::Host,
        ] {
            let reply = render(task(None), &LlmOutcome::Refused(refusal));
            assert_eq!(reply.content, refusal.text());
            assert!(reply.embeds.is_empty());
        }
        let reply = render(task(None), &LlmOutcome::NotFound { top_hits: vec![] });
        assert_eq!(reply.content, NOT_FOUND);
    }

    #[test]
    fn a_summary_is_added_to_the_catchup() {
        let base = Reply {
            content: "Since".to_owned(),
            embeds: vec![render::embed("Mentions", "x", Some("note".to_owned()))],
            public: true,
        };
        let reply = render(
            task(Some(base.clone())),
            &LlmOutcome::Refused(Refusal::Provider),
        );
        assert!(reply.public);
        assert_eq!(reply.content, base.content);
        assert_eq!(reply.embeds.len(), 2);
        assert!(reply.embeds[0].footer.is_none());
        assert_eq!(
            reply.embeds[1].description.as_deref(),
            Some(Refusal::Provider.text())
        );
    }
}
