//! Tests for the entity-tag matching logic.
//!
//! The crate is a `cdylib` that links against nginx, so its symbols cannot be
//! resolved in a plain test binary. Including the pure module directly sidesteps
//! that: this test compiles no nginx code at all.

include!("../src/etag.rs");

mod tests {
    use super::etag_list_contains;

    #[test]
    fn matches_a_single_tag() {
        assert!(etag_list_contains(br#""abc""#, br#""abc""#, false));
        assert!(!etag_list_contains(br#""abc""#, br#""abd""#, false));
    }

    #[test]
    fn matches_inside_a_list() {
        let list = br#""aaa", "bbb", "ccc""#;
        assert!(etag_list_contains(list, br#""aaa""#, false));
        assert!(etag_list_contains(list, br#""bbb""#, false));
        assert!(etag_list_contains(list, br#""ccc""#, false));
        assert!(!etag_list_contains(list, br#""ddd""#, false));
    }

    #[test]
    fn a_prefix_is_not_a_match() {
        // "ab" must not match the entry "abc".
        assert!(!etag_list_contains(br#""abc""#, br#""ab"#, false));
    }

    #[test]
    fn weak_prefix_is_stripped_from_list_entries() {
        assert!(etag_list_contains(br#"W/"abc""#, br#""abc""#, true));
        assert!(!etag_list_contains(br#"W/"abc""#, br#""abc""#, false));
    }

    #[test]
    fn tolerates_padding_between_entries() {
        let list = br#""aaa" ,   "bbb""#;
        assert!(etag_list_contains(list, br#""bbb""#, false));
    }

    #[test]
    fn empty_list_matches_nothing() {
        assert!(!etag_list_contains(b"", br#""abc""#, false));
    }
}
