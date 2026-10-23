//! Asset source: gpui-kit's default component icons + the Lucide icons we use + bundled fonts.

use gpui_kit::assets::{Assets, IconName};
use gpui_kit::component::Icon;
use gpui_kit::*;
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(
    pub ExtraIcons,
    [
        Bell, Bookmark, Bot, Braces, Bug, Check, ChevronDown, ChevronRight, CircleAlert, CircleCheck,
        CircleDot, CircleStop, Code, Cpu, Database, EllipsisVertical, Eye, File, FileBraces, FileCode,
        FileCog, FileImage, FileLock, FileTerminal, FileText, FileType, FlaskConical, Folder,
        FolderOpen, Gauge, GitBranch, GitCommitHorizontal, GitCompare, GitMerge, GitPullRequest,
        Hammer, ListTree, Locate, Lock, MessageSquare, Minus, OctagonX, Package, Play, Plus,
        RefreshCw, RotateCcw, Search, Server, Settings, ShieldAlert, ShieldCheck, Sparkles, Square,
        SquareTerminal, Table, Terminal, TriangleAlert, Trophy, User, Users, X, Zap
    ]
);

pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = ExtraIcons.load(path)? {
            return Ok(Some(bytes));
        }
        Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths = Assets.list(path)?;
        paths.extend(ExtraIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

/// Register JetBrains Mono + Inter (OFL, bundled in `assets/fonts`).
pub fn load_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-Italic.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/JetBrainsMono-BoldItalic.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/Inter-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/Inter-Medium.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../../../assets/fonts/Inter-Bold.ttf")),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("tandem: failed to load bundled fonts: {err}");
    }
}

/// A 16px (or `size`) Lucide icon in `color`.
pub fn icon(name: IconName, size: f32, color: Hsla) -> Icon {
    Icon::new(name).size(px(size)).text_color(color)
}

/// File-type glyph and color for the Project tree, tabs and search results.
pub fn file_icon(name: &str) -> (IconName, Hsla) {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    let c = |v: u32| -> Hsla { rgb(v).into() };
    match (name, ext.as_str()) {
        ("Cargo.toml" | "Cargo.lock", _) => (IconName::Package, c(0xC77D55)),
        ("package.json" | "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock", _) => (IconName::Package, c(0xCB3837)),
        ("Dockerfile", _) => (IconName::Package, c(0x2496ED)),
        (_, "rs") => (IconName::FileCode, c(0xE57E3F)),
        (_, "ts" | "tsx" | "mts" | "cts") => (IconName::FileCode, c(0x3D8FD1)),
        (_, "js" | "jsx" | "mjs" | "cjs") => (IconName::FileCode, c(0xE8BF5A)),
        (_, "py") => (IconName::FileCode, c(0x4B8BBE)),
        (_, "go") => (IconName::FileCode, c(0x00ADD8)),
        (_, "java" | "kt" | "kts") => (IconName::FileCode, c(0xC77DBB)),
        (_, "swift" | "c" | "h" | "cpp" | "hpp" | "cc" | "rb" | "php" | "lua" | "zig") => (IconName::FileCode, c(0x8D9BC9)),
        (_, "json" | "jsonc") => (IconName::FileBraces, c(0xE8BF5A)),
        (_, "toml" | "yaml" | "yml" | "ini" | "conf" | "env") => (IconName::FileCog, c(0x8E9CB3)),
        (_, "md" | "mdx" | "txt" | "rst") => (IconName::FileText, c(0x6F9EF5)),
        (_, "html" | "htm" | "vue" | "svelte" | "astro") => (IconName::FileCode, c(0xE06C4C)),
        (_, "css" | "scss" | "sass" | "less") => (IconName::FileType, c(0x6F9EF5)),
        (_, "sh" | "bash" | "zsh" | "fish") => (IconName::FileTerminal, c(0x5FB865)),
        (_, "sql") => (IconName::Database, c(0xC4A35A)),
        (_, "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico") => (IconName::FileImage, c(0x5FB865)),
        (_, "lock") => (IconName::FileLock, c(0x868A91)),
        _ => (IconName::File, c(0x868A91)),
    }
}

/// Editor language (syntect token) for a path.
pub fn language_for(path: &std::path::Path) -> &'static str {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match (name, ext.as_str()) {
        ("Makefile" | "makefile", _) => "make",
        ("Dockerfile", _) => "dockerfile",
        (_, "rs") => "rust",
        (_, "ts" | "mts" | "cts") => "typescript",
        (_, "tsx") => "tsx",
        (_, "js" | "mjs" | "cjs" | "jsx") => "javascript",
        (_, "py" | "pyi") => "python",
        (_, "go") => "go",
        (_, "java") => "java",
        (_, "kt" | "kts") => "kotlin",
        (_, "swift") => "swift",
        (_, "c" | "h") => "c",
        (_, "cpp" | "cc" | "hpp" | "cxx") => "cpp",
        (_, "rb") => "ruby",
        (_, "php") => "php",
        (_, "json" | "jsonc") => "json",
        (_, "toml") => "toml",
        (_, "yaml" | "yml") => "yaml",
        (_, "md" | "mdx") => "markdown",
        (_, "html" | "htm") => "html",
        (_, "css" | "scss") => "css",
        (_, "sh" | "bash" | "zsh") => "bash",
        (_, "sql") => "sql",
        (_, "lua") => "lua",
        (_, "zig") => "zig",
        _ => "text",
    }
}

/// Human language name for the status bar.
pub fn language_display(lang: &str) -> &'static str {
    match lang {
        "rust" => "Rust",
        "typescript" => "TypeScript",
        "tsx" => "TypeScript JSX",
        "javascript" => "JavaScript",
        "python" => "Python",
        "go" => "Go",
        "java" => "Java",
        "kotlin" => "Kotlin",
        "swift" => "Swift",
        "c" => "C",
        "cpp" => "C++",
        "ruby" => "Ruby",
        "php" => "PHP",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" => "YAML",
        "markdown" => "Markdown",
        "html" => "HTML",
        "css" => "CSS",
        "bash" => "Shell Script",
        "sql" => "SQL",
        "lua" => "Lua",
        "zig" => "Zig",
        "make" => "Makefile",
        "dockerfile" => "Dockerfile",
        _ => "Plain text",
    }
}

/// Line-comment prefix for ⌘/.
pub fn line_comment(lang: &str) -> &'static str {
    match lang {
        "python" | "ruby" | "bash" | "toml" | "yaml" | "make" | "dockerfile" => "#",
        "sql" | "lua" => "--",
        _ => "//",
    }
}
