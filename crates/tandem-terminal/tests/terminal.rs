use std::time::{Duration, Instant};

use tandem_terminal::{
    CellFlags, Key, KeyInput, Mods, SpawnOptions, TermEvent, Terminal, palette,
};

const T: Duration = Duration::from_secs(10);

fn sh(script: &str) -> Terminal {
    Terminal::spawn(SpawnOptions {
        shell: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        ..Default::default()
    })
    .expect("spawn")
}

/// Poll `f` until it returns true (tests only).
fn eventually(mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + T;
    while Instant::now() < deadline {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("condition not met within {T:?}");
}

#[test]
fn hello_and_exit_code() {
    let t = sh(r#"printf "hello\n"; exit 3"#);
    assert!(t.pid().is_some());
    assert_eq!(t.wait_exit(T), Some(3));
    let snap = t.snapshot();
    assert!(snap.text().contains("hello"), "got {:?}", snap.text());
    assert_eq!(snap.row_text(0), "hello");
    let rx = t.events();
    let mut events = Vec::new();
    while let Ok(e) = rx.try_recv() {
        events.push(e);
    }
    assert!(events.contains(&TermEvent::Exit(3)), "{events:?}");
    assert!(events.contains(&TermEvent::Wakeup));
    assert_eq!(t.exit_status(), Some(3));
}

#[test]
fn resize_updates_grid() {
    let t = Terminal::spawn(SpawnOptions { shell: Some("/bin/cat".into()), cols: 80, rows: 24, ..Default::default() })
        .unwrap();
    let s = t.snapshot();
    assert_eq!((s.cols, s.rows, s.cells.len()), (80, 24, 80 * 24));
    t.resize(100, 30).unwrap();
    let s = t.snapshot();
    assert_eq!((s.cols, s.rows, s.cells.len()), (100, 30, 100 * 30));
    assert_eq!(t.size(), (100, 30));
    t.kill().unwrap();
    assert!(t.wait_exit(T).is_some());
}

#[test]
fn utf8_and_emoji() {
    let t = sh(r#"printf 'a😀b héllo 日本\n'"#);
    t.wait_exit(T).unwrap();
    let s = t.snapshot();
    assert_eq!(s.row_text(0), "a😀b héllo 日本");
    // Emoji is double-width: wide cell followed by a spacer.
    assert_eq!(s.cell(0, 1).ch, '😀');
    assert!(s.cell(0, 1).flags.contains(CellFlags::WIDE));
    assert!(s.cell(0, 2).flags.contains(CellFlags::WIDE_SPACER));
    assert_eq!(s.cell(0, 3).ch, 'b');
    assert_eq!(t.find("héllo"), vec![(0, 5)]);
}

#[test]
fn interactive_echo_roundtrip() {
    let t = Terminal::spawn(SpawnOptions { shell: Some("/bin/cat".into()), ..Default::default() }).unwrap();
    t.write("ping 🚀\n".as_bytes()).unwrap();
    eventually(|| t.snapshot().text().matches("ping 🚀").count() >= 2);
    // Ctrl-D ends cat.
    t.send_key(KeyInput::new(Key::Char('d'), Mods::CTRL)).unwrap();
    assert_eq!(t.wait_exit(T), Some(0));
}

#[test]
fn colors_title_and_cursor() {
    let t = sh(r#"printf '\033]0;my title\007\033[1;31mred\033[0m \033[7minv\033[0m plain'"#);
    t.wait_exit(T).unwrap();
    let s = t.snapshot();
    let pal = palette::darcula();
    assert_eq!(s.title, "my title");
    let r = s.cell(0, 0);
    assert_eq!(r.ch, 'r');
    assert_eq!(r.fg, pal.ansi[1]);
    assert!(r.flags.bold());
    let i = s.cell(0, 4);
    assert!(i.flags.inverse());
    assert_eq!(i.effective_colors(), (pal.background, pal.foreground));
    assert_eq!(s.cell(0, 8).fg, pal.foreground);
    assert_eq!(s.cell(0, 8).bg, pal.background);
    assert!(s.cursor.visible);
    assert_eq!((s.cursor.row, s.cursor.col), (0, 13));

    t.set_palette(palette::intellij_light());
    let s = t.snapshot();
    assert_eq!(s.cell(0, 0).fg, palette::intellij_light().ansi[1]);
    assert_eq!(s.default_bg, palette::intellij_light().background);
}

#[test]
fn truecolor_and_256() {
    let t = sh(r#"printf '\033[38;2;1;2;3mA\033[48;5;196mB'"#);
    t.wait_exit(T).unwrap();
    let s = t.snapshot();
    assert_eq!((s.cell(0, 0).fg.r, s.cell(0, 0).fg.g, s.cell(0, 0).fg.b), (1, 2, 3));
    assert_eq!(s.cell(0, 1).bg.to_hex(), 0xFF0000);
}

#[test]
fn modes_and_paste() {
    let t = sh(r#"printf '\033[?1h\033[?2004h'"#);
    t.wait_exit(T).unwrap();
    let m = t.modes();
    assert!(m.app_cursor && m.bracketed_paste);
    assert_eq!(tandem_terminal::encode_key(KeyInput::plain(Key::Up), m.app_cursor), b"\x1bOA");
    assert_eq!(tandem_terminal::encode_paste("x", m.bracketed_paste), b"\x1b[200~x\x1b[201~");
}

#[test]
fn scrollback_after_200_lines() {
    let t = Terminal::spawn(SpawnOptions {
        shell: Some("/bin/sh".into()),
        args: vec!["-c".into(), "i=0; while [ $i -lt 200 ]; do echo line$i; i=$((i+1)); done".into()],
        rows: 24,
        ..Default::default()
    })
    .unwrap();
    t.wait_exit(T).unwrap();

    let live = t.snapshot();
    assert_eq!(live.display_offset, 0);
    assert!(live.scrollback_len >= 200 - 24, "scrollback {}", live.scrollback_len);
    assert!(live.text().contains("line199"));
    assert!(!live.text().contains("line0\n"));

    // Oldest scrollback line.
    let top = t.snapshot_range(-(live.scrollback_len as i32), 5);
    assert_eq!(top.rows, 5);
    assert_eq!(top.row_text(0), "line0");
    assert_eq!(top.row_text(4), "line4");

    // Scrolling moves the viewport.
    t.scroll(10);
    let up = t.snapshot();
    assert_eq!(up.display_offset, 10);
    assert_eq!(up.top_line, -10);
    assert!(!up.cursor.visible || up.cursor.row < up.rows);
    assert!(!up.text().contains("line199"));
    t.scroll(-1000);
    assert_eq!(t.snapshot().display_offset, 0);
    t.scroll(100_000);
    assert_eq!(t.snapshot().display_offset, live.scrollback_len);
    t.scroll_to_bottom();

    // Search spans scrollback and screen.
    let hits = t.find("line150");
    assert_eq!(hits.len(), 1);
    assert!(hits[0].0 < 0, "line150 should be in scrollback: {hits:?}");
    assert_eq!(hits[0].1, 0);
    assert_eq!(t.find("line0")[0], (-(live.scrollback_len as i32), 0));
    let all = t.find("line19");
    assert_eq!(all.len(), 11); // line19, line190..line199
    assert!(all.windows(2).all(|w| w[0].0 < w[1].0));
    assert!(t.find("LINE19").is_empty());
    assert_eq!(t.find_with("LINE19", false).len(), 11);
    assert!(t.find("").is_empty());

    // Out-of-range lines are blank.
    let beyond = t.snapshot_range(1000, 2);
    assert_eq!(beyond.text(), "");
}
