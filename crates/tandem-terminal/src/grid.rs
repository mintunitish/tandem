//! Plain-data snapshot types.

/// 24-bit color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// `Rgb::hex(0x2B2B2B)`.
    pub const fn hex(v: u32) -> Self {
        Self { r: (v >> 16) as u8, g: (v >> 8) as u8, b: v as u8 }
    }

    pub const fn to_hex(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }
}

/// Cell attribute bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct CellFlags(pub u16);

impl CellFlags {
    pub const BOLD: Self = Self(1);
    pub const ITALIC: Self = Self(1 << 1);
    pub const UNDERLINE: Self = Self(1 << 2);
    pub const INVERSE: Self = Self(1 << 3);
    pub const DIM: Self = Self(1 << 4);
    pub const STRIKEOUT: Self = Self(1 << 5);
    pub const HIDDEN: Self = Self(1 << 6);
    /// This cell holds a double-width character; the next cell is a spacer.
    pub const WIDE: Self = Self(1 << 7);
    /// Placeholder cell to the right of a [`WIDE`](Self::WIDE) cell (draw nothing).
    pub const WIDE_SPACER: Self = Self(1 << 8);

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
    pub const fn bold(self) -> bool {
        self.contains(Self::BOLD)
    }
    pub const fn italic(self) -> bool {
        self.contains(Self::ITALIC)
    }
    pub const fn underline(self) -> bool {
        self.contains(Self::UNDERLINE)
    }
    pub const fn inverse(self) -> bool {
        self.contains(Self::INVERSE)
    }
}

/// One terminal cell. `fg`/`bg` are resolved RGB *before* applying [`CellFlags::INVERSE`];
/// use [`Cell::effective_colors`] to get the colors to paint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: CellFlags,
}

impl Cell {
    /// `(fg, bg)` with inverse video applied.
    pub fn effective_colors(&self) -> (Rgb, Rgb) {
        if self.flags.inverse() { (self.bg, self.fg) } else { (self.fg, self.bg) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
}

/// Cursor position relative to the snapshot (row 0 = first row of the snapshot).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    /// False when hidden by the app (DECTCEM) or when the cursor is outside the snapshot window
    /// (e.g. scrolled back).
    pub visible: bool,
}

#[derive(Clone, Debug)]
pub struct GridSnapshot {
    pub cols: usize,
    pub rows: usize,
    /// Row-major, `rows * cols` cells.
    pub cells: Vec<Cell>,
    pub cursor: Cursor,
    pub title: String,
    /// Number of lines currently in scrollback.
    pub scrollback_len: usize,
    /// How many lines the viewport is scrolled up into history (0 = following live output).
    pub display_offset: usize,
    /// Grid line of the first row of this snapshot (0 = top of the live screen, negative =
    /// scrollback).
    pub top_line: i32,
    /// Default background, for painting padding / empty areas.
    pub default_bg: Rgb,
    pub default_fg: Rgb,
}

impl GridSnapshot {
    pub fn row(&self, row: usize) -> &[Cell] {
        &self.cells[row * self.cols..(row + 1) * self.cols]
    }

    pub fn cell(&self, row: usize, col: usize) -> &Cell {
        &self.cells[row * self.cols + col]
    }

    /// Text of one row with trailing blanks trimmed; wide-char spacers are skipped.
    pub fn row_text(&self, row: usize) -> String {
        let mut s: String = self
            .row(row)
            .iter()
            .filter(|c| !c.flags.contains(CellFlags::WIDE_SPACER))
            .map(|c| c.ch)
            .collect();
        s.truncate(s.trim_end().len());
        s
    }

    /// All rows joined with `\n`, trailing blank rows removed.
    pub fn text(&self) -> String {
        let mut lines: Vec<String> = (0..self.rows).map(|r| self.row_text(r)).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        lines.join("\n")
    }
}
