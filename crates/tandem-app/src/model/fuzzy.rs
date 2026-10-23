//! Tiny subsequence fuzzy scorer for Search Everywhere. Swap for `tandem_project::fuzzy` later.

/// Score `candidate` against `query` (case-insensitive subsequence). Higher is better.
/// Returns `None` when `query` is not a subsequence. Matches in the file name (after the last
/// `/`), at word starts and in runs score higher; shorter candidates win ties.
pub fn score(query: &str, candidate: &str) -> Option<(i64, Vec<usize>)> {
    if query.is_empty() {
        return Some((0, vec![]));
    }
    let cand: Vec<char> = candidate.chars().collect();
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).flat_map(|c| c.to_lowercase()).collect();
    let name_start = candidate.rfind('/').map(|i| candidate[..=i].chars().count()).unwrap_or(0);

    // Prefer matching inside the file name: try a greedy match starting at the name first.
    let attempt = |from: usize| -> Option<Vec<usize>> {
        let mut idx = Vec::with_capacity(q.len());
        let mut qi = 0;
        for (i, c) in cand.iter().enumerate().skip(from) {
            if qi < q.len() && c.to_lowercase().next() == Some(q[qi]) {
                idx.push(i);
                qi += 1;
            }
        }
        (qi == q.len()).then_some(idx)
    };
    let positions = attempt(name_start).or_else(|| attempt(0))?;

    let mut s: i64 = 0;
    let mut prev: Option<usize> = None;
    for &p in &positions {
        let c = cand[p];
        let at_word_start = p == 0
            || matches!(cand[p - 1], '/' | '_' | '-' | '.' | ' ')
            || (c.is_uppercase() && cand[p - 1].is_lowercase());
        s += 10;
        if p >= name_start {
            s += 15;
        }
        if at_word_start {
            s += 20;
        }
        if let Some(pp) = prev {
            if p == pp + 1 {
                s += 15;
            } else {
                s -= (p - pp) as i64;
            }
        }
        prev = Some(p);
    }
    if positions.first() == Some(&name_start) {
        s += 25;
    }
    s -= cand.len() as i64 / 4;
    Some((s, positions))
}

#[cfg(test)]
mod tests {
    use super::score;

    #[test]
    fn file_name_beats_directory() {
        let a = score("main", "src/main.rs").unwrap().0;
        let b = score("main", "mainframe/x/lib.rs").unwrap().0;
        assert!(a > b);
    }

    #[test]
    fn non_subsequence_is_none() {
        assert!(score("xyz", "src/main.rs").is_none());
    }

    #[test]
    fn camel_humps() {
        assert!(score("SE", "SearchEverywhere").is_some());
    }
}
