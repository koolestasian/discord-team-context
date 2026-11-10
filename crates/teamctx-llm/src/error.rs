//! Error types. None of them carries a provider response body, an API key, a URL or any
//! chat text: their `Display` output is safe to show in Discord.

use thiserror::Error;

use teamctx_core::repo::RepoError;

use crate::budget::{BudgetLimit, LedgerError};

/// A failure talking to a language model provider.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LlmError {
    /// The environment variable named in `llm.api_key_env` is unset or empty. The name is
    /// deliberately not part of the error: a key pasted there by mistake would otherwise
    /// reach logs and terminals, and no check can tell a key from a variable name.
    #[error("the environment variable named in llm.api_key_env is not set or is empty")]
    MissingApiKey,
    /// The adapter's configuration is unusable.
    #[error("invalid language model configuration: {0}")]
    InvalidConfig(&'static str),
    /// The provider did not answer within the timeout.
    #[error("the language model provider timed out")]
    Timeout,
    /// The provider could not be reached.
    #[error("could not connect to the language model provider")]
    Connect,
    /// Any other transport failure.
    #[error("the language model request failed")]
    Transport,
    /// The provider answered with a non-success status. The body is discarded.
    #[error("the language model provider returned HTTP {status}")]
    Http {
        /// The HTTP status code.
        status: u16,
    },
    /// The provider's answer was not the expected JSON shape.
    #[error("the language model provider sent a malformed response: {0}")]
    MalformedResponse(&'static str),
    /// The provider's answer exceeded the size limit.
    #[error("the language model provider sent an oversized response")]
    ResponseTooLarge,
}

/// Why [`AskPipeline`](crate::AskPipeline) could not produce an answer.
#[derive(Debug, Error)]
pub enum AskError {
    /// The question is empty or too long.
    #[error("invalid question: {0}")]
    InvalidQuestion(&'static str),
    /// The member used up a budget.
    #[error("{0}")]
    BudgetExceeded(BudgetLimit),
    /// Reading the record failed.
    #[error("could not read the team record")]
    Retrieval(#[source] RepoError),
    /// Reading or writing the usage ledger failed.
    #[error("could not read or update the usage ledger")]
    Ledger(#[source] LedgerError),
    /// The provider failed.
    #[error("the language model could not answer: {0}")]
    Provider(#[source] LlmError),
}

impl From<RepoError> for AskError {
    fn from(error: RepoError) -> Self {
        Self::Retrieval(error)
    }
}

impl From<LedgerError> for AskError {
    fn from(error: LedgerError) -> Self {
        Self::Ledger(error)
    }
}

impl From<LlmError> for AskError {
    fn from(error: LlmError) -> Self {
        Self::Provider(error)
    }
}
