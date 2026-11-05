//! Enums that are stored and exchanged as fixed lowercase text.
//!
//! The text forms match the `CHECK (... IN (...))` lists of plan section 6 exactly, so
//! the store can write [`as_str`](crate::record::EntryKind::as_str) values and read them
//! back with `FromStr`. Parsing is exact: no trimming, no case folding.

use thiserror::Error;

/// A text value did not name any variant of a text enum.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown {type_name} {value:?}")]
pub struct ParseEnumError {
    /// The enum that was being parsed, for example `"EntryStatus"`.
    pub type_name: &'static str,
    /// The rejected input. It may come from a user or a database row, so callers that
    /// show it must treat it as untrusted text.
    pub value: String,
}

/// Declares a fieldless enum with a fixed text form per variant.
///
/// Ordering follows declaration order. Generates `ALL`, `as_str`, `Display` (writes
/// `as_str`) and `FromStr` (exact match, error [`ParseEnumError`]). `FromStr` and
/// `as_str` are inverse by construction.
macro_rules! text_enum {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $text:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        $vis enum $name {
            $( $(#[$vmeta])* $variant, )+
        }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [Self] = &[$( Self::$variant, )+];

            /// The stored text form.
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $( Self::$variant => $text, )+
                }
            }
        }

        impl ::core::fmt::Display for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::core::str::FromStr for $name {
            type Err = $crate::text_enum::ParseEnumError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                match s {
                    $( $text => Ok(Self::$variant), )+
                    _ => Err($crate::text_enum::ParseEnumError {
                        type_name: stringify!($name),
                        value: s.to_owned(),
                    }),
                }
            }
        }
    };
}

pub(crate) use text_enum;
