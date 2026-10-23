//! Line-level editing commands (⌘D, ⌘⌫, ⌥⇧↑/↓, ⌘/) as pure functions over the buffer text.
//! Each returns one replacement plus the selection to restore, so the result is a single undo step.

use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
    pub selection: Range<usize>,
}

/// Byte range of the full lines touched by `sel`, excluding the final newline.
fn line_block(text: &str, sel: &Range<usize>) -> Range<usize> {
    let start = text[..sel.start.min(text.len())].rfind('\n').map(|i| i + 1).unwrap_or(0);
    // A selection ending exactly at a line start does not include that line.
    let end_probe = if sel.end > sel.start && text[..sel.end].ends_with('\n') { sel.end - 1 } else { sel.end };
    let end = text[end_probe.min(text.len())..].find('\n').map(|i| end_probe + i).unwrap_or(text.len());
    start..end.max(start)
}

pub fn duplicate(text: &str, sel: Range<usize>) -> Edit {
    if sel.start != sel.end {
        let s = text[sel.clone()].to_string();
        let len = s.len();
        return Edit { range: sel.end..sel.end, text: s, selection: sel.end..sel.end + len };
    }
    let block = line_block(text, &sel);
    let line = &text[block.clone()];
    let col = sel.start - block.start;
    let insert = format!("\n{line}");
    let new_cursor = block.end + 1 + col;
    Edit { range: block.end..block.end, text: insert, selection: new_cursor..new_cursor }
}

pub fn delete_lines(text: &str, sel: Range<usize>) -> Edit {
    let block = line_block(text, &sel);
    let (range, cursor) = if block.end < text.len() {
        (block.start..block.end + 1, block.start)
    } else if block.start > 0 {
        (block.start - 1..block.end, text[..block.start - 1].rfind('\n').map(|i| i + 1).unwrap_or(0))
    } else {
        (block.clone(), 0)
    };
    Edit { range, text: String::new(), selection: cursor..cursor }
}

/// Swap the selected line block with the line above (`up`) or below. `None` at the buffer edge.
pub fn move_lines(text: &str, sel: Range<usize>, up: bool) -> Option<Edit> {
    let block = line_block(text, &sel);
    if up {
        if block.start == 0 {
            return None;
        }
        let prev_start = text[..block.start - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let prev = &text[prev_start..block.start - 1];
        let cur = &text[block.clone()];
        let delta = block.start - prev_start;
        Some(Edit {
            range: prev_start..block.end,
            text: format!("{cur}\n{prev}"),
            selection: sel.start - delta..sel.end - delta,
        })
    } else {
        if block.end >= text.len() {
            return None;
        }
        let next_end = text[block.end + 1..].find('\n').map(|i| block.end + 1 + i).unwrap_or(text.len());
        let next = &text[block.end + 1..next_end];
        let cur = &text[block.clone()];
        let delta = next.len() + 1;
        Some(Edit {
            range: block.start..next_end,
            text: format!("{next}\n{cur}"),
            selection: sel.start + delta..sel.end + delta,
        })
    }
}

/// IntelliJ "Comment with Line Comment": prefix at column 0, removed when every non-blank line has it.
pub fn toggle_comment(text: &str, sel: Range<usize>, prefix: &str) -> Edit {
    let block = line_block(text, &sel);
    let lines: Vec<&str> = text[block.clone()].split('\n').collect();
    let all_commented = lines.iter().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with(prefix));
    let mut out = Vec::with_capacity(lines.len());
    let mut first_delta: isize = 0;
    for (i, l) in lines.iter().enumerate() {
        let new = if all_commented {
            let indent = l.len() - l.trim_start().len();
            let rest = &l[indent..];
            if let Some(stripped) = rest.strip_prefix(prefix) {
                let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
                format!("{}{}", &l[..indent], stripped)
            } else {
                l.to_string()
            }
        } else if l.trim().is_empty() && lines.len() > 1 {
            l.to_string()
        } else {
            format!("{prefix} {l}")
        };
        if i == 0 {
            first_delta = new.len() as isize - l.len() as isize;
        }
        out.push(new);
    }
    let new_text = out.join("\n");
    let total_delta = new_text.len() as isize - (block.end - block.start) as isize;
    let shift = |o: usize, d: isize| (o as isize + d).max(block.start as isize) as usize;
    let selection = if sel.start == sel.end {
        // Like IntelliJ: caret moves to the next line after commenting a single line.
        let next = (block.end as isize + total_delta) as usize + 1;
        let next = next.min(text.len().saturating_add_signed(total_delta));
        next..next
    } else {
        shift(sel.start, first_delta)..shift(sel.end, total_delta)
    };
    Edit { range: block, text: new_text, selection }
}

/// Apply an edit to a String (used by tests and by callers without an editor).
pub fn apply(text: &str, e: &Edit) -> String {
    let mut s = String::with_capacity(text.len() + e.text.len());
    s.push_str(&text[..e.range.start]);
    s.push_str(&e.text);
    s.push_str(&text[e.range.end..]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "one\ntwo\nthree";

    #[test]
    fn duplicate_line() {
        let e = duplicate(T, 5..5);
        assert_eq!(apply(T, &e), "one\ntwo\ntwo\nthree");
        assert_eq!(e.selection, 9..9);
    }

    #[test]
    fn duplicate_selection() {
        let e = duplicate(T, 0..3);
        assert_eq!(apply(T, &e), "oneone\ntwo\nthree");
    }

    #[test]
    fn delete_middle_and_last() {
        assert_eq!(apply(T, &delete_lines(T, 5..5)), "one\nthree");
        assert_eq!(apply(T, &delete_lines(T, 10..10)), "one\ntwo");
    }

    #[test]
    fn move_up_down() {
        let up = move_lines(T, 5..5, true).unwrap();
        assert_eq!(apply(T, &up), "two\none\nthree");
        assert_eq!(up.selection, 1..1);
        let down = move_lines(T, 5..5, false).unwrap();
        assert_eq!(apply(T, &down), "one\nthree\ntwo");
        assert!(move_lines(T, 0..0, true).is_none());
        assert!(move_lines(T, 10..10, false).is_none());
    }

    #[test]
    fn comment_roundtrip() {
        let c = toggle_comment(T, 0..7, "//");
        let commented = apply(T, &c);
        assert_eq!(commented, "// one\n// two\nthree");
        let u = toggle_comment(&commented, 0..13, "//");
        assert_eq!(apply(&commented, &u), T);
    }
}
