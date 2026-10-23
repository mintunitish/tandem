use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use parking_lot::{Condvar, Mutex, RwLock};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::grid::{Cell, CellFlags, Cursor, CursorShape, GridSnapshot, Rgb};
use crate::input::{self, KeyInput};
use crate::palette::Palette;

/// Notifications for the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TermEvent {
    /// Grid content changed; take a new snapshot. Coalesced: at most one is pending until the
    /// next `snapshot*` call.
    Wakeup,
    Title(String),
    Bell,
    /// The child exited (all of its output has been processed first).
    Exit(i32),
}

#[derive(Clone, Debug)]
pub struct SpawnOptions {
    /// Program to run. Default: `$SHELL`, else `/bin/zsh`.
    pub shell: Option<String>,
    pub args: Vec<String>,
    /// Working directory. Default: the current process's.
    pub cwd: Option<PathBuf>,
    /// Extra environment, applied on top of the inherited env and the `TERM` defaults.
    pub env: BTreeMap<String, String>,
    pub cols: u16,
    pub rows: u16,
    /// Maximum scrollback lines.
    pub scrollback: usize,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            shell: None,
            args: Vec::new(),
            cwd: None,
            env: BTreeMap::new(),
            cols: 80,
            rows: 24,
            scrollback: 10_000,
        }
    }
}

/// Terminal modes the UI needs for input handling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TermModes {
    /// DECCKM: arrows/Home/End use `ESC O x`.
    pub app_cursor: bool,
    /// DECSET 2004.
    pub bracketed_paste: bool,
    pub alt_screen: bool,
    /// Child requested mouse reporting (click, drag or motion).
    pub mouse_reporting: bool,
    pub sgr_mouse: bool,
    pub focus_reporting: bool,
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// State shared between the emulator proxy, the reader thread and the `Terminal` handle,
/// deliberately not containing the `Term` itself (the proxy lives inside it).
struct Side {
    tx: Sender<TermEvent>,
    wake_pending: AtomicBool,
    title: Mutex<String>,
    palette: RwLock<Palette>,
    writer: SharedWriter,
}

impl Side {
    fn wake(&self) {
        if !self.wake_pending.swap(true, Ordering::AcqRel) {
            let _ = self.tx.try_send(TermEvent::Wakeup);
        }
    }
}

#[derive(Clone)]
struct Proxy(Arc<Side>);

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let side = &self.0;
        match event {
            Event::Title(t) => {
                *side.title.lock() = t.clone();
                let _ = side.tx.try_send(TermEvent::Title(t));
            }
            Event::ResetTitle => {
                side.title.lock().clear();
                let _ = side.tx.try_send(TermEvent::Title(String::new()));
            }
            Event::Bell => {
                let _ = side.tx.try_send(TermEvent::Bell);
            }
            Event::Wakeup => side.wake(),
            // Replies to terminal queries (DSR, DA, ...).
            Event::PtyWrite(s) => {
                let mut w = side.writer.lock();
                let _ = w.write_all(s.as_bytes()).and_then(|()| w.flush());
            }
            Event::ColorRequest(idx, fmt) => {
                let p = side.palette.read();
                let rgb = match idx {
                    256 => p.foreground,
                    257 => p.background,
                    258 => p.cursor,
                    0..=255 => p.indexed(idx as u8),
                    _ => return,
                };
                let reply = fmt(alacritty_terminal::vte::ansi::Rgb { r: rgb.r, g: rgb.g, b: rgb.b });
                let mut w = side.writer.lock();
                let _ = w.write_all(reply.as_bytes()).and_then(|()| w.flush());
            }
            _ => {}
        }
    }
}

struct Size {
    cols: usize,
    rows: usize,
    history: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows + self.history
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct ExitState {
    code: Mutex<Option<i32>>,
    cv: Condvar,
}

/// A running terminal: child process on a PTY plus its emulated screen.
pub struct Terminal {
    term: Arc<Mutex<Term<Proxy>>>,
    side: Arc<Side>,
    exit: Arc<ExitState>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pid: Option<u32>,
    events: Receiver<TermEvent>,
}

impl Terminal {
    pub fn spawn(opts: SpawnOptions) -> Result<Terminal> {
        let cols = opts.cols.max(2);
        let rows = opts.rows.max(1);
        let size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };
        let pair = native_pty_system().openpty(size).context("open pty")?;

        let shell = opts
            .shell
            .clone()
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/zsh".to_string());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.args(&opts.args);
        // portable-pty defaults to $HOME when no cwd is set; inherit ours instead.
        if let Some(cwd) = opts.cwd.clone().or_else(|| std::env::current_dir().ok()) {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }

        let mut child = pair.slave.spawn_command(cmd).with_context(|| format!("spawn {shell}"))?;
        // Drop our slave handle so the master sees EOF when the child side closes.
        drop(pair.slave);
        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer: SharedWriter = Arc::new(Mutex::new(pair.master.take_writer().context("take pty writer")?));

        let (tx, rx) = async_channel::unbounded();
        let side = Arc::new(Side {
            tx,
            wake_pending: AtomicBool::new(false),
            title: Mutex::new(String::new()),
            palette: RwLock::new(Palette::default()),
            writer,
        });
        let config = Config { scrolling_history: opts.scrollback, ..Config::default() };
        let dims = Size { cols: cols as usize, rows: rows as usize, history: 0 };
        let term = Arc::new(Mutex::new(Term::new(config, &dims, Proxy(side.clone()))));
        let exit = Arc::new(ExitState { code: Mutex::new(None), cv: Condvar::new() });

        {
            let term = term.clone();
            let side = side.clone();
            let exit = exit.clone();
            std::thread::Builder::new()
                .name("tandem-terminal-reader".into())
                .spawn(move || {
                    let mut parser: Processor = Processor::new();
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                parser.advance(&mut *term.lock(), &buf[..n]);
                                side.wake();
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            // EIO on macOS/Linux once the slave side is closed.
                            Err(_) => break,
                        }
                    }
                    let code = match child.wait() {
                        Ok(status) => status.exit_code() as i32,
                        Err(e) => {
                            tracing::warn!("waiting for child failed: {e}");
                            -1
                        }
                    };
                    // Final repaint, then Exit; both before waiters are released.
                    let _ = side.tx.try_send(TermEvent::Wakeup);
                    let _ = side.tx.try_send(TermEvent::Exit(code));
                    *exit.code.lock() = Some(code);
                    exit.cv.notify_all();
                })
                .context("spawn reader thread")?;
        }

        Ok(Terminal { term, side, exit, master: Mutex::new(pair.master), killer: Mutex::new(killer), pid, events: rx })
    }

    /// Event stream for repaint/title/bell/exit. Receivers are clones of one channel: with
    /// several consumers each event is delivered to only one of them.
    pub fn events(&self) -> Receiver<TermEvent> {
        self.events.clone()
    }

    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Exit code if the child has exited (and all its output was processed).
    pub fn exit_status(&self) -> Option<i32> {
        *self.exit.code.lock()
    }

    /// Block until the child exits or `timeout` elapses.
    pub fn wait_exit(&self, timeout: Duration) -> Option<i32> {
        let deadline = Instant::now() + timeout;
        let mut code = self.exit.code.lock();
        while code.is_none() {
            if self.exit.cv.wait_until(&mut code, deadline).timed_out() {
                break;
            }
        }
        *code
    }

    /// Write raw bytes to the child (keyboard input, pasted text). Snaps the view back to the
    /// live screen if it was scrolled into history.
    pub fn write(&self, bytes: &[u8]) -> Result<()> {
        {
            let mut term = self.term.lock();
            if term.grid().display_offset() > 0 {
                term.scroll_display(Scroll::Bottom);
                self.side.wake();
            }
        }
        let mut w = self.side.writer.lock();
        w.write_all(bytes).context("write to pty")?;
        w.flush().context("flush pty")
    }

    /// Encode `key` using the current terminal modes and write it.
    pub fn send_key(&self, key: KeyInput) -> Result<()> {
        let bytes = input::encode_key(key, self.modes().app_cursor);
        if bytes.is_empty() { Ok(()) } else { self.write(&bytes) }
    }

    /// Paste text, using bracketed paste when the child enabled it.
    pub fn paste(&self, text: &str) -> Result<()> {
        self.write(&input::encode_paste(text, self.modes().bracketed_paste))
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        let cols = cols.max(2);
        let rows = rows.max(1);
        self.master
            .lock()
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .context("resize pty")?;
        {
            let mut term = self.term.lock();
            let history = term.history_size();
            term.resize(Size { cols: cols as usize, rows: rows as usize, history });
        }
        self.side.wake();
        Ok(())
    }

    /// Ask the child to terminate (SIGHUP on unix). The `Exit` event follows.
    pub fn kill(&self) -> Result<()> {
        self.killer.lock().kill().context("kill child")
    }

    pub fn modes(&self) -> TermModes {
        let m = *self.term.lock().mode();
        TermModes {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            mouse_reporting: m.intersects(TermMode::MOUSE_MODE),
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            focus_reporting: m.contains(TermMode::FOCUS_IN_OUT),
        }
    }

    pub fn set_palette(&self, palette: Palette) {
        *self.side.palette.write() = palette;
        self.side.wake();
    }

    pub fn palette(&self) -> Palette {
        self.side.palette.read().clone()
    }

    /// `(cols, rows)` of the live screen.
    pub fn size(&self) -> (usize, usize) {
        let t = self.term.lock();
        (t.columns(), t.screen_lines())
    }

    /// Scroll the viewport: positive `delta` moves up into scrollback, negative toward live
    /// output. Clamped to the available history.
    pub fn scroll(&self, delta: i32) {
        self.term.lock().scroll_display(Scroll::Delta(delta));
        self.side.wake();
    }

    pub fn scroll_to_bottom(&self) {
        self.term.lock().scroll_display(Scroll::Bottom);
        self.side.wake();
    }

    /// Snapshot of the viewport (honoring the scroll offset set by [`scroll`](Self::scroll)).
    pub fn snapshot(&self) -> GridSnapshot {
        self.side.wake_pending.store(false, Ordering::Release);
        let term = self.term.lock();
        let offset = term.grid().display_offset();
        build_snapshot(&term, &self.side, -(offset as i32), term.screen_lines())
    }

    /// Snapshot of `rows` lines starting at grid line `top_line` (0 = top of the live screen,
    /// negative = scrollback). Lines outside the buffer come back blank.
    pub fn snapshot_range(&self, top_line: i32, rows: usize) -> GridSnapshot {
        self.side.wake_pending.store(false, Ordering::Release);
        let term = self.term.lock();
        build_snapshot(&term, &self.side, top_line, rows)
    }

    /// Case-sensitive substring search over scrollback and screen. Returns `(line, col)` of
    /// each match start, `line` in grid coordinates (negative = scrollback), ordered top to
    /// bottom. Matches do not span wrapped lines.
    pub fn find(&self, query: &str) -> Vec<(i32, usize)> {
        self.find_with(query, true)
    }

    pub fn find_with(&self, query: &str, case_sensitive: bool) -> Vec<(i32, usize)> {
        let fold = |c: char| if case_sensitive { c } else { c.to_lowercase().next().unwrap_or(c) };
        let needle: Vec<char> = query.chars().map(fold).collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let term = self.term.lock();
        let grid = term.grid();
        let top = -(term.history_size() as i32);
        let bottom = term.screen_lines() as i32;
        let mut out = Vec::new();
        let mut hay: Vec<char> = Vec::new();
        let mut cols: Vec<usize> = Vec::new();
        for line in top..bottom {
            hay.clear();
            cols.clear();
            let row = &grid[Line(line)];
            for c in 0..term.columns() {
                let cell = &row[Column(c)];
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                hay.push(fold(cell.c));
                cols.push(c);
            }
            if hay.len() < needle.len() {
                continue;
            }
            for i in 0..=hay.len() - needle.len() {
                if hay[i..i + needle.len()] == needle[..] {
                    out.push((line, cols[i]));
                }
            }
        }
        out
    }

    /// Visible text of the live screen plus scrollback is available through snapshots; this is
    /// a convenience for the current viewport.
    pub fn viewport_text(&self) -> String {
        self.snapshot().text()
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if self.exit_status().is_none() {
            let _ = self.killer.get_mut().kill();
        }
    }
}

fn resolve_color(color: Color, term: &Term<Proxy>, palette: &Palette) -> Rgb {
    let colors = term.colors();
    match color {
        Color::Spec(c) => Rgb::new(c.r, c.g, c.b),
        Color::Indexed(i) => match colors[i as usize] {
            Some(c) => Rgb::new(c.r, c.g, c.b),
            None => palette.indexed(i),
        },
        Color::Named(n) => {
            if let Some(c) = colors[n as usize] {
                return Rgb::new(c.r, c.g, c.b);
            }
            let ansi = |i: usize| palette.ansi[i];
            let dim = |i: usize| Palette::dim(palette.ansi[i]);
            match n {
                NamedColor::Foreground | NamedColor::BrightForeground => palette.foreground,
                NamedColor::DimForeground => Palette::dim(palette.foreground),
                NamedColor::Background => palette.background,
                NamedColor::Cursor => palette.cursor,
                NamedColor::DimBlack => dim(0),
                NamedColor::DimRed => dim(1),
                NamedColor::DimGreen => dim(2),
                NamedColor::DimYellow => dim(3),
                NamedColor::DimBlue => dim(4),
                NamedColor::DimMagenta => dim(5),
                NamedColor::DimCyan => dim(6),
                NamedColor::DimWhite => dim(7),
                // Black..=BrightWhite are discriminants 0..=15.
                other => ansi(other as usize & 15),
            }
        }
    }
}

fn convert_flags(f: Flags) -> CellFlags {
    let mut out = CellFlags::default();
    let pairs = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::ALL_UNDERLINES, CellFlags::UNDERLINE),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::DIM, CellFlags::DIM),
        (Flags::STRIKEOUT, CellFlags::STRIKEOUT),
        (Flags::HIDDEN, CellFlags::HIDDEN),
        (Flags::WIDE_CHAR, CellFlags::WIDE),
        (Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER, CellFlags::WIDE_SPACER),
    ];
    for (from, to) in pairs {
        if f.intersects(from) {
            out.insert(to);
        }
    }
    out
}

fn build_snapshot(term: &Term<Proxy>, side: &Side, top_line: i32, rows: usize) -> GridSnapshot {
    let palette = side.palette.read();
    let cols = term.columns();
    let grid = term.grid();
    let first = -(term.history_size() as i32);
    let last = term.screen_lines() as i32 - 1;
    let blank = Cell { ch: ' ', fg: palette.foreground, bg: palette.background, flags: CellFlags::default() };

    let mut cells = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        let line = top_line + r as i32;
        if line < first || line > last {
            cells.extend(std::iter::repeat_n(blank, cols));
            continue;
        }
        let row = &grid[Line(line)];
        for c in 0..cols {
            let cell = &row[Column(c)];
            cells.push(Cell {
                ch: cell.c,
                fg: resolve_color(cell.fg, term, &palette),
                bg: resolve_color(cell.bg, term, &palette),
                flags: convert_flags(cell.flags),
            });
        }
    }

    let cur = grid.cursor.point;
    let style = term.cursor_style();
    let mut col = cur.column.0;
    if grid[cur.line][cur.column].flags.contains(Flags::WIDE_CHAR_SPACER) {
        col = col.saturating_sub(1);
    }
    use alacritty_terminal::vte::ansi::CursorShape as S;
    let (shape, hidden) = match style.shape {
        S::Block => (CursorShape::Block, false),
        S::Underline => (CursorShape::Underline, false),
        S::Beam => (CursorShape::Beam, false),
        S::HollowBlock => (CursorShape::HollowBlock, false),
        S::Hidden => (CursorShape::Block, true),
    };
    let rel = cur.line.0 - top_line;
    let in_window = rel >= 0 && (rel as usize) < rows;
    let cursor = Cursor {
        row: if in_window { rel as usize } else { 0 },
        col,
        shape,
        visible: in_window && !hidden && term.mode().contains(TermMode::SHOW_CURSOR),
    };

    GridSnapshot {
        cols,
        rows,
        cells,
        cursor,
        title: side.title.lock().clone(),
        scrollback_len: term.history_size(),
        display_offset: grid.display_offset(),
        top_line,
        default_bg: palette.background,
        default_fg: palette.foreground,
    }
}
