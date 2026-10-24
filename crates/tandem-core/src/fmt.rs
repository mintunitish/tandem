//! Tiny display helpers shared by the harness report and the app.

/// Compact token count: `950`, `12.3k`, `1.2M`.
pub fn fmt_tokens(t: u64) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1e6)
    } else if t >= 1_000 {
        format!("{:.1}k", t as f64 / 1e3)
    } else {
        t.to_string()
    }
}

/// First `n` chars of `s`, with `…` appended when it was cut.
pub fn ellipsize(s: &str, n: usize) -> std::borrow::Cow<'_, str> {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]).into(),
        None => s.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(fmt_tokens(950), "950");
        assert_eq!(fmt_tokens(12_345), "12.3k");
        assert_eq!(fmt_tokens(1_250_000), "1.2M");
        assert_eq!(ellipsize("héllo", 3), "hél…");
        assert_eq!(ellipsize("hi", 3), "hi");
        assert_eq!(ellipsize("abc", 3), "abc");
        assert_eq!(ellipsize("", 0), "");
        assert_eq!(ellipsize("ab", 0), "…");
        assert!(matches!(ellipsize("hi", 3), std::borrow::Cow::Borrowed(_)));
    }
}
