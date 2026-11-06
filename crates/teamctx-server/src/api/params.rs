//! Query-string parsing for the JSON API.
//!
//! The query string is untrusted input. It is decoded once (`%XX` and `+`), must be UTF-8,
//! and may only use the parameter names a route lists, each at most once. Empty values count
//! as absent, so `record?kind=&status=` is the same as `record`. Errors are fixed strings
//! that never echo the input.

/// The longest raw query string accepted, in bytes.
const MAX_QUERY_BYTES: usize = 4096;

/// Why a query string was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParamError {
    TooLong,
    /// Bad `%` escape or bytes that are not UTF-8.
    Malformed,
    /// A name the route does not take.
    Unknown,
    /// The same name twice.
    Duplicate,
    /// A number parameter that is not a whole number.
    NotANumber(&'static str),
    /// A required parameter that is absent or empty.
    Missing(&'static str),
}

impl ParamError {
    /// The message sent back. Written here, never built from the request.
    pub(crate) fn message(self) -> String {
        match self {
            Self::TooLong => "The query string is too long.".to_owned(),
            Self::Malformed => "The query string is not valid percent-encoded UTF-8.".to_owned(),
            Self::Unknown => "The request has a parameter this route does not take.".to_owned(),
            Self::Duplicate => "A parameter was given more than once.".to_owned(),
            Self::NotANumber(name) => format!("`{name}` must be a whole number."),
            Self::Missing(name) => format!("`{name}` is required."),
        }
    }
}

/// Decoded parameters of one request.
#[derive(Debug, Default)]
pub(crate) struct Params(Vec<(String, String)>);

impl Params {
    /// Parses `raw` (the part after `?`), allowing only the names in `allowed`.
    pub(crate) fn parse(raw: Option<&str>, allowed: &[&str]) -> Result<Self, ParamError> {
        let Some(raw) = raw else {
            return Ok(Self::default());
        };
        if raw.len() > MAX_QUERY_BYTES {
            return Err(ParamError::TooLong);
        }
        let mut pairs: Vec<(String, String)> = Vec::new();
        for part in raw.split('&').filter(|part| !part.is_empty()) {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            let name = decode(name)?;
            let value = decode(value)?;
            if !allowed.contains(&name.as_str()) {
                return Err(ParamError::Unknown);
            }
            if pairs.iter().any(|(seen, _)| *seen == name) {
                return Err(ParamError::Duplicate);
            }
            pairs.push((name, value));
        }
        Ok(Self(pairs))
    }

    /// The value of `name`, or `None` when absent or empty.
    pub(crate) fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .filter(|value| !value.is_empty())
    }

    /// Like [`Self::get`] but an absent value is an error.
    pub(crate) fn required(&self, name: &'static str) -> Result<&str, ParamError> {
        self.get(name).ok_or(ParamError::Missing(name))
    }

    /// A whole-number parameter.
    pub(crate) fn number(&self, name: &'static str) -> Result<Option<u32>, ParamError> {
        match self.get(name) {
            None => Ok(None),
            Some(text) => text
                .parse::<u32>()
                .map(Some)
                .map_err(|_| ParamError::NotANumber(name)),
        }
    }
}

fn decode(text: &str) -> Result<String, ParamError> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' => {
                let hex = bytes.get(i + 1..i + 3).ok_or(ParamError::Malformed)?;
                let digits = std::str::from_utf8(hex).map_err(|_| ParamError::Malformed)?;
                out.push(u8::from_str_radix(digits, 16).map_err(|_| ParamError::Malformed)?);
                i += 2;
            }
            byte => out.push(byte),
        }
        i += 1;
    }
    String::from_utf8(out).map_err(|_| ParamError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALLOWED: &[&str] = &["q", "kind", "limit"];

    #[test]
    fn decodes_percent_escapes_and_plus() {
        let params = Params::parse(Some("q=a%20b+c%26d&kind=task"), ALLOWED).unwrap();
        assert_eq!(params.get("q"), Some("a b c&d"));
        assert_eq!(params.get("kind"), Some("task"));
        let params = Params::parse(Some("q=%E2%80%A6"), ALLOWED).unwrap();
        assert_eq!(params.get("q"), Some("…"));
    }

    #[test]
    fn empty_values_and_no_query_are_absent() {
        let params = Params::parse(Some("kind=&q"), ALLOWED).unwrap();
        assert_eq!(params.get("kind"), None);
        assert_eq!(params.get("q"), None);
        assert_eq!(Params::parse(None, ALLOWED).unwrap().get("q"), None);
        assert_eq!(params.required("q").unwrap_err(), ParamError::Missing("q"));
    }

    #[test]
    fn refuses_bad_escapes_unknown_and_repeated_names() {
        for bad in ["q=%", "q=%2", "q=%zz", "q=%FF", "q=%C3"] {
            assert_eq!(
                Params::parse(Some(bad), ALLOWED).unwrap_err(),
                ParamError::Malformed,
                "{bad}"
            );
        }
        assert_eq!(
            Params::parse(Some("other=1"), ALLOWED).unwrap_err(),
            ParamError::Unknown
        );
        assert_eq!(
            Params::parse(Some("q=1&q=2"), ALLOWED).unwrap_err(),
            ParamError::Duplicate
        );
        let long = format!("q={}", "a".repeat(MAX_QUERY_BYTES));
        assert_eq!(
            Params::parse(Some(&long), ALLOWED).unwrap_err(),
            ParamError::TooLong
        );
    }

    #[test]
    fn numbers_must_be_whole() {
        let ok = Params::parse(Some("limit=4"), ALLOWED).unwrap();
        assert_eq!(ok.number("limit"), Ok(Some(4)));
        assert_eq!(ok.number("kind"), Ok(None));
        for bad in ["limit=-1", "limit=1.5", "limit=x", "limit=99999999999"] {
            let params = Params::parse(Some(bad), ALLOWED).unwrap();
            assert_eq!(params.number("limit"), Err(ParamError::NotANumber("limit")));
        }
    }
}
