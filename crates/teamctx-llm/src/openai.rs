//! OpenAI-compatible Chat Completions adapter: `POST {base_url}/chat/completions`.
//!
//! The base URL is configurable, so the same adapter covers OpenRouter, Ollama
//! (`http://localhost:11434/v1`) and LM Studio (`http://localhost:1234/v1`). The key is
//! optional because local servers need none.

use std::time::Duration;

use reqwest::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::http::{DEFAULT_TIMEOUT, build_client, checked_base_url, post_json, secret_header};
use crate::provider::{Completion, CompletionRequest, LlmProvider, TokenUsage, estimate_tokens};
use crate::secret::ApiKey;

/// Settings for [`OpenAiCompatible`].
#[derive(Debug, Clone)]
pub struct OpenAiConfig {
    /// The URL that `/chat/completions` is appended to, including any `/v1`.
    pub base_url: String,
    /// The model name.
    pub model: String,
    /// The API key, if the server needs one.
    pub api_key: Option<ApiKey>,
    /// Per-request timeout; [`DEFAULT_TIMEOUT`] is 60 seconds.
    pub timeout: Duration,
}

impl OpenAiConfig {
    /// A configuration with the default 60 second timeout.
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<ApiKey>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// The OpenAI-compatible adapter.
#[derive(Debug)]
pub struct OpenAiCompatible {
    client: Client,
    endpoint: String,
    model: String,
    api_key: Option<ApiKey>,
}

impl OpenAiCompatible {
    /// Checks the configuration and builds the HTTP client.
    ///
    /// # Errors
    ///
    /// [`LlmError::InvalidConfig`] for a bad base URL, an empty model name or a client
    /// that cannot be built.
    pub fn new(config: OpenAiConfig) -> Result<Self, LlmError> {
        if config.model.trim().is_empty() {
            return Err(LlmError::InvalidConfig("the model name is empty"));
        }
        let base = checked_base_url(&config.base_url, config.api_key.is_some())?;
        Ok(Self {
            client: build_client(config.timeout)?,
            endpoint: format!("{base}/chat/completions"),
            model: config.model,
            api_key: config.api_key,
        })
    }
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    stream: bool,
    messages: [RequestMessage<'a>; 2],
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ResponseBody {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

fn parse_response(bytes: &[u8], request: &CompletionRequest) -> Result<Completion, LlmError> {
    let body: ResponseBody = serde_json::from_slice(bytes)
        .map_err(|_| LlmError::MalformedResponse("not the expected JSON"))?;
    let text = body
        .choices
        .into_iter()
        .next()
        .and_then(|choice| choice.message.content)
        .filter(|text| !text.trim().is_empty())
        .ok_or(LlmError::MalformedResponse("no text in the answer"))?;
    // Local servers sometimes omit `usage`; budgets still need a number.
    let usage = match body.usage {
        Some(usage) => TokenUsage {
            input: usage.prompt_tokens,
            output: usage.completion_tokens,
            estimated: false,
        },
        None => TokenUsage {
            input: estimate_tokens(&request.system).saturating_add(estimate_tokens(&request.user)),
            output: estimate_tokens(&text),
            estimated: true,
        },
    };
    Ok(Completion { text, usage })
}

impl LlmProvider for OpenAiCompatible {
    fn provider_name(&self) -> &str {
        "openai_compatible"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, LlmError> {
        let body = RequestBody {
            model: &self.model,
            max_tokens: request.max_output_tokens,
            stream: false,
            messages: [
                RequestMessage {
                    role: "system",
                    content: &request.system,
                },
                RequestMessage {
                    role: "user",
                    content: &request.user,
                },
            ],
        };
        let mut headers = HeaderMap::new();
        if let Some(key) = &self.api_key {
            let value = format!("Bearer {}", key.expose());
            headers.insert(AUTHORIZATION, secret_header(&value)?);
        }
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let bytes = post_json(&self.client, &self.endpoint, headers, &body).await?;
        parse_response(&bytes, &request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> CompletionRequest {
        CompletionRequest {
            system: "rules".to_owned(),
            user: "question".to_owned(),
            max_output_tokens: 10,
        }
    }

    #[test]
    fn reads_the_first_choice_and_usage() {
        let completion = parse_response(
            br#"{"choices":[{"message":{"role":"assistant","content":"Yes [D-1]"}}],"usage":{"prompt_tokens":20,"completion_tokens":4,"total_tokens":24}}"#,
            &request(),
        )
        .unwrap();
        assert_eq!(completion.text, "Yes [D-1]");
        assert_eq!(completion.usage.total(), 24);
        assert!(!completion.usage.estimated);
    }

    #[test]
    fn missing_usage_is_estimated() {
        let completion = parse_response(
            br#"{"choices":[{"message":{"content":"abcdefgh"}}]}"#,
            &request(),
        )
        .unwrap();
        assert!(completion.usage.estimated);
        assert_eq!(completion.usage.output, 2);
        assert!(completion.usage.input > 0);
    }

    #[test]
    fn rejects_malformed_bodies() {
        for body in [
            "".as_bytes(),
            b"[]",
            br#"{"choices":[]}"#,
            br#"{"choices":[{"message":{"content":null}}]}"#,
            br#"{"choices":[{"message":{"content":"  "}}]}"#,
            br#"{"choices":[{"message":{}}]}"#,
        ] {
            assert!(matches!(
                parse_response(body, &request()),
                Err(LlmError::MalformedResponse(_))
            ));
        }
    }
}
