//! A deterministic provider for tests and offline demos. It never touches the network.

use std::sync::{Mutex, PoisonError};

use crate::error::LlmError;
use crate::provider::{Completion, CompletionRequest, LlmProvider, TokenUsage, estimate_tokens};

type Responder = Box<dyn Fn(&CompletionRequest) -> Result<String, LlmError> + Send + Sync>;

/// A provider whose answer is a function of the request. It records every request it
/// receives, so tests can inspect exactly what would have been sent to a model.
pub struct FakeProvider {
    responder: Responder,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl std::fmt::Debug for FakeProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProvider")
            .field("calls", &self.call_count())
            .finish_non_exhaustive()
    }
}

impl FakeProvider {
    /// A provider that answers with `responder(request)`.
    pub fn new(
        responder: impl Fn(&CompletionRequest) -> Result<String, LlmError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            responder: Box::new(responder),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// A provider that always answers with `text`.
    #[must_use]
    pub fn fixed(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::new(move |_| Ok(text.clone()))
    }

    /// A provider that always fails with `error`.
    #[must_use]
    pub fn failing(error: LlmError) -> Self {
        Self::new(move |_| Err(error.clone()))
    }

    /// Every request received so far, oldest first.
    #[must_use]
    pub fn requests(&self) -> Vec<CompletionRequest> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many requests were received.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }
}

impl LlmProvider for FakeProvider {
    fn provider_name(&self) -> &str {
        "fake"
    }

    fn model(&self) -> &str {
        "fake-model"
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, LlmError> {
        let result = (self.responder)(&request);
        let input = estimate_tokens(&request.system).saturating_add(estimate_tokens(&request.user));
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let text = result?;
        let output = estimate_tokens(&text);
        Ok(Completion {
            text,
            usage: TokenUsage {
                input,
                output,
                estimated: true,
            },
        })
    }
}
