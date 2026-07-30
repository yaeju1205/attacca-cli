//! Small formatting helpers with no dependencies on the rest of the crate.

/// The leading 8 characters of an id, which is what the UI shows in place of a full UUID.
pub fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_takes_the_first_eight_characters() {
        assert_eq!(short("abc12345def67890"), "abc12345");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
    }

    #[test]
    fn short_counts_characters_not_bytes() {
        assert_eq!(short("한글한글한글한글한글"), "한글한글한글한글");
    }
}
