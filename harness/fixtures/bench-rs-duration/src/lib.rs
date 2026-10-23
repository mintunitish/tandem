/// Parses durations like "90s", "5m", "1h30m", "2h5m10s" into seconds.
/// Returns `None` for empty input, unknown units or a number without a unit.
pub fn parse_duration(s: &str) -> Option<u64> {
    let mut total = 0;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let n: u64 = num.parse().ok()?;
        total += match c {
            's' => n,
            'm' => n * 60,
            'h' => n * 60,
            _ => return None,
        };
        num.clear();
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::parse_duration;

    #[test]
    fn seconds_and_minutes() {
        assert_eq!(parse_duration("90s"), Some(90));
        assert_eq!(parse_duration("5m"), Some(300));
    }

    #[test]
    fn hours() {
        assert_eq!(parse_duration("1h30m"), Some(5400));
        assert_eq!(parse_duration("2h5m10s"), Some(7510));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("10"), None);
        assert_eq!(parse_duration("3d"), None);
    }
}
