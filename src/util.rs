pub fn truncate_marked(s: &str, max_chars: usize) -> String {
    let mut out: String = s.chars().take(max_chars).collect();
    if s.chars().count() > max_chars {
        out.push_str(" ...[truncated]");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_input_unchanged() {
        assert_eq!(truncate_marked("abc", 5), "abc");
        assert_eq!(truncate_marked("abcde", 5), "abcde");
    }

    #[test]
    fn long_input_truncated_with_marker() {
        assert_eq!(truncate_marked("abcdef", 5), "abcde ...[truncated]");
    }
}
