//! Color palettes and color resolution.

use crate::grid::Rgb;

/// Default fg/bg/cursor plus the 16 ANSI colors. Indexed colors 16..=255 (6x6x6 cube and gray
/// ramp) are computed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    /// 0-7 normal, 8-15 bright.
    pub ansi: [Rgb; 16],
}

impl Default for Palette {
    fn default() -> Self {
        darcula()
    }
}

impl Palette {
    /// Resolve an xterm 256-color index.
    pub fn indexed(&self, idx: u8) -> Rgb {
        match idx {
            0..=15 => self.ansi[idx as usize],
            16..=231 => {
                let i = idx - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
                Rgb::new(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            232..=255 => {
                let v = 8 + 10 * (idx - 232);
                Rgb::new(v, v, v)
            }
        }
    }

    /// Dimmed variant (2/3 intensity), used for `NamedColor::Dim*`.
    pub fn dim(c: Rgb) -> Rgb {
        let f = |v: u8| (v as u16 * 2 / 3) as u8;
        Rgb::new(f(c.r), f(c.g), f(c.b))
    }
}

/// JetBrains Darcula terminal colors.
pub fn darcula() -> Palette {
    Palette {
        foreground: Rgb::hex(0xBBBBBB),
        background: Rgb::hex(0x2B2B2B),
        cursor: Rgb::hex(0xBBBBBB),
        ansi: [
            Rgb::hex(0x000000),
            Rgb::hex(0xFF6B68),
            Rgb::hex(0xA8C023),
            Rgb::hex(0xD6BF55),
            Rgb::hex(0x5394EC),
            Rgb::hex(0xAE8ABE),
            Rgb::hex(0x299999),
            Rgb::hex(0xBBBBBB),
            Rgb::hex(0x555555),
            Rgb::hex(0xFF8785),
            Rgb::hex(0xA8C023),
            Rgb::hex(0xFFFF00),
            Rgb::hex(0x7EAEF1),
            Rgb::hex(0xFF99FF),
            Rgb::hex(0x6CDADA),
            Rgb::hex(0xFFFFFF),
        ],
    }
}

/// IntelliJ Light terminal colors.
pub fn intellij_light() -> Palette {
    Palette {
        foreground: Rgb::hex(0x000000),
        background: Rgb::hex(0xFFFFFF),
        cursor: Rgb::hex(0x000000),
        ansi: [
            Rgb::hex(0x000000),
            Rgb::hex(0xC91B00),
            Rgb::hex(0x00A600),
            Rgb::hex(0x999900),
            Rgb::hex(0x0225C7),
            Rgb::hex(0xC930C7),
            Rgb::hex(0x00A5A5),
            Rgb::hex(0xBFBFBF),
            Rgb::hex(0x686868),
            Rgb::hex(0xFF6E67),
            Rgb::hex(0x00C200),
            Rgb::hex(0xC7C400),
            Rgb::hex(0x6871FF),
            Rgb::hex(0xFF77FF),
            Rgb::hex(0x00C5C7),
            Rgb::hex(0xFFFFFF),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_cube_and_gray() {
        let p = darcula();
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(p.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(p.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(p.indexed(1), p.ansi[1]);
    }
}
