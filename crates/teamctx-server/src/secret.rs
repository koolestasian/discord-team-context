//! A wrapper that keeps secrets out of logs, panic messages and error text (plan
//! section 13).
//!
//! Rules the type enforces:
//! - `Debug` and `Display` print `***`, so `{:?}`, `{}`, `tracing` fields and any error
//!   or struct that embeds a `Secret` never show the value.
//! - It does not implement `Serialize`, `Clone` or `PartialEq`, so a secret cannot be
//!   written into a JSON response, copied around casually, or compared with a
//!   short-circuiting `==`.
//! - [`Secret::expose`] is the only way to read the value, so every use is greppable.
//!
//! Not covered: the memory is not zeroed on drop (no `zeroize` dependency, plan
//! section 17), and a caller can still copy the exposed value somewhere else.

use std::fmt;

/// Holds a secret value. `Debug` and `Display` print `***`, never the value.
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
        f.write_str("***")
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("***")
    }
}

#[cfg(test)]
mod tests {
    use std::marker::PhantomData;

    use super::Secret;

    const VALUE: &str = "tctx_test_do_not_print";

    #[test]
    fn debug_and_display_print_only_stars() {
        let secret = Secret::new(VALUE.to_owned());
        assert_eq!(format!("{secret}"), "***");
        assert_eq!(format!("{secret:?}"), "***");
        assert_eq!(format!("{secret:#?}"), "***");
        assert_eq!(format!("{secret:>40}"), "***");
        assert_eq!(secret.expose(), VALUE);
    }

    #[test]
    fn embedding_types_do_not_reveal_the_value() {
        #[derive(Debug)]
        #[expect(dead_code, reason = "only read through Debug")]
        struct Config {
            name: &'static str,
            token: Secret<String>,
        }
        #[derive(Debug, thiserror::Error)]
        #[error("token {0} was rejected")]
        struct Rejected(Secret<String>);

        let config = Config {
            name: "demo",
            token: Secret::new(VALUE.to_owned()),
        };
        let error = Rejected(Secret::new(VALUE.to_owned()));
        for text in [
            format!("{config:?}"),
            format!("{config:#?}"),
            error.to_string(),
            format!("{error:?}"),
            format!("{:?}", Some(Secret::new(VALUE))),
            format!("{:?}", vec![Secret::new(VALUE)]),
        ] {
            assert!(!text.contains(VALUE), "{text}");
            assert!(!text.contains("tctx_"), "{text}");
            assert!(text.contains("***"), "{text}");
        }
    }

    /// Compile-time check without extra tooling: the inherent `IMPLEMENTS` exists only
    /// when `T: Serialize`; otherwise the trait default (`false`) is used.
    struct Probe<T>(PhantomData<T>);
    trait NotSerialize {
        const IMPLEMENTS: bool = false;
    }
    impl<T> NotSerialize for Probe<T> {}
    impl<T: serde::Serialize> Probe<T> {
        const IMPLEMENTS: bool = true;
    }

    // Checked at compile time: the test build fails if `Secret` ever implements
    // `Serialize`. The first line proves the probe itself works.
    const _: () = assert!(Probe::<String>::IMPLEMENTS);
    const _: () = assert!(!Probe::<Secret<String>>::IMPLEMENTS);
}
