//! The `/ask` pipeline (plan section 11): retrieve, prompt, one call without tools,
//! validate, render.
//!
//! Two steps, because the store traits are synchronous and a SQLite connection cannot be
//! held across an `await`:
//!
//! 1. [`AskPipeline::prepare`] (sync): validates the question, checks the member's
//!    budget, searches the store and assembles the framed prompt.
//! 2. [`AskPipeline::run`] (async): reserves the budget, calls the provider, settles the
//!    reservation with the real usage, validates the answer and renders it.
//!
//! Budget: a call may use its prompt plus up to [`AskConfig::max_output_tokens`], so
//! both steps check that sum, not the prompt alone. `prepare` only checks, to refuse
//! early; `run` reserves atomically right before the provider call, so questions
//! prepared at the same time cannot all spend the same remaining budget, and a `run`
//! future dropped mid-call is still charged in full (see [`crate::budget`]).
//!
//! Discord rendering is the caller's job: turn each [`Citation`] with a link into a jump
//! link, and send the text with mentions disabled ([`without_urls`] removes every link a
//! caller does not want shown at all).
//!
//! [`AskPipeline::prepare_summary`] is the same pipeline for `/catchup summarize:true`:
//! the sources are the items the catch-up shows instead of search hits, the system prompt
//! asks for a summary, and the answer is validated the same way.

use teamctx_core::citation::{CitationId, DiscordMessageLink};
use teamctx_core::time::UnixMillis;

use crate::budget::{self, Asker, Budget, Reservation, UsageEntry, UsageLedger};
use crate::bundle::{Bundle, BundleLimits, Items, Retrieval, Scope};
use crate::error::AskError;
use crate::prompt::{self, NOT_FOUND_TOKEN, SUMMARY_SYSTEM_PROMPT, SYSTEM_PROMPT};
use crate::provider::{CompletionRequest, LlmProvider, TokenUsage, estimate_tokens};
use crate::validate;

/// What the member sees when nothing in the record supports an answer.
pub const NOT_FOUND_TEXT: &str =
    "Not found in the record. I couldn't find this in the team's messages.";

/// Settings for the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AskConfig {
    /// Bounds on the retrieval bundle.
    pub bundle: BundleLimits,
    /// Per-member limits.
    pub budget: Budget,
    /// Longest question in characters, after cleaning.
    pub max_question_chars: usize,
    /// Most tokens the model may generate. The token budget must have room for this
    /// many on top of the prompt before a question is sent.
    pub max_output_tokens: u32,
    /// How many top search hits a "not found" answer lists.
    pub top_hits: usize,
}

impl Default for AskConfig {
    fn default() -> Self {
        Self {
            bundle: BundleLimits::default(),
            budget: Budget::default(),
            max_question_chars: 500,
            max_output_tokens: 800,
            top_hits: 3,
        }
    }
}

/// One question from one member or access token.
#[derive(Debug, Clone, Copy)]
pub struct AskRequest<'a> {
    /// Who asked; budgets are per asker.
    pub asker: Asker,
    /// The question, as typed. Untrusted.
    pub question: &'a str,
    /// The current time, passed in so tests control it.
    pub now: UnixMillis,
}

/// A cited source with its jump link, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Citation {
    /// The id, formatted with [`CitationId::marker`] in the answer text.
    pub id: CitationId,
    /// The Discord message to link to.
    pub link: Option<DiscordMessageLink>,
}

/// An answer supported by at least one valid citation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundAnswer {
    /// The validated text. It ends with a `Sources:` line of citation markers.
    pub text: String,
    /// The valid citations, in order of first use.
    pub citations: Vec<Citation>,
    /// Markers naming an id that was not in the bundle, removed from the text.
    pub dropped_citations: usize,
    /// URLs that were not in the sources, replaced in the text.
    pub removed_urls: usize,
}

/// Why there is no answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFoundReason {
    /// Search found nothing, so the model was not called.
    NoSources,
    /// The model said it could not answer.
    ModelDeclined,
    /// The model's answer cited nothing valid, so it is treated as a guess.
    NoValidCitation,
}

/// No supported answer: the search hits are shown instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotFoundAnswer {
    /// Why.
    pub reason: NotFoundReason,
    /// The best search hits, so the member can look for themselves.
    pub top_hits: Vec<Citation>,
}

/// The result of a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// A validated answer.
    Found(FoundAnswer),
    /// Nothing supported an answer.
    NotFound(NotFoundAnswer),
}

impl Answer {
    /// The text to show. Never contains provider text that failed validation.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Found(found) => &found.text,
            Self::NotFound(_) => NOT_FOUND_TEXT,
        }
    }

    /// The citations to link: the answer's, or the top hits of a "not found".
    #[must_use]
    pub fn citations(&self) -> &[Citation] {
        match self {
            Self::Found(found) => &found.citations,
            Self::NotFound(not_found) => &not_found.top_hits,
        }
    }
}

/// An answer and what it cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskResult {
    /// The validated answer.
    pub answer: Answer,
    /// Tokens used; zero when the model was not called.
    pub usage: TokenUsage,
}

/// A question that passed validation and the budget check, with its prompt built.
pub struct Prepared {
    asker: Asker,
    now: UnixMillis,
    bundle: Bundle,
    request: CompletionRequest,
    /// Estimated prompt tokens, reserved along with the output allowance.
    prompt_tokens: u64,
}

impl std::fmt::Debug for Prepared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Counts only: the prompt holds chat text.
        f.debug_struct("Prepared")
            .field("asker", &self.asker)
            .field("sources", &self.bundle.sources().len())
            .finish_non_exhaustive()
    }
}

impl Prepared {
    /// The retrieval bundle, for inspection and tests.
    #[must_use]
    pub const fn bundle(&self) -> &Bundle {
        &self.bundle
    }

    /// The request that [`AskPipeline::run`] will send, if the bundle is not empty.
    #[must_use]
    pub const fn request(&self) -> &CompletionRequest {
        &self.request
    }
}

/// The `/ask` pipeline over one provider.
#[derive(Debug)]
pub struct AskPipeline<P> {
    provider: P,
    config: AskConfig,
}

impl<P: LlmProvider> AskPipeline<P> {
    /// A pipeline that calls `provider` under `config`.
    #[must_use]
    pub const fn new(provider: P, config: AskConfig) -> Self {
        Self { provider, config }
    }

    /// The provider, for tests that inspect a fake.
    #[must_use]
    pub const fn provider(&self) -> &P {
        &self.provider
    }

    /// Validates the question, enforces the budget, retrieves the sources and builds
    /// the prompt. Nothing is sent to the provider yet.
    ///
    /// # Errors
    ///
    /// [`AskError::InvalidQuestion`], [`AskError::BudgetExceeded`],
    /// [`AskError::Retrieval`] or [`AskError::Ledger`].
    pub fn prepare<R, L>(
        &self,
        reader: &R,
        ledger: &L,
        request: &AskRequest<'_>,
    ) -> Result<Prepared, AskError>
    where
        R: Retrieval + ?Sized,
        L: UsageLedger + ?Sized,
    {
        self.prepare_in(reader, ledger, request, Scope::All)
    }

    /// [`AskPipeline::prepare`] with sources limited to `scope`, such as the channels
    /// the asking Discord member can read.
    ///
    /// # Errors
    ///
    /// The same as [`AskPipeline::prepare`].
    pub fn prepare_in<R, L>(
        &self,
        reader: &R,
        ledger: &L,
        request: &AskRequest<'_>,
        scope: Scope<'_>,
    ) -> Result<Prepared, AskError>
    where
        R: Retrieval + ?Sized,
        L: UsageLedger + ?Sized,
    {
        let question = prompt::clean_question(request.question, self.config.max_question_chars)?;
        // Refuse early, before any search, when the asker has no budget left at all.
        budget::check(ledger, &self.config.budget, request.asker, request.now, 0)?;
        let bundle =
            Bundle::retrieve_in(reader, &question, request.now, &self.config.bundle, scope)?;
        let user = if bundle.is_empty() {
            String::new()
        } else {
            prompt::user_message(&bundle, &question)
        };
        self.finish(
            ledger,
            request.asker,
            request.now,
            bundle,
            SYSTEM_PROMPT,
            user,
        )
    }

    /// Prepares a summary of `items` (the entries and messages a `/catchup` shows) for
    /// `asker`, limited to `scope`. Budgets, the provider call and validation are the
    /// same as for a question; run it with [`AskPipeline::run`].
    ///
    /// # Errors
    ///
    /// [`AskError::BudgetExceeded`], [`AskError::Retrieval`] or [`AskError::Ledger`].
    pub fn prepare_summary<R, L>(
        &self,
        reader: &R,
        ledger: &L,
        asker: Asker,
        now: UnixMillis,
        items: &Items,
        scope: Scope<'_>,
    ) -> Result<Prepared, AskError>
    where
        R: Retrieval + ?Sized,
        L: UsageLedger + ?Sized,
    {
        budget::check(ledger, &self.config.budget, asker, now, 0)?;
        let bundle = Bundle::of_items(reader, items, &self.config.bundle, scope)?;
        let user = if bundle.is_empty() {
            String::new()
        } else {
            prompt::summary_message(&bundle)
        };
        self.finish(ledger, asker, now, bundle, SUMMARY_SYSTEM_PROMPT, user)
    }

    /// Checks that the prompt plus the output allowance fits the budget and packs the
    /// request. An empty bundle costs nothing and is never sent.
    fn finish<L: UsageLedger + ?Sized>(
        &self,
        ledger: &L,
        asker: Asker,
        now: UnixMillis,
        bundle: Bundle,
        system: &str,
        user: String,
    ) -> Result<Prepared, AskError> {
        let prompt_tokens = if bundle.is_empty() {
            0
        } else {
            let prompt_tokens = estimate_tokens(system).saturating_add(estimate_tokens(&user));
            budget::check(
                ledger,
                &self.config.budget,
                asker,
                now,
                prompt_tokens.saturating_add(u64::from(self.config.max_output_tokens)),
            )?;
            prompt_tokens
        };
        Ok(Prepared {
            asker,
            now,
            bundle,
            request: CompletionRequest {
                system: system.to_owned(),
                user,
                max_output_tokens: self.config.max_output_tokens,
            },
            prompt_tokens,
        })
    }

    /// Reserves the budget, calls the provider once (no tools), settles the reservation
    /// with the real usage, and validates the answer.
    ///
    /// With an empty bundle the provider is not called and nothing is recorded.
    ///
    /// The reservation is taken atomically before the call, so of several questions
    /// prepared at once only those that still fit reach the provider. If this future is
    /// dropped during the call, the question is recorded as a failed call charged with
    /// its full reservation (prompt estimate plus [`AskConfig::max_output_tokens`]).
    ///
    /// # Errors
    ///
    /// [`AskError::BudgetExceeded`] if the reservation does not fit any more (the
    /// provider is not called), [`AskError::Provider`] if the call fails (the failed call
    /// is still recorded, so it counts towards the hourly question limit),
    /// [`AskError::Ledger`] if the usage cannot be reserved or recorded.
    pub async fn run<L>(&self, ledger: &L, prepared: Prepared) -> Result<AskResult, AskError>
    where
        L: UsageLedger + Sync + ?Sized,
    {
        let Prepared {
            asker,
            now,
            bundle,
            request,
            prompt_tokens,
        } = prepared;
        if bundle.is_empty() {
            return Ok(AskResult {
                answer: Answer::NotFound(NotFoundAnswer {
                    reason: NotFoundReason::NoSources,
                    top_hits: Vec::new(),
                }),
                usage: TokenUsage::default(),
            });
        }
        let row = |input_tokens, output_tokens, ok| UsageEntry {
            at: now,
            asker,
            provider: self.provider.provider_name().to_owned(),
            model: self.provider.model().to_owned(),
            input_tokens,
            output_tokens,
            ok,
        };
        let reservation = Reservation::take(
            ledger,
            &self.config.budget,
            row(prompt_tokens, u64::from(request.max_output_tokens), false),
        )?;
        let outcome = self.provider.complete(request).await;
        let usage = outcome
            .as_ref()
            .map_or_else(|_| TokenUsage::default(), |c| c.usage);
        let recorded = reservation.settle(&row(usage.input, usage.output, outcome.is_ok()));
        let completion = outcome?;
        recorded?;
        Ok(AskResult {
            answer: self.validate(&bundle, &completion.text),
            usage,
        })
    }

    /// Turns model text into an [`Answer`]: citations checked against the bundle, URLs
    /// checked against the sources, and no valid citation means "not found".
    fn validate(&self, bundle: &Bundle, text: &str) -> Answer {
        let not_found = |reason| {
            Answer::NotFound(NotFoundAnswer {
                reason,
                top_hits: bundle
                    .sources()
                    .iter()
                    .take(self.config.top_hits)
                    .map(|s| Citation {
                        id: s.id(),
                        link: s.link(),
                    })
                    .collect(),
            })
        };
        let checked = validate::check_citations(text, &bundle.ids());
        // The text with every marker removed: is anything left to say?
        let bare = validate::check_citations(&checked.text, &std::collections::HashSet::new());
        let bare = bare
            .text
            .trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation() && c != '_');
        if bare.is_empty() || bare.eq_ignore_ascii_case(NOT_FOUND_TOKEN) {
            return not_found(NotFoundReason::ModelDeclined);
        }
        if checked.cited.is_empty() {
            return not_found(NotFoundReason::NoValidCitation);
        }
        let known_urls = validate::urls_in(bundle.sources().iter().map(|s| s.text()));
        let (body, removed_urls) = validate::strip_unknown_urls(&checked.text, &known_urls);
        let markers: Vec<String> = checked.cited.iter().map(|id| id.marker()).collect();
        let citations = checked
            .cited
            .iter()
            .map(|&id| Citation {
                id,
                link: bundle
                    .sources()
                    .iter()
                    .find(|s| s.id() == id)
                    .and_then(|s| s.link()),
            })
            .collect();
        Answer::Found(FoundAnswer {
            text: format!("{}\n\nSources: {}", body.trim(), markers.join(" ")),
            citations,
            dropped_citations: checked.dropped,
            removed_urls,
        })
    }
}

/// `text` with every link and URL replaced by a placeholder, and how many there were.
/// For callers that show an answer where no link should be clickable at all (Discord
/// answers link their sources through the citations instead).
#[must_use]
pub fn without_urls(text: &str) -> (String, usize) {
    validate::strip_unknown_urls(text, &std::collections::HashSet::new())
}
