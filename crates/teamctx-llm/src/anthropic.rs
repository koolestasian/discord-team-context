//! Anthropic Messages API adapter: `POST {base_url}/v1/messages` over plain HTTP.

use std::time::Duration;

use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::http::{DEFAULT_TIMEOUT, build_client, checked_base_url, post_json, secret_header};
use crate::provider::{Completion, CompletionRequest, LlmProvider, TokenUsage};
use crate::secret::ApiKey;

/// The API version header value this adapter is written against.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Settings for [`AnthropicMessages`]. Nothing is hardcoded: the model and the base URL
/// come from the configuration, the key from the environment variable it names.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// Scheme and host, for example `https://api.anthropic.com`.
    pub base_url: String,
    /// The model name.
    pub model: String,
    /// The API key (see [`ApiKey::from_env`]).
    pub api_key: ApiKey,
    /// Per-request timeout; [`DEFAULT_TIMEOUT`] is 60 seconds.
    pub timeout: Duration,
}

impl AnthropicConfig {
    /// A configuration with the default 60 second timeout.
    #[must_use]
    pub fn new(base_url: impl Into<String>, model: impl Into<String>, api_key: ApiKey) -> Self {
        Self {
            base_url: base_url.into(),
            model: model.into(),
            api_key,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

/// The Anthropic Messages adapter.
#[derive(Debug)]
pub struct AnthropicMessages {
    client: Client,
    endpoint: String,
    model: String,
    api_key: ApiKey,
}

impl AnthropicMessages {
    /// Checks the configuration and builds the HTTP client.
    ///
    /// # Errors
    ///
    /// [`LlmError::InvalidConfig`] for a bad base URL, an empty model name or a client
    /// that cannot be built.
    pub fn new(config: AnthropicConfig) -> Result<Self, LlmError> {
        if config.model.trim().is_empty() {
            return Err(LlmError::InvalidConfig("the model name is empty"));
        }
        let base = checked_base_url(&config.base_url, true)?;
        Ok(Self {
            client: build_client(config.timeout)?,
            endpoint: format!("{base}/v1/messages"),
            model: config.model,
            api_key: config.api_key,
        })
    }
}

#[derive(Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    max_tokens: u32,
    system: &'a str,
    messages: [RequestMessage<'a>; 1],
}

#[derive(Serialize)]
struct RequestMessage<'a> {
    role: &'static str,
    content: &'a str,
}

#[derive(Deserialize)]
struct ResponseBody {
    content: Vec<ContentBlock>,
    usage: Usage,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

fn parse_response(bytes: &[u8]) -> Result<Completion, LlmError> {
    let body: ResponseBody = serde_json::from_slice(bytes)
        .map_err(|_| LlmError::MalformedResponse("not the expected JSON"))?;
    let text: String = body
        .content
        .into_iter()
        .filter(|block| block.kind == "text")
        .filter_map(|block| block.text)
        .collect();
    if text.trim().is_empty() {
        return Err(LlmError::MalformedResponse("no text in the answer"));
    }
    Ok(Completion {
        text,
        usage: TokenUsage {
            input: body.usage.input_tokens,
            output: body.usage.output_tokens,
            estimated: false,
        },
    })
}

impl LlmProvider for AnthropicMessages {
    fn provider_name(&self) -> &str {
        "anthropic"
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn complete(&self, request: CompletionRequest) -> Result<Completion, LlmError> {
        let body = RequestBody {
            model: &self.model,
            max_tokens: request.max_output_tokens,
            system: &request.system,
            messages: [RequestMessage {
                role: "user",
                content: &request.user,
            }],
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", secret_header(self.api_key.expose())?);
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        let bytes = post_json(&self.client, &self.endpoint, headers, &body).await?;
        parse_response(&bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_text_blocks_and_ignores_other_blocks() {
        let completion = parse_response(
            br#"{"content":[{"type":"text","text":"Hello "},{"type":"thinking","thinking":"x"},{"type":"text","text":"world [D-1]"}],"usage":{"input_tokens":12,"output_tokens":5}}"#,
        )
        .unwrap();
        assert_eq!(completion.text, "Hello world [D-1]");
        assert_eq!(completion.usage.input, 12);
        assert_eq!(completion.usage.output, 5);
        assert!(!completion.usage.estimated);
    }

    #[test]
    fn rejects_malformed_bodies() {
        for body in [
            "".as_bytes(),
            b"not json",
            br#"{"content":[]}"#,
            br#"{"content":[],"usage":{"input_tokens":1,"output_tokens":1}}"#,
            br#"{"content":[{"type":"text","text":"  "}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
            br#"{"content":[{"type":"text","text":"hi"}],"usage":{"input_tokens":-1,"output_tokens":1}}"#,
        ] {
            assert!(matches!(
                parse_response(body),
                Err(LlmError::MalformedResponse(_))
            ));
        }
    }
}
