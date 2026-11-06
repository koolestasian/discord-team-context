//! Small readers for raw JSON payloads, shared by the mappers.

use serde_json::{Map, Value};
use teamctx_core::id::Snowflake;
use teamctx_core::time::UnixMillis;
use twilight_model::util::Timestamp;

use super::MapError;

/// The payload as a JSON object.
pub(super) fn object<'a>(
    value: &'a Value,
    what: &'static str,
) -> Result<&'a Map<String, Value>, MapError> {
    value
        .as_object()
        .ok_or_else(|| MapError::malformed(what, "expected a JSON object"))
}

/// A snowflake given as a JSON string (Discord's form) or a plain number.
pub(super) fn snowflake(value: &Value, what: &'static str) -> Result<Snowflake, MapError> {
    let parsed = match value {
        Value::String(text) => text.parse::<Snowflake>().ok(),
        Value::Number(number) => number.as_u64().and_then(Snowflake::new),
        _ => None,
    };
    parsed.ok_or_else(|| MapError::malformed(what, "expected a non-zero snowflake id"))
}

/// A required snowflake field.
pub(super) fn required_snowflake(
    map: &Map<String, Value>,
    key: &'static str,
) -> Result<Snowflake, MapError> {
    match map.get(key) {
        Some(value) => snowflake(value, key),
        None => Err(MapError::malformed(key, "missing field")),
    }
}

/// An optional snowflake field; `null` and absent both give `None`.
pub(super) fn optional_snowflake(
    map: &Map<String, Value>,
    key: &'static str,
) -> Result<Option<Snowflake>, MapError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => snowflake(value, key).map(Some),
    }
}

/// A required string field.
pub(super) fn required_str<'a>(
    map: &'a Map<String, Value>,
    key: &'static str,
) -> Result<&'a str, MapError> {
    map.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| MapError::malformed(key, "missing or not a string"))
}

/// An optional string field; `null`, absent and non-strings give `None`.
pub(super) fn optional_str<'a>(map: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    map.get(key).and_then(Value::as_str)
}

/// An ISO 8601 timestamp field as Unix milliseconds. `null` and absent give `None`.
pub(super) fn optional_millis(
    map: &Map<String, Value>,
    key: &'static str,
) -> Result<Option<UnixMillis>, MapError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Timestamp::parse(text)
            .map(|timestamp| {
                Some(UnixMillis::from_millis(
                    timestamp.as_micros().div_euclid(1_000),
                ))
            })
            .map_err(|_| MapError::malformed(key, "not an ISO 8601 timestamp")),
        Some(_) => Err(MapError::malformed(key, "not an ISO 8601 timestamp")),
    }
}
