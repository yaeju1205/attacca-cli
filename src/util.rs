/// Shorten a UUID for display (UTF-8 safe).
pub fn short(s: &str) -> String {
    s.chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_short() {
        assert_eq!(short("abc12345def67890"), "abc12345");
        assert_eq!(short("abc"), "abc");
    }
}
