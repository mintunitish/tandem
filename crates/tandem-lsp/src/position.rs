//! Conversion between UTF-8 byte offsets (editor buffers) and LSP positions
//! (zero-based line, UTF-16 code unit column). Lines end at `\n`; a `\r` before
//! `\n` is treated as part of the line terminator. Out-of-range input is clamped.

use lsp_types::{Position, Range};

/// Byte offset -> LSP position. `offset` is clamped to the text length and floored
/// to a char boundary.
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    let before = &text[..offset];
    let line = before.bytes().filter(|&b| b == b'\n').count();
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character: usize = text[line_start..offset].chars().map(char::len_utf16).sum();
    Position::new(line as u32, character as u32)
}

/// LSP position -> byte offset. A line past the end yields `text.len()`; a column past
/// the end of the line yields the line end (before `\r\n`); a column inside a surrogate
/// pair yields the start of that character.
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    let mut line_start = 0usize;
    for _ in 0..pos.line {
        match text[line_start..].find('\n') {
            Some(i) => line_start += i + 1,
            None => return text.len(),
        }
    }
    let mut line_end = text[line_start..].find('\n').map_or(text.len(), |i| line_start + i);
    if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
        line_end -= 1;
    }
    let mut units = 0u32;
    for (i, ch) in text[line_start..line_end].char_indices() {
        let next = units + ch.len_utf16() as u32;
        if next > pos.character {
            return line_start + i;
        }
        units = next;
    }
    line_end
}

pub fn range_to_offsets(text: &str, range: Range) -> std::ops::Range<usize> {
    let start = position_to_offset(text, range.start);
    let end = position_to_offset(text, range.end).max(start);
    start..end
}

pub fn offsets_to_range(text: &str, range: std::ops::Range<usize>) -> Range {
    Range::new(offset_to_position(text, range.start), offset_to_position(text, range.end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(l: u32, c: u32) -> Position {
        Position::new(l, c)
    }

    fn roundtrip_all(text: &str) {
        for (off, _) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            let pos = offset_to_position(text, off);
            assert_eq!(position_to_offset(text, pos), off, "offset {off} pos {pos:?} in {text:?}");
        }
    }

    #[test]
    fn ascii() {
        let t = "let a = 1;\nlet b = 2;\n";
        assert_eq!(offset_to_position(t, 0), p(0, 0));
        assert_eq!(offset_to_position(t, 4), p(0, 4));
        assert_eq!(offset_to_position(t, 11), p(1, 0));
        assert_eq!(offset_to_position(t, 15), p(1, 4));
        assert_eq!(offset_to_position(t, t.len()), p(2, 0));
        assert_eq!(position_to_offset(t, p(1, 4)), 15);
        assert_eq!(position_to_offset(t, p(0, 99)), 10, "clamped to line end");
        assert_eq!(position_to_offset(t, p(9, 0)), t.len(), "line past end");
        roundtrip_all(t);
    }

    #[test]
    fn emoji_surrogate_pairs() {
        // 😀 is 4 UTF-8 bytes, 2 UTF-16 units.
        let t = "a😀b\n😀😀x";
        assert_eq!(offset_to_position(t, 1), p(0, 1));
        assert_eq!(offset_to_position(t, 5), p(0, 3)); // 'b'
        assert_eq!(position_to_offset(t, p(0, 3)), 5);
        assert_eq!(position_to_offset(t, p(0, 2)), 1, "inside surrogate pair -> char start");
        assert_eq!(offset_to_position(t, 3), p(0, 1), "mid-char byte offset floors");
        assert_eq!(offset_to_position(t, t.len() - 1), p(1, 4));
        assert_eq!(position_to_offset(t, p(1, 4)), t.len() - 1);
        roundtrip_all(t);
    }

    #[test]
    fn cjk() {
        // CJK chars: 3 UTF-8 bytes, 1 UTF-16 unit.
        let t = "変数 = \"漢字\";\nconst 名前 = 1;";
        let off = t.find('=').expect("eq");
        assert_eq!(offset_to_position(t, off), p(0, 3));
        assert_eq!(position_to_offset(t, p(0, 3)), off);
        let name = t.find("名前").expect("name");
        assert_eq!(offset_to_position(t, name), p(1, 6));
        assert_eq!(position_to_offset(t, p(1, 7)), name + 3);
        roundtrip_all(t);
    }

    #[test]
    fn crlf() {
        let t = "ab\r\ncd";
        assert_eq!(offset_to_position(t, 4), p(1, 0));
        assert_eq!(position_to_offset(t, p(0, 10)), 2, "clamp before \\r\\n");
        assert_eq!(position_to_offset(t, p(1, 1)), 5);
    }

    #[test]
    fn ranges() {
        let t = "x😀y\nz";
        let r = offsets_to_range(t, 1..5);
        assert_eq!(r, Range::new(p(0, 1), p(0, 3)));
        assert_eq!(range_to_offsets(t, r), 1..5);
    }
}
