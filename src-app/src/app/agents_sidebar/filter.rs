//! The one matching rule the app's list filters share.
//!
//! It used to be a family: the rail had its own search field, and this module
//! held the visibility rules that field needed - is this container visible,
//! is this surface visible under it, where does the query hit inside a title,
//! does anything match at all. The rail's field is gone (its head is a button
//! that opens the palette, which searches both what is open and what is only
//! on disk), and with it went every rule that only it used.
//!
//! What is left is the matcher itself, shared with the sessions sidebar so
//! the two search boxes still behave identically.

/// Case-insensitive substring match. `lowered_needle` MUST already be
/// `to_lowercase()`-ed by the caller -- on a workspace with N projects
/// and P threads/project the previous "lowercase inside" form burned
/// N*P needle allocations per keystroke (audit P1-4). Empty needle
/// matches everything; the caller short-circuits before this is
/// called, but the behaviour is documented anyway for symmetry.
#[inline]
pub(crate) fn matches(haystack: &str, lowered_needle: &str) -> bool {
    if lowered_needle.is_empty() {
        return true;
    }
    haystack.to_lowercase().contains(lowered_needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches("anything", ""));
    }

    #[test]
    fn matches_is_case_insensitive() {
        // Callers must pre-lower the needle (the filter's contract): the
        // haystack is lowered here, the needle is taken as-is.
        assert!(matches("Splitlane", "lane"));
        assert!(matches("splitlane", "split"));
        assert!(!matches("splitlane", "xyz"));
    }

    #[test]
    fn an_already_lowered_needle_is_taken_as_is() {
        // An upper-case needle is the caller's bug, and the matcher does not
        // paper over it - documenting that is what keeps the contract real.
        assert!(!matches("Splitlane", "PANE"));
    }
}
