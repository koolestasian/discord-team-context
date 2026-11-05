//! A minimal wrapper that keeps secrets out of logs and panic messages.
//!
//! Spike S3 only needs this much. The token store (T11) owns the real type.

use std::fmt;

/// Holds a secret value. `Debug` and `Display` print `***`, never the value.
///
/// The only way to read the value is [`Secret::expose`], so every use is greppable.
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps a value.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Returns the wrapped value. Do not log or format the result.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use super::Secret;

    #[test]
    fn debug_and_display_hide_the_value() {
        let secret = Secret::new("tctx_test_do_not_print".to_owned());
        assert_eq!(format!("{secret}"), "***");
        assert!(!format!("{secret:?}").contains("tctx_test"));
        assert_eq!(secret.expose(), "tctx_test_do_not_print");
    }
}
