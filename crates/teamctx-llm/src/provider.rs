//! The provider boundary (plan section 11, step 7).
//!
//! A request has no tool, function or search fields on purpose: `/ask` is a single call
//! without tool use, which keeps the damage of a prompt injection in chat small.

use std::future::Future;

use crate::error::LlmError;

/// One completion call: a fixed system prompt and one user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionRequest {
    /// The rules for the model. Written by this crate, never by chat participants.
    pub system: String,
    /// The user message: framed sources and the question.
    pub user: String,
    /// Most tokens the model may generate.
    pub max_output_tokens: u32,
}

/// Token counts for one call, for `llm_usage` accounting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Tokens in the prompt.
    pub input: u64,
    /// Tokens generated.
    pub output: u64,
    /// `true` when the provider sent no `usage` and the counts are an estimate
    /// (about four characters per token).
    pub estimated: bool,
}

impl TokenUsage {
    /// Input plus output, saturating.
    #[must_use]
    pub const fn total(self) -> u64 {
        self.input.saturating_add(self.output)
    }
}

/// What a provider returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// The model's text. Untrusted: it is validated before anyone sees it.
    pub text: String,
    /// Token accounting.
    pub usage: TokenUsage,
}

/// Rough token count for text: four characters per token, rounded up.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count().div_ceil(4)).unwrap_or(u64::MAX)
}

/// A language model provider.
///
/// Implementations: [`AnthropicMessages`](crate::AnthropicMessages),
/// [`OpenAiCompatible`](crate::OpenAiCompatible) and, for tests,
/// [`FakeProvider`](crate::FakeProvider).
pub trait LlmProvider {
    /// A short stable name for the `llm_usage.provider` column, such as `"anthropic"`.
    fn provider_name(&self) -> &str;

    /// The configured model, for the `llm_usage.model` column.
    fn model(&self) -> &str;

    /// Sends one request and returns the model's text.
    ///
    /// # Errors
    ///
    /// An [`LlmError`] that never contains the provider's response body or any key.
    fn complete(
        &self,
        request: CompletionRequest,
    ) -> impl Future<Output = Result<Completion, LlmError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_estimate_rounds_up_by_characters() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens("雪雪雪雪雪"), 2);
    }

    #[test]
    fn usage_total_saturates() {
        let usage = TokenUsage {
            input: u64::MAX,
            output: 5,
            estimated: false,
        };
        assert_eq!(usage.total(), u64::MAX);
    }
}
