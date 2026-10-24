//! Application bootstrap: fonts, theme, keymap, menus, the main window, and the
//! `--screenshot` mode (built with `--features test-support`) used for docs/screenshots.

use crate::assets::{self, AppAssets};
use crate::keymap::{self, OpenSettings, Quit, ToggleTheme};
use crate::state::{AppSettings, WindowGeometry};
use crate::stripes::ToolWindowId;
use crate::theme::{self, ThemeKind};
use crate::workspace::Workspace;
use gpui_kit::component::TitleBar;
use gpui_kit::*;
use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
pub struct Args {
    pub project: Option<PathBuf>,
    pub open: Vec<PathBuf>,
    pub show: Vec<ToolWindowId>,
    pub theme: Option<ThemeKind>,
    pub screenshot: Option<PathBuf>,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut out = Args::default();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--open" => out.open.push(value("--open")?.into()),
            "--screenshot" => out.screenshot = Some(value("--screenshot")?.into()),
            "--theme" => {
                out.theme = Some(match value("--theme")?.as_str() {
                    "dark" | "darcula" => ThemeKind::Darcula,
                    "light" => ThemeKind::Light,
                    other => return Err(format!("unknown theme {other}")),
                })
            }
            "--show" => {
                let v = value("--show")?;
                let id = ToolWindowId::all().find(|t| t.title().eq_ignore_ascii_case(&v)).ok_or_else(|| format!("unknown tool window {v}"))?;
                out.show.push(id);
            }
            "-h" | "--help" => return Err(USAGE.into()),
            s if s.starts_with('-') => return Err(format!("unknown flag {s}\n{USAGE}")),
            _ => out.project = Some(a.into()),
        }
    }
    Ok(out)
}

const USAGE: &str = "usage: tandem [PROJECT] [--open FILE]... [--show TOOLWINDOW]... [--theme dark|light] [--screenshot OUT.png]";

/// Everything a window needs: components, fonts, settings, theme, key bindings.
pub fn init(theme_override: Option<ThemeKind>, cx: &mut App) {
    gpui_kit::init(cx);
    assets::load_fonts(cx);
    let mut settings = AppSettings::load();
    if let Some(t) = theme_override {
        settings.theme = t;
    }
    theme::apply(settings.theme, settings.editor_font_size, cx);
    cx.set_global(settings);
    keymap::bind_keys(cx);
    crate::editor::actions::bind_keys(cx);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &ToggleTheme, cx| crate::settings::toggle_theme(cx));
    cx.set_menus(vec![
        Menu::new("Tandem").items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit Tandem", Quit),
        ]),
    ]);
}

pub fn window_options(cx: &App) -> WindowOptions {
    let mut titlebar = TitleBar::title_bar_options();
    titlebar.traffic_light_position = Some(point(px(14.), px(13.)));
    let bounds = match WindowGeometry::load() {
        Some(g) if g.width >= 640.0 && g.height >= 400.0 => {
            let b = Bounds::new(point(px(g.x), px(g.y)), size(px(g.width), px(g.height)));
            if g.maximized { WindowBounds::Maximized(b) } else { WindowBounds::Windowed(b) }
        }
        _ => WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx)),
    };
    WindowOptions {
        titlebar: Some(titlebar),
        window_bounds: Some(bounds),
        window_min_size: Some(size(px(800.), px(500.))),
        app_id: Some("dev.tandem.ide".into()),
        ..Default::default()
    }
}

pub fn open_workspace(project: PathBuf, options: WindowOptions, cx: &mut App) -> anyhow::Result<(AnyWindowHandle, Entity<Workspace>)> {
    gpui_kit::open_window(options, cx, |window, cx| {
        let ws = cx.new(|cx| Workspace::new(project, window, cx));
        let f = ws.read(cx).focus_handle(cx);
        window.focus(&f, cx);
        ws
    })
}

pub fn run() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    let project = args.project.clone().unwrap_or_else(default_project);
    remember_project(&project);
    gpui_kit::application().with_assets(AppAssets).run(move |cx| {
        init(args.theme, cx);
        let mut options = window_options(cx);
        if args.screenshot.is_some() {
            options.window_bounds = Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1440.), px(900.)), cx)));
        }
        let (handle, ws) = match open_workspace(project, options, cx) {
            Ok(v) => v,
            Err(err) => {
                eprintln!("tandem: cannot open window: {err}");
                cx.quit();
                return;
            }
        };
        cx.activate(true);
        let _ = handle.update(cx, |_, window, cx| {
            ws.update(cx, |ws, cx| {
                for f in &args.open {
                    ws.open_file(f.clone(), window, cx);
                }
                for id in &args.show {
                    if !ws.is_open(*id) {
                        ws.show(*id, window, cx);
                    }
                }
            })
        });
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        if let Some(out) = args.screenshot.clone() {
            screenshot(handle, out, cx);
        }
    });
}

#[cfg(feature = "test-support")]
fn screenshot(handle: AnyWindowHandle, out: PathBuf, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor().timer(std::time::Duration::from_millis(2500)).await;
        let _ = cx.update_window(handle, |_, window, cx| {
            window.refresh();
            let _ = window.draw(cx);
            match window.render_to_image() {
                Ok(img) => match img.save(&out) {
                    Ok(()) => eprintln!("tandem: wrote {}", out.display()),
                    Err(err) => eprintln!("tandem: cannot write {}: {err}", out.display()),
                },
                Err(err) => eprintln!("tandem: render_to_image failed: {err}"),
            }
        });
        cx.update(|cx| cx.quit());
    })
    .detach();
}

#[cfg(not(feature = "test-support"))]
fn screenshot(_: AnyWindowHandle, _: PathBuf, cx: &mut App) {
    eprintln!("tandem: --screenshot needs a build with `--features test-support`");
    cx.quit();
}
/// Project to open when none is given: the last one opened, else the current
/// directory, unless that is `/` or `$HOME` (Finder launches start at `/`), in
/// which case fall back to `$HOME`.
fn default_project() -> PathBuf {
    let last = crate::state::tandem_home().join("last-project");
    if let Ok(s) = std::fs::read_to_string(&last) {
        let p = PathBuf::from(s.trim());
        if p.is_dir() { return p; }
    }
    let home = tandem_core::fs::home();
    match std::env::current_dir() {
        Ok(cwd) if cwd != std::path::Path::new("/") && cwd != home => cwd,
        _ => home,
    }
}

fn remember_project(p: &std::path::Path) {
    let home = crate::state::tandem_home();
    let _ = std::fs::create_dir_all(&home);
    let _ = std::fs::write(home.join("last-project"), p.to_string_lossy().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn parses_flags() {
        let a = parse_args(["/p", "--open", "src/a.rs", "--show", "agents", "--theme", "light"].map(String::from)).unwrap();
        assert_eq!(a.project, Some(PathBuf::from("/p")));
        assert_eq!(a.show, vec![ToolWindowId::Agents]);
        assert_eq!(a.theme, Some(ThemeKind::Light));
        assert!(parse_args(["--bogus".to_string()]).is_err());
    }
}
