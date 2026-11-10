//! Host wiring for the existing single-call pipeline. Both transports share one
//! provider and persistent usage ledger. No network calls are made during setup.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use teamctx_discord::connector::Clock;
use teamctx_discord::interactions::ask::{
    self as discord, Cited, LlmJob, LlmOutcome, LlmRequest, Refusal,
};
use teamctx_llm::budget::{Asker, Budget, BudgetLimit, StoreLedger};
use teamctx_llm::bundle::{Items, Scope};
use teamctx_llm::provider::{Completion, CompletionRequest, LlmProvider};
use teamctx_llm::{
    Answer, AnthropicConfig, AnthropicMessages, ApiKey, AskConfig, AskError, AskPipeline,
    AskRequest, LlmError, OpenAiCompatible, OpenAiConfig, without_urls,
};
use teamctx_server::ask as api;
use teamctx_store::Store;
use tokio::sync::Semaphore;

use crate::config::{ConfigError, Llm};
use crate::run::{RunError, midnight_in};

/// The two configured adapters; tests inject the deterministic provider instead.
#[derive(Debug)]
pub(crate) enum Provider {
    Anthropic(AnthropicMessages),
    OpenAi(OpenAiCompatible),
    #[cfg(test)]
    Fake(teamctx_llm::FakeProvider),
}

impl LlmProvider for Provider {
    fn provider_name(&self) -> &str {
        match self {
            Self::Anthropic(p) => p.provider_name(),
            Self::OpenAi(p) => p.provider_name(),
            #[cfg(test)]
            Self::Fake(p) => p.provider_name(),
        }
    }
    fn model(&self) -> &str {
        match self {
            Self::Anthropic(p) => p.model(),
            Self::OpenAi(p) => p.model(),
            #[cfg(test)]
            Self::Fake(p) => p.model(),
        }
    }
    async fn complete(&self, request: CompletionRequest) -> Result<Completion, LlmError> {
        match self {
            Self::Anthropic(p) => p.complete(request).await,
            Self::OpenAi(p) => p.complete(request).await,
            #[cfg(test)]
            Self::Fake(p) => p.complete(request).await,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Setup {
    pub(crate) provider: Provider,
    pub(crate) config: AskConfig,
}

fn invalid(field: &'static str, reason: &'static str) -> ConfigError {
    ConfigError {
        field,
        reason,
        line: None,
    }
}

/// Requires credentials for hosted providers; a loopback OpenAI-compatible server
/// may be keyless. A configured but malformed key is always refused.
pub(crate) fn configure(
    config: Option<&Llm>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Option<Setup>, ConfigError> {
    let Some(config) = config else {
        return Ok(None);
    };
    let url = reqwest::Url::parse(&config.base_url)
        .map_err(|_| invalid("llm.base_url", "must be an absolute HTTP(S) URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "llm.base_url",
            "must use HTTP(S), with a host and no credentials, query or fragment",
        ));
    }
    let loopback = url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    let key = match lookup(&config.api_key_env) {
        Some(raw) => Some(
            ApiKey::new(raw)
                .map_err(|_| invalid("llm.api_key_env", "must contain a usable API key"))?,
        ),
        None if config.provider == "openai_compatible" && loopback => None,
        None => {
            return Err(invalid(
                "llm.api_key_env",
                "the named environment variable is not set or is empty",
            ));
        }
    };
    if key.is_some() && url.scheme() == "http" && !loopback {
        return Err(invalid(
            "llm.base_url",
            "use HTTPS when sending an API key outside loopback",
        ));
    }
    let provider = match config.provider.as_str() {
        "anthropic" => Provider::Anthropic(
            AnthropicMessages::new(AnthropicConfig::new(
                &config.base_url,
                &config.model,
                key.ok_or_else(|| {
                    invalid(
                        "llm.api_key_env",
                        "the named environment variable is not set or is empty",
                    )
                })?,
            ))
            .map_err(|_| invalid("llm", "cannot initialise the Anthropic adapter"))?,
        ),
        "openai_compatible" => Provider::OpenAi(
            OpenAiCompatible::new(OpenAiConfig::new(&config.base_url, &config.model, key))
                .map_err(|_| invalid("llm", "cannot initialise the OpenAI-compatible adapter"))?,
        ),
        _ => {
            return Err(invalid(
                "llm.provider",
                "must be anthropic or openai_compatible",
            ));
        }
    };
    Ok(Some(Setup {
        provider,
        config: AskConfig {
            budget: Budget {
                max_questions_per_hour: config.max_questions_per_member_per_hour,
                // Only the two limits in the public config apply. There is no separate
                // hourly token limit in plan section 12.
                max_tokens_per_hour: u64::MAX,
                max_tokens_per_day: config.max_tokens_per_day,
            },
            ..AskConfig::default()
        },
    }))
}

#[derive(Debug)]
pub(crate) struct Backend {
    db: PathBuf,
    pipeline: AskPipeline<Provider>,
    ledger: StoreLedger,
    zone: teamctx_core::when::FixedOffset,
    slots: Arc<Semaphore>,
}

impl Setup {
    pub(crate) fn open(
        self,
        db: &Path,
        zone: teamctx_core::when::FixedOffset,
    ) -> Result<Arc<Backend>, RunError> {
        Ok(Arc::new(Backend {
            db: db.to_owned(),
            pipeline: AskPipeline::new(self.provider, self.config),
            ledger: StoreLedger::open(db).map_err(|_| RunError::Store)?,
            zone,
            slots: Arc::new(Semaphore::new(16)),
        }))
    }
}

enum Job {
    Discord(LlmRequest),
    Api {
        token: teamctx_core::id::TokenId,
        question: String,
        now: teamctx_core::time::UnixMillis,
    },
}

impl Backend {
    async fn answer(self: &Arc<Self>, job: Job) -> Result<Answer, Refusal> {
        let permit = Arc::clone(&self.slots)
            .try_acquire_owned()
            .map_err(|_| Refusal::Host)?;
        let this = Arc::clone(self);
        let runtime = tokio::runtime::Handle::current();
        // ponytail: at most 16 blocking workers wait on model I/O. Split ledger calls
        // into async operations if sustained throughput warrants a different design.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut store = Store::open_read_only(&this.db).map_err(|_| Refusal::Host)?;
            store.set_date_resolver(midnight_in(this.zone));
            let prepared = match &job {
                Job::Discord(request) => {
                    let asker = Asker::Member(request.member);
                    let scope = Scope::Channels(&request.scope);
                    match &request.job {
                        LlmJob::Ask { question } => this.pipeline.prepare_in(
                            &store,
                            &this.ledger,
                            &AskRequest {
                                asker,
                                question,
                                now: request.now,
                            },
                            scope,
                        ),
                        LlmJob::Summarize { entries, messages } => this.pipeline.prepare_summary(
                            &store,
                            &this.ledger,
                            asker,
                            request.now,
                            &Items {
                                entries: entries.clone(),
                                messages: messages.clone(),
                            },
                            scope,
                        ),
                    }
                }
                Job::Api {
                    token,
                    question,
                    now,
                } => this.pipeline.prepare(
                    &store,
                    &this.ledger,
                    &AskRequest {
                        asker: Asker::Token(*token),
                        question,
                        now: *now,
                    },
                ),
            }
            .map_err(|error| refusal(&error))?;
            drop(store);
            runtime.block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(60),
                    this.pipeline.run(&this.ledger, prepared),
                )
                .await
                .map_err(|_| Refusal::Provider)?
                .map(|result| result.answer)
                .map_err(|error| refusal(&error))
            })
        })
        .await
        .map_err(|_| Refusal::Host)?
    }
}

fn refusal(error: &AskError) -> Refusal {
    let refusal = match error {
        AskError::InvalidQuestion(_) => Refusal::InvalidQuestion,
        AskError::BudgetExceeded(BudgetLimit::QuestionsPerHour) => Refusal::QuestionsPerHour,
        AskError::BudgetExceeded(BudgetLimit::TokensPerHour) => Refusal::TokensPerHour,
        AskError::BudgetExceeded(BudgetLimit::TokensPerDay) => Refusal::TokensPerDay,
        AskError::Provider(_) => Refusal::Provider,
        AskError::Ledger(_) | AskError::Retrieval(_) => Refusal::Host,
    };
    tracing::warn!(reason = refusal.name(), "model request refused");
    refusal
}

/// Each transport holds a cheap clone of the same backend.
pub(crate) struct Service {
    pub(crate) backend: Arc<Backend>,
    pub(crate) clock: Clock,
}

impl discord::AskBackend for Service {
    fn ask(&self, request: LlmRequest) -> discord::AskFuture<'_> {
        Box::pin(async move {
            match self.backend.answer(Job::Discord(request)).await {
                Ok(Answer::Found(answer)) => LlmOutcome::Answer {
                    text: without_urls(&answer.text).0,
                    citations: answer
                        .citations
                        .iter()
                        .map(|c| Cited {
                            id: c.id,
                            link: c.link,
                        })
                        .collect(),
                },
                Ok(Answer::NotFound(answer)) => LlmOutcome::NotFound {
                    top_hits: answer
                        .top_hits
                        .iter()
                        .map(|c| Cited {
                            id: c.id,
                            link: c.link,
                        })
                        .collect(),
                },
                Err(error) => LlmOutcome::Refused(error),
            }
        })
    }
}

impl api::AskBackend for Service {
    fn ask(&self, token: teamctx_core::id::TokenId, question: String) -> api::AskFuture<'_> {
        Box::pin(async move {
            let answer = self
                .backend
                .answer(Job::Api {
                    token,
                    question,
                    now: (self.clock)(),
                })
                .await
                .map_err(|r| match r {
                    Refusal::InvalidQuestion => api::AskFailure::InvalidQuestion,
                    Refusal::QuestionsPerHour | Refusal::TokensPerHour | Refusal::TokensPerDay => {
                        api::AskFailure::Budget
                    }
                    Refusal::Provider => api::AskFailure::Provider,
                    Refusal::Host => api::AskFailure::Host,
                })?;
            let citations: Vec<_> = answer
                .citations()
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.id.to_string(), "url": c.link.map(|link| link.to_string()),
                    })
                })
                .collect();
            Ok(api::answer(
                &without_urls(answer.text()).0,
                matches!(answer, Answer::Found(_)),
                &citations,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn setup_accepts_both_adapters_and_keyless_loopback_without_exposing_keys() {
        assert!(configure(None, |_| None).unwrap().is_none());
        let mut config = Llm {
            provider: "openai_compatible".to_owned(),
            base_url: "http://127.0.0.1:1234/v1".to_owned(),
            model: "fictional-model".to_owned(),
            ..Llm::default()
        };
        assert!(configure(Some(&config), |_| None).unwrap().is_some());
        config.base_url = "http://[::1]:11434/v1".to_owned();
        assert!(configure(Some(&config), |_| None).unwrap().is_some());
        assert!(configure(Some(&config), |_| Some(String::new())).is_err());
        config.provider = "anthropic".to_owned();
        config.base_url = "https://api.example".to_owned();
        let key = "FAKE-api-key-not-a-credential";
        let setup = configure(Some(&config), |_| Some(key.to_owned()))
            .unwrap()
            .unwrap();
        assert!(!format!("{setup:?}").contains(key));
        assert_eq!(
            setup.config.budget.max_questions_per_hour,
            config.max_questions_per_member_per_hour
        );
        assert_eq!(
            setup.config.budget.max_tokens_per_day,
            config.max_tokens_per_day
        );
        assert_eq!(setup.config.budget.max_tokens_per_hour, u64::MAX);
    }
}
