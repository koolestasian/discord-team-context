//! Shared HTTP plumbing for the two adapters: URL checks, a client that never follows
//! redirects, a bounded body read, and error mapping that drops everything the provider
//! or the network said.

use std::net::IpAddr;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{Client, Url, redirect};
use serde::Serialize;

use crate::error::LlmError;

/// The time limit for one request (plan section 11, step 6).
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// The most response bytes an adapter reads.
pub(crate) const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Checks a base URL and returns it without a trailing slash.
///
/// Rules: `http` or `https`; a host; no user info, query or fragment; and plain `http`
/// only for loopback hosts when an API key will be sent, so a key never travels in
/// clear text across a network.
pub(crate) fn checked_base_url(base_url: &str, sends_key: bool) -> Result<String, LlmError> {
    let url = Url::parse(base_url.trim())
        .map_err(|_| LlmError::InvalidConfig("base_url is not a valid URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(LlmError::InvalidConfig("base_url must use http or https"));
    }
    if url.host().is_none() {
        return Err(LlmError::InvalidConfig("base_url has no host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LlmError::InvalidConfig(
            "base_url must not contain a user name or password",
        ));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(LlmError::InvalidConfig(
            "base_url must not contain a query or fragment",
        ));
    }
    if url.scheme() == "http" && sends_key && !is_loopback(&url) {
        return Err(LlmError::InvalidConfig(
            "an API key may only be sent over plain http to localhost; use https",
        ));
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // IPv6 hosts come back in brackets.
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
        .is_ok_and(|ip| ip.is_loopback())
}

/// A client with the given timeout that never follows redirects, so an API key in a
/// header cannot be forwarded to another host.
pub(crate) fn build_client(timeout: Duration) -> Result<Client, LlmError> {
    Client::builder()
        .timeout(timeout)
        .redirect(redirect::Policy::none())
        .build()
        .map_err(|_| LlmError::InvalidConfig("could not build the HTTP client"))
}

/// A sensitive header value (excluded from reqwest's own debug output).
pub(crate) fn secret_header(value: &str) -> Result<HeaderValue, LlmError> {
    let mut header = HeaderValue::from_str(value)
        .map_err(|_| LlmError::InvalidConfig("the API key is not a valid header value"))?;
    header.set_sensitive(true);
    Ok(header)
}

/// Sends `body` as JSON and returns the response body (at most [`MAX_RESPONSE_BYTES`]).
/// A non-success status becomes [`LlmError::Http`] without reading the body.
pub(crate) async fn post_json(
    client: &Client,
    url: &str,
    headers: HeaderMap,
    body: &impl Serialize,
) -> Result<Vec<u8>, LlmError> {
    let mut response = client
        .post(url)
        .headers(headers)
        .json(body)
        .send()
        .await
        .map_err(|error| map_error(&error))?;
    let status = response.status();
    if !status.is_success() {
        return Err(LlmError::Http {
            status: status.as_u16(),
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(LlmError::ResponseTooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| map_error(&error))? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(LlmError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Maps a transport error to a unit-like variant. The original error is dropped: it can
/// contain the URL and low-level details nobody should show.
fn map_error(error: &reqwest::Error) -> LlmError {
    if error.is_timeout() {
        LlmError::Timeout
    } else if error.is_connect() {
        LlmError::Connect
    } else {
        LlmError::Transport
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert_eq!(
            checked_base_url("https://api.example.com/", true).unwrap(),
            "https://api.example.com"
        );
        assert_eq!(
            checked_base_url("http://localhost:11434/v1", true).unwrap(),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            checked_base_url("http://127.0.0.1:1234/v1/", true).unwrap(),
            "http://127.0.0.1:1234/v1"
        );
        assert!(checked_base_url("http://[::1]:8080", true).is_ok());
        // No key, no cleartext concern (a LAN Ollama box).
        assert!(checked_base_url("http://192.168.1.20:11434/v1", false).is_ok());
        for bad in [
            "http://api.example.com",
            "ftp://api.example.com",
            "https://user:pass@api.example.com",
            "https://user@api.example.com",
            "https://api.example.com/v1?key=1",
            "https://api.example.com/#frag",
            "not a url",
            "",
        ] {
            assert!(checked_base_url(bad, true).is_err(), "{bad}");
        }
    }

    #[test]
    fn config_errors_do_not_echo_the_url() {
        let error = checked_base_url("https://user:hunter2@api.example.com", true).unwrap_err();
        assert!(!error.to_string().contains("hunter2"));
    }
}
