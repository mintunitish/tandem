//! Built-in language server registry, detection and install checks.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One way to launch a server. `program` is a bare name (looked up on PATH plus common
/// install dirs) or an absolute path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCommand {
    pub program: String,
    pub args: Vec<String>,
}

impl ServerCommand {
    pub fn new(program: &str, args: &[&str]) -> Self {
        Self { program: program.to_string(), args: args.iter().map(|s| s.to_string()).collect() }
    }
}

/// File extension (without dot, lowercase) -> LSP `languageId`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileType {
    pub extension: String,
    pub language_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LanguageServerConfig {
    /// Stable id, e.g. "typescript", "python", "rust".
    pub id: String,
    pub name: String,
    /// Launch candidates in preference order; the first installed one wins.
    pub commands: Vec<ServerCommand>,
    pub file_types: Vec<FileType>,
    /// Files whose presence marks a project root for this server (e.g. `Cargo.toml`).
    pub root_markers: Vec<String>,
    pub install_hint: String,
    pub initialization_options: Option<serde_json::Value>,
    /// Extra environment for the server process.
    pub env: Vec<(String, String)>,
}

impl LanguageServerConfig {
    /// First launch candidate found on disk, with `program` resolved to an absolute path.
    pub fn resolve_command(&self) -> Option<ServerCommand> {
        self.resolve_commands().into_iter().next()
    }

    /// All launch candidates found on disk, in preference order. `LspClient::start`
    /// tries them in turn until one completes the handshake.
    pub fn resolve_commands(&self) -> Vec<ServerCommand> {
        self.commands
            .iter()
            .filter_map(|c| {
                which(&c.program).map(|p| ServerCommand {
                    program: p.to_string_lossy().into_owned(),
                    args: c.args.clone(),
                })
            })
            .collect()
    }

    /// Cheap check: some launch candidate exists and is executable. Does not run it.
    pub fn is_installed(&self) -> bool {
        self.resolve_command().is_some()
    }

    pub fn language_id_for(&self, path: &Path) -> Option<&str> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        self.file_types.iter().find(|f| f.extension == ext).map(|f| f.language_id.as_str())
    }

    pub fn handles(&self, path: &Path) -> bool {
        self.language_id_for(path).is_some()
    }
}

fn ft(pairs: &[(&str, &str)]) -> Vec<FileType> {
    pairs
        .iter()
        .map(|(e, l)| FileType { extension: e.to_string(), language_id: l.to_string() })
        .collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn server(
    id: &str,
    name: &str,
    commands: Vec<ServerCommand>,
    file_types: Vec<FileType>,
    root_markers: &[&str],
    install_hint: &str,
) -> LanguageServerConfig {
    LanguageServerConfig {
        id: id.into(),
        name: name.into(),
        commands,
        file_types,
        root_markers: strs(root_markers),
        install_hint: install_hint.into(),
        initialization_options: None,
        env: vec![],
    }
}

/// All servers Tandem knows how to launch.
pub fn builtin_servers() -> Vec<LanguageServerConfig> {
    vec![
        server(
            "typescript",
            "TypeScript/JavaScript",
            // typescript-language-server needs a TS <= 6 `tsserver.js` (workspace or
            // global); TypeScript 7 (native) ships its own LSP in `tsc`/`tsgo`.
            vec![
                ServerCommand::new("typescript-language-server", &["--stdio"]),
                ServerCommand::new("tsgo", &["--lsp", "--stdio"]),
                ServerCommand::new("tsc", &["--lsp", "--stdio"]),
            ],
            ft(&[
                ("ts", "typescript"), ("mts", "typescript"), ("cts", "typescript"),
                ("tsx", "typescriptreact"), ("js", "javascript"), ("mjs", "javascript"),
                ("cjs", "javascript"), ("jsx", "javascriptreact"),
            ]),
            &["tsconfig.json", "jsconfig.json", "package.json"],
            "npm i -g typescript-language-server typescript@5  (or TypeScript 7: npm i -g typescript)",
        ),
        server(
            "python",
            "Python (basedpyright / pyright)",
            vec![
                ServerCommand::new("basedpyright-langserver", &["--stdio"]),
                ServerCommand::new("pyright-langserver", &["--stdio"]),
            ],
            ft(&[("py", "python"), ("pyi", "python")]),
            &["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "Pipfile", "pyrightconfig.json"],
            "pip install basedpyright  (or: npm i -g pyright)",
        ),
        server(
            "rust",
            "Rust (rust-analyzer)",
            vec![ServerCommand::new("rust-analyzer", &[])],
            ft(&[("rs", "rust")]),
            &["Cargo.toml"],
            "rustup component add rust-analyzer",
        ),
        server(
            "go",
            "Go (gopls)",
            vec![ServerCommand::new("gopls", &[])],
            ft(&[("go", "go")]),
            &["go.mod", "go.work"],
            "go install golang.org/x/tools/gopls@latest",
        ),
        server(
            "json",
            "JSON (vscode-json-language-server)",
            vec![ServerCommand::new("vscode-json-language-server", &["--stdio"])],
            ft(&[("json", "json"), ("jsonc", "jsonc")]),
            &[],
            "npm i -g vscode-langservers-extracted",
        ),
        server(
            "yaml",
            "YAML (yaml-language-server)",
            vec![ServerCommand::new("yaml-language-server", &["--stdio"])],
            ft(&[("yaml", "yaml"), ("yml", "yaml")]),
            &[],
            "npm i -g yaml-language-server",
        ),
        server(
            "css",
            "CSS (vscode-css-language-server)",
            vec![ServerCommand::new("vscode-css-language-server", &["--stdio"])],
            ft(&[("css", "css"), ("scss", "scss"), ("less", "less")]),
            &[],
            "npm i -g vscode-langservers-extracted",
        ),
        server(
            "html",
            "HTML (vscode-html-language-server)",
            vec![ServerCommand::new("vscode-html-language-server", &["--stdio"])],
            ft(&[("html", "html"), ("htm", "html")]),
            &[],
            "npm i -g vscode-langservers-extracted",
        ),
    ]
}

/// Built-in server that handles `path`, by extension.
pub fn server_for_path(path: &Path) -> Option<LanguageServerConfig> {
    builtin_servers().into_iter().find(|c| c.handles(path))
}

const SKIP_DIRS: &[&str] = &["node_modules", "target", "dist", "build", "out", "vendor", "__pycache__", "venv"];
const MAX_SCAN_ENTRIES: usize = 5000;
const MAX_SCAN_DEPTH: usize = 3;

/// Servers relevant to the project at `root`: root markers present, or files with a
/// handled extension within a shallow scan (depth 3, max 5000 entries, skipping hidden
/// and build/dependency dirs). Order follows [`builtin_servers`].
pub fn detect(root: &Path) -> Vec<LanguageServerConfig> {
    let mut exts = std::collections::HashSet::new();
    let mut budget = MAX_SCAN_ENTRIES;
    scan(root, 0, &mut budget, &mut exts);
    builtin_servers()
        .into_iter()
        .filter(|c| {
            c.root_markers.iter().any(|m| root.join(m).exists())
                || c.file_types.iter().any(|f| exts.contains(&f.extension))
        })
        .collect()
}

fn scan(dir: &Path, depth: usize, budget: &mut usize, exts: &mut std::collections::HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue;
        }
        let Ok(ft) = entry.file_type() else { continue };
        let path = entry.path();
        if ft.is_dir() {
            if depth < MAX_SCAN_DEPTH && !SKIP_DIRS.contains(&name.as_ref()) {
                scan(&path, depth + 1, budget, exts);
            }
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            exts.insert(ext.to_ascii_lowercase());
        }
    }
}

/// Locate an executable: paths are checked directly; bare names are searched on `PATH` plus
/// common install dirs that GUI apps on macOS often lack.
pub fn which(program: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let home = tandem_core::fs::home();
    let extra = [PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")]
        .into_iter()
        .chain([".cargo/bin", "go/bin", ".local/bin", ".npm-global/bin", ".bun/bin", ".volta/bin"].map(|s| home.join(s)));
    for d in extra {
        if !dirs.contains(&d) {
            dirs.push(d);
        }
    }
    tandem_core::fs::which_in(program, &std::env::join_paths(dirs).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_language_ids() {
        let all = builtin_servers();
        let ids: Vec<_> = all.iter().map(|c| c.id.as_str()).collect();
        for id in ["typescript", "python", "rust", "go", "json", "yaml", "css", "html"] {
            assert!(ids.contains(&id), "{id}");
        }
        assert!(all.iter().all(|c| !c.install_hint.is_empty()));
        let ts = server_for_path(Path::new("/a/b.TSX")).expect("ts");
        assert_eq!(ts.language_id_for(Path::new("/a/b.tsx")), Some("typescriptreact"));
        let py = server_for_path(Path::new("x.py")).expect("py");
        assert_eq!(py.commands[1].program, "pyright-langserver");
        assert!(server_for_path(Path::new("x.unknown")).is_none());
    }

    #[test]
    fn detect_by_markers_and_extensions() {
        let dir = tempfile::tempdir().expect("tmp");
        std::fs::write(dir.path().join("Cargo.toml"), "").expect("w");
        std::fs::create_dir_all(dir.path().join("web/src")).expect("mk");
        std::fs::write(dir.path().join("web/src/app.ts"), "").expect("w");
        std::fs::create_dir_all(dir.path().join("node_modules/x")).expect("mk");
        std::fs::write(dir.path().join("node_modules/x/a.py"), "").expect("w");
        let ids: Vec<_> = detect(dir.path()).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["typescript", "rust"]);
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }
}
