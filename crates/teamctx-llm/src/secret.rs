//! The API key wrapper. The key is read from the environment variable named in the
//! configuration and is never shown by `Debug`, errors or logs.

use std::fmt;

use crate::error::LlmError;

/// A provider API key. `Debug` prints `ApiKey(***)`; there is no `Display`.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key. Surrounding whitespace is trimmed.
    ///
    /// # Errors
    ///
    /// [`LlmError::InvalidConfig`] if the key is empty or contains control characters
    /// (it would not be a valid header value).
    pub fn new(key: impl AsRef<str>) -> Result<Self, LlmError> {
        let key = key.as_ref().trim();
        if key.is_empty() {
            return Err(LlmError::InvalidConfig("the API key is empty"));
        }
        if key.chars().any(char::is_control) {
            return Err(LlmError::InvalidConfig(
                "the API key contains control characters",
            ));
        }
        Ok(Self(key.to_owned()))
    }

    /// Reads the key from the environment variable `var`.
    ///
    /// # Errors
    ///
    /// [`LlmError::MissingApiKey`] if the variable is unset, not Unicode or empty;
    /// [`LlmError::InvalidConfig`] if the value is not a usable key.
    pub fn from_env(var: &str) -> Result<Self, LlmError> {
        Self::from_lookup(var, |name| std::env::var(name).ok())
    }

    /// Like [`ApiKey::from_env`] with an injectable lookup, for tests and hosts that
    /// keep secrets elsewhere.
    ///
    /// # Errors
    ///
    /// The same as [`ApiKey::from_env`].
    pub fn from_lookup(
        var: &str,
        lookup: impl FnOnce(&str) -> Option<String>,
    ) -> Result<Self, LlmError> {
        match lookup(var) {
            Some(value) if !value.trim().is_empty() => Self::new(value),
            _ => Err(LlmError::MissingApiKey {
                var: var.to_owned(),
            }),
        }
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_hides_the_key() {
        let key = ApiKey::new("sk-fictional-0123456789").unwrap();
        assert_eq!(format!("{key:?}"), "ApiKey(***)");
        assert_eq!(format!("{key:#?}"), "ApiKey(***)");
        assert_eq!(key.expose(), "sk-fictional-0123456789");
    }

    #[test]
    fn lookup_names_the_variable_but_never_a_value() {
        let missing = ApiKey::from_lookup("TEAMCTX_LLM_KEY", |_| None).unwrap_err();
        assert_eq!(
            missing,
            LlmError::MissingApiKey {
                var: "TEAMCTX_LLM_KEY".to_owned()
            }
        );
        assert!(missing.to_string().contains("TEAMCTX_LLM_KEY"));
        let blank = ApiKey::from_lookup("K", |_| Some("  \n".to_owned())).unwrap_err();
        assert!(matches!(blank, LlmError::MissingApiKey { .. }));
        let key = ApiKey::from_lookup("K", |_| Some(" secret-value\n".to_owned())).unwrap();
        assert_eq!(key.expose(), "secret-value");
    }

    #[test]
    fn unusable_keys_are_rejected_without_echoing_them() {
        let error = ApiKey::new("bad\nkey").unwrap_err();
        assert!(!error.to_string().contains("bad"));
        assert!(ApiKey::new("").is_err());
    }

    #[test]
    fn from_env_reports_an_unset_variable() {
        let error = ApiKey::from_env("TEAMCTX_TEST_SURELY_UNSET_VARIABLE").unwrap_err();
        assert!(matches!(error, LlmError::MissingApiKey { .. }));
    }
}
