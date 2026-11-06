//! The fixed stopword list shared by `search` (bare words) and `context_for`.
//!
//! Stopwords carry no topic: they are the small function words and question words of a
//! natural-language question ("What database did we finally pick?"). Dropping them keeps
//! a bare-word search from matching every message that merely contains "the" or "we".
//! The list is fixed, lowercase and general English; it is not tuned to any dataset.
//! Words that can name a topic ("done", "before", "after", "all") are deliberately kept.

/// Lowercase stopwords, sorted so [`is_stopword`] can binary-search.
const STOPWORDS: &[&str] = &[
    "a", "about", "am", "an", "and", "are", "as", "at", "be", "been", "being", "but", "by", "can",
    "could", "did", "do", "does", "doing", "for", "from", "had", "has", "have", "he", "her", "him",
    "his", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "of", "on", "or", "our",
    "ours", "she", "should", "so", "than", "that", "the", "their", "them", "then", "there",
    "these", "they", "this", "those", "to", "us", "was", "we", "were", "what", "when", "where",
    "which", "who", "whom", "whose", "why", "will", "with", "would", "you", "your", "yours",
];

/// Apostrophe contractions of the words above, written with `'` for both apostrophes.
const CONTRACTIONS: &[&str] = &[
    "can't",
    "couldn't",
    "didn't",
    "doesn't",
    "don't",
    "hasn't",
    "haven't",
    "he's",
    "how's",
    "i'd",
    "i'll",
    "i'm",
    "i've",
    "isn't",
    "it's",
    "she's",
    "shouldn't",
    "that's",
    "there's",
    "they're",
    "we'd",
    "we'll",
    "we're",
    "we've",
    "wasn't",
    "weren't",
    "what's",
    "when's",
    "where's",
    "who's",
    "won't",
    "wouldn't",
    "you'd",
    "you'll",
    "you're",
    "you've",
];

/// True when `word`, ignoring case, surrounding punctuation and a trailing possessive
/// `'s`, is a stopword. Words with no letters or digits (`-`, `*`) are not stopwords.
#[must_use]
pub fn is_stopword(word: &str) -> bool {
    let normal = word
        .trim_matches(|c: char| !c.is_alphanumeric())
        .replace('\u{2019}', "'")
        .to_lowercase();
    if normal.is_empty() {
        return false;
    }
    if STOPWORDS.binary_search(&normal.as_str()).is_ok() || CONTRACTIONS.contains(&normal.as_str())
    {
        return true;
    }
    normal
        .strip_suffix("'s")
        .is_some_and(|stem| STOPWORDS.binary_search(&stem).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_is_sorted_unique_and_lowercase() {
        assert!(STOPWORDS.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(
            STOPWORDS
                .iter()
                .chain(CONTRACTIONS)
                .all(|word| *word == word.to_lowercase())
        );
    }

    #[test]
    fn recognises_question_words_despite_case_and_punctuation() {
        for word in [
            "What",
            "did",
            "WE",
            "the",
            "it?",
            "(how)",
            "what\u{2019}s",
            "Didn't",
            "who's",
        ] {
            assert!(is_stopword(word), "{word:?}");
        }
        for word in ["database", "pick", "sqlite", "teams", "done", "-", "*", ""] {
            assert!(!is_stopword(word), "{word:?}");
        }
    }
}
