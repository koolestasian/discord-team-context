//! Access-token format, generation and hashing (plan section 13, ADR 0006).
//!
//! Format: `tctx_` followed by 32 bytes from the operating system's random number
//! generator, written as 64 lowercase hex digits (69 characters in all). The fixed prefix
//! lets secret scanners recognise a leaked token; hex keeps the encoding obviously
//! correct without another dependency. 256 random bits cannot be guessed online, so the
//! failure rate limit only keeps noise and load down.
//!
//! Only `SHA-256(token)` is stored. A plain hash (no salt, no slow KDF) is right here:
//! the input is 256 random bits, not a password, and a fast hash lets every request
//! look its token up by digest.

use sha2::{Digest, Sha256};
use teamctx_core::repo::TokenHash;
use thiserror::Error;

use crate::secret::Secret;

/// Every token starts with this.
pub const PREFIX: &str = "tctx_";

/// Random bytes per token.
const RANDOM_BYTES: usize = 32;

/// Length of a token in characters: the prefix plus two hex digits per byte.
pub const TOKEN_LEN: usize = PREFIX.len() + 2 * RANDOM_BYTES;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// A freshly generated token: the plaintext to show once, and the hash to store.
pub struct NewToken {
    /// The token. Show it to the person once; never store or log it.
    pub plaintext: Secret<String>,
    /// What the store keeps.
    pub hash: TokenHash,
}

impl std::fmt::Debug for NewToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The hash is not a secret, but nothing needs to see it either.
        f.debug_struct("NewToken")
            .field("plaintext", &self.plaintext)
            .finish_non_exhaustive()
    }
}

/// The operating system could not provide random bytes. No token was created.
#[derive(Debug, Error)]
#[error("the operating system's random number generator failed")]
pub struct RandomError(#[source] getrandom::Error);

/// Generates a token from 32 bytes of operating-system randomness.
///
/// # Errors
///
/// [`RandomError`] if the operating system's generator fails; there is no fallback.
pub fn generate() -> Result<NewToken, RandomError> {
    let mut bytes = [0_u8; RANDOM_BYTES];
    getrandom::fill(&mut bytes).map_err(RandomError)?;
    let mut text = String::with_capacity(TOKEN_LEN);
    text.push_str(PREFIX);
    for byte in bytes {
        text.push(char::from(HEX[usize::from(byte >> 4)]));
        text.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    let hash = hash(text.as_bytes());
    Ok(NewToken {
        plaintext: Secret::new(text),
        hash,
    })
}

/// SHA-256 of whatever bytes were presented.
#[must_use]
pub fn hash(token: &[u8]) -> TokenHash {
    TokenHash(Sha256::digest(token).into())
}

/// Whether `candidate` has the token shape: the prefix, then exactly 64 lowercase hex
/// digits. It inspects every byte, so its time depends only on the input length.
#[must_use]
pub fn is_well_formed(candidate: &[u8]) -> bool {
    if candidate.len() != TOKEN_LEN {
        return false;
    }
    let (prefix, digits) = candidate.split_at(PREFIX.len());
    let mut ok = prefix == PREFIX.as_bytes();
    for &byte in digits {
        ok &= byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_have_the_documented_shape() {
        let token = generate().expect("random bytes");
        let text = token.plaintext.expose();
        assert_eq!(text.len(), 69);
        assert!(text.starts_with("tctx_"));
        assert!(is_well_formed(text.as_bytes()));
        assert_eq!(token.hash, hash(text.as_bytes()));
    }

    #[test]
    fn generated_tokens_differ() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            let token = generate().expect("random bytes");
            assert!(seen.insert(token.plaintext.expose().clone()));
        }
    }

    #[test]
    fn hash_is_sha256() {
        // FIPS 180-2 test vector for "abc".
        let expected = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];
        assert_eq!(hash(b"abc"), TokenHash(expected));
    }

    #[test]
    fn debug_hides_the_plaintext_and_the_hash() {
        let token = generate().expect("random bytes");
        let text = format!("{token:?}");
        assert!(!text.contains(token.plaintext.expose().as_str()), "{text}");
        assert!(!text.contains("tctx_"), "{text}");
        assert!(!text.contains("TokenHash"), "{text}");
    }

    #[test]
    fn shape_check_rejects_near_misses() {
        let good = format!("tctx_{}", "0123456789abcdef".repeat(4));
        assert!(is_well_formed(good.as_bytes()));
        let upper = good.to_uppercase().replace("TCTX_", "tctx_");
        let bad = [
            "",
            "tctx_",
            &good[..68],
            &format!("{good}0"),
            &good.replace("tctx_", "tctX_"),
            &good.replace("tctx_", "xxxx_"),
            &upper,
            &good.replacen('a', "g", 1),
            &good.replacen('a', " ", 1),
            &good.replacen('a', "\u{0}", 1),
        ];
        for candidate in bad {
            assert!(!is_well_formed(candidate.as_bytes()), "{candidate:?}");
        }
    }
}
