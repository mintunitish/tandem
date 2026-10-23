//! Keyboard / paste encoding (xterm defaults).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A printable character (already shifted/composed by the UI layer).
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// Function key 1..=12.
    F(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mods {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// macOS Command. Never sent to the child except for a few line-editing shortcuts
    /// (Cmd-Backspace, Cmd-Left/Right); every other Cmd combination encodes to nothing so the
    /// app can use it for its own shortcuts.
    pub cmd: bool,
}

impl Mods {
    pub const NONE: Mods = Mods { ctrl: false, alt: false, shift: false, cmd: false };
    pub const CTRL: Mods = Mods { ctrl: true, ..Self::NONE };
    pub const ALT: Mods = Mods { alt: true, ..Self::NONE };
    pub const SHIFT: Mods = Mods { shift: true, ..Self::NONE };
    pub const CMD: Mods = Mods { cmd: true, ..Self::NONE };

    /// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4). `1` means no modifiers.
    fn param(self) -> u8 {
        1 + self.shift as u8 + 2 * self.alt as u8 + 4 * self.ctrl as u8
    }
    fn any(self) -> bool {
        self.param() != 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub key: Key,
    pub mods: Mods,
}

impl KeyInput {
    pub fn new(key: Key, mods: Mods) -> Self {
        Self { key, mods }
    }
    pub fn plain(key: Key) -> Self {
        Self { key, mods: Mods::NONE }
    }
}

/// Encode a key press into the bytes to write to the PTY. `app_cursor_mode` is DECCKM
/// (`TermMode::APP_CURSOR`, see [`crate::Terminal::modes`]). Returns an empty vec for keys that
/// have no encoding.
pub fn encode_key(input: KeyInput, app_cursor_mode: bool) -> Vec<u8> {
    let KeyInput { key, mods } = input;

    if mods.cmd {
        return match key {
            Key::Backspace => vec![0x15], // ^U kill line
            Key::Left => vec![0x01],      // ^A
            Key::Right => vec![0x05],     // ^E
            _ => Vec::new(),
        };
    }

    match key {
        Key::Char(c) => encode_char(c, mods),
        Key::Enter => alt_prefix(mods, vec![b'\r']),
        Key::Tab => {
            if mods.shift {
                b"\x1b[Z".to_vec()
            } else {
                alt_prefix(mods, vec![b'\t'])
            }
        }
        Key::Backspace => {
            let b = if mods.ctrl { 0x08 } else { 0x7f };
            alt_prefix(mods, vec![b])
        }
        Key::Escape => alt_prefix(mods, vec![0x1b]),
        Key::Up => cursor_key(b'A', mods, app_cursor_mode),
        Key::Down => cursor_key(b'B', mods, app_cursor_mode),
        Key::Right => cursor_key(b'C', mods, app_cursor_mode),
        Key::Left => cursor_key(b'D', mods, app_cursor_mode),
        Key::Home => cursor_key(b'H', mods, app_cursor_mode),
        Key::End => cursor_key(b'F', mods, app_cursor_mode),
        Key::Insert => tilde_key(2, mods),
        Key::Delete => tilde_key(3, mods),
        Key::PageUp => tilde_key(5, mods),
        Key::PageDown => tilde_key(6, mods),
        Key::F(n) => match n {
            1..=4 => {
                let final_byte = b'P' + (n - 1);
                if mods.any() {
                    format!("\x1b[1;{}{}", mods.param(), final_byte as char).into_bytes()
                } else {
                    vec![0x1b, b'O', final_byte]
                }
            }
            5..=12 => {
                const CODES: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
                tilde_key(CODES[(n - 5) as usize], mods)
            }
            _ => Vec::new(),
        },
    }
}

fn alt_prefix(mods: Mods, mut bytes: Vec<u8>) -> Vec<u8> {
    if mods.alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn encode_char(c: char, mods: Mods) -> Vec<u8> {
    let mut out = Vec::with_capacity(5);
    if mods.alt {
        out.push(0x1b);
    }
    if mods.ctrl {
        let ctrl = match c {
            'a'..='z' => Some(c as u8 - b'a' + 1),
            'A'..='Z' => Some(c as u8 - b'A' + 1),
            ' ' | '@' | '2' => Some(0),
            '[' | '3' => Some(0x1b),
            '\\' | '4' => Some(0x1c),
            ']' | '5' => Some(0x1d),
            '^' | '6' => Some(0x1e),
            '_' | '/' | '7' => Some(0x1f),
            '?' | '8' => Some(0x7f),
            _ => None,
        };
        if let Some(b) = ctrl {
            out.push(b);
            return out;
        }
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    out
}

/// Arrows / Home / End: `CSI A`, or `SS3 A` in application cursor mode, or `CSI 1;m A` with
/// modifiers.
fn cursor_key(final_byte: u8, mods: Mods, app_cursor: bool) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[1;{}{}", mods.param(), final_byte as char).into_bytes()
    } else if app_cursor {
        vec![0x1b, b'O', final_byte]
    } else {
        vec![0x1b, b'[', final_byte]
    }
}

/// `CSI n ~` or `CSI n;m ~`.
fn tilde_key(code: u8, mods: Mods) -> Vec<u8> {
    if mods.any() {
        format!("\x1b[{};{}~", code, mods.param()).into_bytes()
    } else {
        format!("\x1b[{code}~").into_bytes()
    }
}

/// Encode pasted text. With `bracketed` (the child enabled DECSET 2004) the text is wrapped in
/// `ESC[200~ ... ESC[201~` with any embedded ESC bytes removed so the payload cannot terminate
/// the paste early. Otherwise line breaks become `\r` (what Enter sends), as xterm does.
pub fn encode_paste(text: &str, bracketed: bool) -> Vec<u8> {
    if bracketed {
        let mut out = Vec::with_capacity(text.len() + 12);
        out.extend_from_slice(b"\x1b[200~");
        out.extend(text.bytes().filter(|&b| b != 0x1b));
        out.extend_from_slice(b"\x1b[201~");
        out
    } else {
        text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(key: Key, mods: Mods) -> Vec<u8> {
        encode_key(KeyInput::new(key, mods), false)
    }

    #[test]
    fn key_table() {
        assert_eq!(k(Key::Char('c'), Mods::CTRL), vec![0x03]);
        assert_eq!(k(Key::Char('d'), Mods::CTRL), vec![0x04]);
        assert_eq!(k(Key::Char('['), Mods::CTRL), vec![0x1b]);
        assert_eq!(k(Key::Char(' '), Mods::CTRL), vec![0x00]);
        assert_eq!(k(Key::Char('b'), Mods::ALT), b"\x1bb");
        assert_eq!(k(Key::Char('a'), Mods::NONE), b"a");
        assert_eq!(k(Key::Char('é'), Mods::NONE), "é".as_bytes());
        assert_eq!(k(Key::Char('😀'), Mods::NONE), "😀".as_bytes());
        assert_eq!(k(Key::Tab, Mods::SHIFT), b"\x1b[Z");
        assert_eq!(k(Key::Tab, Mods::NONE), b"\t");
        assert_eq!(k(Key::Enter, Mods::NONE), b"\r");
        assert_eq!(k(Key::Backspace, Mods::NONE), vec![0x7f]);
        assert_eq!(k(Key::Backspace, Mods::ALT), b"\x1b\x7f");
        assert_eq!(k(Key::Escape, Mods::NONE), vec![0x1b]);
        assert_eq!(k(Key::Up, Mods::NONE), b"\x1b[A");
        assert_eq!(k(Key::Down, Mods::NONE), b"\x1b[B");
        assert_eq!(k(Key::Right, Mods::NONE), b"\x1b[C");
        assert_eq!(k(Key::Left, Mods::NONE), b"\x1b[D");
        assert_eq!(k(Key::Home, Mods::NONE), b"\x1b[H");
        assert_eq!(k(Key::End, Mods::NONE), b"\x1b[F");
        assert_eq!(k(Key::PageUp, Mods::NONE), b"\x1b[5~");
        assert_eq!(k(Key::PageDown, Mods::NONE), b"\x1b[6~");
        assert_eq!(k(Key::Delete, Mods::NONE), b"\x1b[3~");
        assert_eq!(k(Key::F(1), Mods::NONE), b"\x1bOP");
        assert_eq!(k(Key::F(4), Mods::NONE), b"\x1bOS");
        assert_eq!(k(Key::F(5), Mods::NONE), b"\x1b[15~");
        assert_eq!(k(Key::F(12), Mods::NONE), b"\x1b[24~");
        assert_eq!(k(Key::F(13), Mods::NONE), Vec::<u8>::new());
    }

    #[test]
    fn app_cursor_mode() {
        let up = KeyInput::plain(Key::Up);
        assert_eq!(encode_key(up, true), b"\x1bOA");
        assert_eq!(encode_key(up, false), b"\x1b[A");
        assert_eq!(encode_key(KeyInput::plain(Key::Home), true), b"\x1bOH");
        // Modifiers always use the CSI form.
        let ctrl_up = KeyInput::new(Key::Up, Mods::CTRL);
        assert_eq!(encode_key(ctrl_up, true), b"\x1b[1;5A");
    }

    #[test]
    fn modified_keys() {
        assert_eq!(k(Key::Left, Mods::ALT), b"\x1b[1;3D");
        assert_eq!(k(Key::Right, Mods { shift: true, ctrl: true, ..Mods::NONE }), b"\x1b[1;6C");
        assert_eq!(k(Key::Delete, Mods::CTRL), b"\x1b[3;5~");
        assert_eq!(k(Key::F(1), Mods::SHIFT), b"\x1b[1;2P");
        assert_eq!(k(Key::F(5), Mods::SHIFT), b"\x1b[15;2~");
    }

    #[test]
    fn cmd_is_swallowed() {
        assert!(k(Key::Char('c'), Mods::CMD).is_empty());
        assert_eq!(k(Key::Backspace, Mods::CMD), vec![0x15]);
        assert_eq!(k(Key::Left, Mods::CMD), vec![0x01]);
    }

    #[test]
    fn paste() {
        assert_eq!(encode_paste("a\nb", false), b"a\rb");
        assert_eq!(encode_paste("a\r\nb", false), b"a\rb");
        assert_eq!(encode_paste("a\nb", true), b"\x1b[200~a\nb\x1b[201~");
        assert_eq!(encode_paste("x\x1b[201~y", true), b"\x1b[200~x[201~y\x1b[201~");
    }
}
