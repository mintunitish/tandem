//! Risk classification of agent tool calls (deny-by-default).
//!
//! Every tool call gets a [`RiskClass`] and a human-readable reason before the UI or
//! policy sees it. Shell commands are parsed with [`crate::shell`] and *every*
//! sub-command (after `;`, `&&`, `||`, `|`, newlines, inside `$(...)`, backticks,
//! `sh -c` strings, `eval`, here-docs piped to shells, `xargs`/`sudo`/`env` wrappers,
//! decoded base64 payloads...) is evaluated; the worst class wins.
//!
//! The classifier is a policy layer, not a security boundary on its own: the Seatbelt
//! profile ([`crate::seatbelt`]) is the backstop for anything it cannot see (e.g. what
//! `npm test` actually runs).

use crate::paths::{self, Protected, Root};
use crate::shell::{self, Part, RawWord, RedirKind, Script, Warning};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tandem_core::{Policy, RiskClass, ToolCall, ToolKind};

use RiskClass::{Destructive, Review, Safe};

/// Hosts a `Fetch` tool call may reach without asking (documentation sites).
/// Matches the host exactly or any subdomain of it.
pub const DEFAULT_FETCH_ALLOW_HOSTS: &[&str] = &[
    "docs.rs",
    "doc.rust-lang.org",
    "rust-lang.github.io",
    "developer.mozilla.org",
    "docs.python.org",
    "nodejs.org",
    "docs.npmjs.com",
    "pkg.go.dev",
    "go.dev",
    "docs.github.com",
    "learn.microsoft.com",
    "readthedocs.io",
    "readthedocs.org",
    "docs.anthropic.com",
    "docs.claude.com",
    "platform.openai.com",
    "ai.google.dev",
    "typescriptlang.org",
    "react.dev",
    "kotlinlang.org",
    "docs.oracle.com",
    "en.cppreference.com",
    "man7.org",
    "postgresql.org",
    "dev.mysql.com",
    "sqlite.org",
];

pub struct Classifier {
    policy: Policy,
    project_root: PathBuf,
    worktree_root: PathBuf,
    home: PathBuf,
    fetch_allow_hosts: Vec<String>,
    project: Root,
    worktree: Root,
    temps: Vec<Root>,
    protected: Protected,
}

/// What a command writes to stdout, as far as the next pipeline stage is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Flow {
    None,
    /// Statically known text (echo/printf literal, here-doc, here-string).
    Literal(String),
    /// Bytes from the network (curl/wget).
    Fetch,
    /// Decoded/deobfuscated bytes (base64 -d, xxd -r...). `Some` when we could decode.
    Decoded(Option<String>),
    Unknown,
}

#[derive(Clone, Debug)]
struct Word {
    text: String,
    dynamic: bool,
    glob: bool,
    /// Produced by a substitution whose output came from the network / a decoder.
    tainted: bool,
}

impl Word {
    fn lit(s: &str) -> Self {
        Word { text: s.to_string(), dynamic: false, glob: false, tainted: false }
    }
    fn is_flag(&self) -> bool {
        !self.dynamic && self.text.starts_with('-') && self.text.len() > 1
    }
}

#[derive(Clone, Debug)]
struct Ctx {
    cwd: Option<PathBuf>,
    depth: usize,
    functions: BTreeSet<String>,
    /// Files written by curl/wget earlier in this script (download-then-execute).
    downloaded: BTreeSet<PathBuf>,
}

impl Ctx {
    fn child(&self) -> Ctx {
        Ctx { cwd: self.cwd.clone(), depth: self.depth + 1, functions: self.functions.clone(), downloaded: self.downloaded.clone() }
    }
}

struct Acc {
    class: RiskClass,
    reason: String,
}

impl Acc {
    fn new() -> Self {
        Acc { class: Safe, reason: String::new() }
    }
    fn add(&mut self, class: RiskClass, reason: impl Into<String>) {
        if class > self.class || (class == self.class && self.reason.is_empty()) {
            self.class = class;
            self.reason = reason.into();
        }
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).unwrap_or_else(|_| Regex::new("$^").expect("trivial regex")))
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "csh", "tcsh", "ash", "busybox"];
const INTERPRETERS: &[&str] = &[
    "python", "python2", "python3", "perl", "ruby", "node", "nodejs", "php", "deno", "lua", "osascript", "rscript",
    "tclsh", "awk", "gawk", "nawk", "jshell", "pwsh", "powershell",
];
const SQL_CLIENTS: &[&str] = &[
    "psql", "mysql", "mariadb", "sqlite3", "sqlite", "duckdb", "sqlcmd", "clickhouse-client", "clickhouse", "cockroach",
    "pgcli", "mycli", "litecli", "usql",
];
/// Read-only utilities. Path arguments are checked as reads.
const SAFE_READERS: &[&str] = &[
    "ls", "cat", "head", "tail", "wc", "grep", "egrep", "fgrep", "rg", "ag", "find", "tree", "stat", "file", "du",
    "sort", "cut", "diff", "cmp", "jq", "nl", "base64", "md5", "md5sum", "shasum", "sha256sum", "realpath", "readlink",
];
/// Commands that never touch files through their arguments.
const SAFE_PURE: &[&str] = &[
    "echo", "printf", "pwd", "which", "type", "true", "false", ":", "test", "[", "[[", "basename", "dirname", "whoami",
    "id", "uname", "sleep", "seq", "printenv", "tr", "date", "hostname", "nproc", "arch", "sw_vers", "cd", "pushd",
    "popd", "wait", "yes", "expr",
];

impl Classifier {
    /// Uses `$HOME` from the environment.
    pub fn new(policy: Policy, project_root: impl Into<PathBuf>, worktree_root: impl Into<PathBuf>) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/var/empty"));
        Self::with_home(policy, project_root, worktree_root, home)
    }

    pub fn with_home(
        policy: Policy,
        project_root: impl Into<PathBuf>,
        worktree_root: impl Into<PathBuf>,
        home: impl Into<PathBuf>,
    ) -> Self {
        let project_root = paths::normalize(&project_root.into());
        let worktree_root = paths::normalize(&worktree_root.into());
        let home = paths::normalize(&home.into());
        let protected = Protected::new(&home, &project_root, &policy.protected_paths);
        let mut temps = vec![Root::new(Path::new("/tmp")), Root::new(Path::new("/private/tmp"))];
        if let Some(t) = std::env::var_os("TMPDIR") {
            temps.push(Root::new(Path::new(&t)));
        }
        Classifier {
            project: Root::new(&project_root),
            worktree: Root::new(&worktree_root),
            temps,
            protected,
            policy,
            project_root,
            worktree_root,
            home,
            fetch_allow_hosts: DEFAULT_FETCH_ALLOW_HOSTS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Replace the documentation-host allow-list used for `Fetch` calls.
    pub fn with_fetch_allow_hosts(mut self, hosts: Vec<String>) -> Self {
        self.fetch_allow_hosts = hosts.into_iter().map(|h| h.to_lowercase()).collect();
        self
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
    pub fn worktree_root(&self) -> &Path {
        &self.worktree_root
    }

    /// Classify a tool call. Never panics; unknown shapes are `Review`.
    pub fn classify(&self, call: &ToolCall) -> (RiskClass, String) {
        let mut acc = Acc::new();
        match call.kind {
            ToolKind::Think => acc.add(Safe, "no side effects"),
            ToolKind::Read | ToolKind::Search => {
                let ps = self.call_paths(call);
                if ps.is_empty() {
                    if call.kind == ToolKind::Search {
                        acc.add(Safe, "search inside project");
                    } else {
                        acc.add(Review, "read without a reported location");
                    }
                }
                for p in ps {
                    let (c, r) = self.read_class(Some(&p));
                    acc.add(c, r);
                }
            }
            ToolKind::Edit | ToolKind::Move => {
                let ps = self.call_paths(call);
                if ps.is_empty() {
                    acc.add(Review, "edit without a reported location");
                }
                for p in ps {
                    let (c, r) = self.edit_class(&p);
                    acc.add(c, r);
                }
            }
            ToolKind::Delete => {
                let ps = self.call_paths(call);
                if ps.is_empty() {
                    acc.add(Review, "delete without a reported location");
                }
                for p in ps {
                    let (c, r) = self.edit_class(&p);
                    if c == Destructive {
                        acc.add(c, r);
                    } else if c == Safe {
                        acc.add(Review, format!("delete inside worktree: {}", p.display()));
                    } else {
                        acc.add(Destructive, format!("delete outside worktree: {}", p.display()));
                    }
                }
            }
            ToolKind::Execute => match command_text(call) {
                Some(cmd) => return self.classify_command(&cmd),
                None => acc.add(Review, "shell command not reported"),
            },
            ToolKind::Fetch => {
                let url = call
                    .raw_input
                    .as_deref()
                    .and_then(|r| json_string_field(r, &["url", "uri", "href"]).or_else(|| Some(r.trim().to_string())))
                    .filter(|s| !s.is_empty())
                    .or_else(|| call.locations.first().map(|p| p.to_string_lossy().into_owned()));
                match url {
                    Some(u) => {
                        let (c, r) = self.classify_url(&u);
                        acc.add(c, r);
                    }
                    None => acc.add(Review, "network fetch without URL"),
                }
            }
            ToolKind::Other => acc.add(Review, "unrecognized tool kind"),
        }
        finish(acc)
    }

    /// Classify a raw shell command string.
    pub fn classify_command(&self, cmd: &str) -> (RiskClass, String) {
        let mut acc = Acc::new();
        if cmd.trim().is_empty() {
            acc.add(Review, "empty command");
            return finish(acc);
        }
        if cmd.contains('\0') {
            acc.add(Review, "NUL byte in command");
        }
        let mut ctx = Ctx { cwd: Some(self.worktree_root.clone()), depth: 0, functions: BTreeSet::new(), downloaded: BTreeSet::new() };
        self.eval_script(cmd, &mut ctx, &mut acc);
        finish(acc)
    }

    /// Classify a URL for a network fetch.
    pub fn classify_url(&self, url: &str) -> (RiskClass, String) {
        let u = url.trim();
        let lower = u.to_lowercase();
        if lower.starts_with("file://") {
            let p = PathBuf::from(u.get(7..).unwrap_or(""));
            return self.read_class(Some(&p));
        }
        let rest = match lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")) {
            Some(r) => r,
            None => return (Review, format!("network fetch with unsupported scheme: {u}")),
        };
        let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or("");
        let hostport = authority.rsplit('@').next().unwrap_or("");
        let host = if hostport.starts_with('[') {
            hostport.split(']').next().unwrap_or("").trim_start_matches('[')
        } else {
            hostport.split(':').next().unwrap_or("")
        };
        let host = host.trim_end_matches('.');
        if host.is_empty() {
            return (Review, "network fetch: malformed URL".into());
        }
        let allowed = self
            .fetch_allow_hosts
            .iter()
            .any(|h| host == h || (host.len() > h.len() && host.ends_with(h.as_str()) && host.as_bytes()[host.len() - h.len() - 1] == b'.'));
        if allowed && !authority.contains('@') {
            (Safe, format!("fetch from documentation host {host}"))
        } else {
            (Review, format!("network fetch: {host}"))
        }
    }

    // ---------------------------------------------------------------- paths

    fn abs(&self, p: &Path, cwd: Option<&Path>) -> Option<PathBuf> {
        let s = p.to_string_lossy();
        let joined = if s == "~" {
            self.home.clone()
        } else if let Some(r) = s.strip_prefix("~/") {
            self.home.join(r)
        } else if s.starts_with('~') {
            return None; // ~user
        } else if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd?.join(p)
        };
        Some(paths::normalize(&joined))
    }

    fn display(&self, p: &Path) -> String {
        if let Ok(r) = p.strip_prefix(&self.home) {
            return format!("~/{}", r.display());
        }
        p.display().to_string()
    }

    fn is_control(&self, p: &Path) -> bool {
        // Tandem's own state (policy, pins) and git code-execution hooks.
        let tandem = self.project_root.join(".tandem");
        let in_tandem = paths::forms(p).iter().any(|f| paths::is_under(f, &tandem)) && !self.worktree.contains(p);
        let lower = p.to_string_lossy().to_lowercase();
        in_tandem || lower.contains("/.git/hooks") || lower.ends_with("/.git") || lower.ends_with("/.git/config") || lower.contains("/.git/config/")
    }

    fn all_forms_in(&self, root: &Root, p: &Path) -> bool {
        paths::forms(p).iter().all(|f| root.contains(f))
    }

    fn read_class(&self, p: Option<&Path>) -> (RiskClass, String) {
        let Some(p) = p else { return (Review, "read of unresolved path".into()) };
        let p = match self.abs(p, Some(&self.worktree_root)) {
            Some(p) => p,
            None => return (Review, format!("read of unresolved path {}", p.display())),
        };
        if self.protected.matches(&p) {
            return (Destructive, format!("secret path: {}", self.display(&p)));
        }
        if self.all_forms_in(&self.project, &p) {
            (Safe, format!("read inside project: {}", self.display(&p)))
        } else {
            (Review, format!("read outside project: {}", self.display(&p)))
        }
    }

    /// Edit/Move tool rule.
    fn edit_class(&self, p: &Path) -> (RiskClass, String) {
        let Some(p) = self.abs(p, Some(&self.worktree_root)) else {
            return (Destructive, format!("write to unresolved path {}", p.display()));
        };
        let d = self.display(&p);
        if self.protected.matches(&p) {
            return (Destructive, format!("secret path: {d}"));
        }
        if self.is_control(&p) {
            return (Destructive, format!("write to protected control file: {d}"));
        }
        if self.all_forms_in(&self.worktree, &p) {
            (Safe, format!("edit inside worktree: {d}"))
        } else if self.all_forms_in(&self.project, &p) {
            (Review, format!("edit outside worktree: {d}"))
        } else {
            (Destructive, format!("edit outside project: {d}"))
        }
    }

    /// Shell write (redirection, cp/mv/tee targets...).
    fn shell_write(&self, w: &Word, ctx: &Ctx) -> (RiskClass, String) {
        if w.dynamic {
            return (Review, format!("write to dynamic target {}", w.text));
        }
        let t = w.text.as_str();
        if matches!(t, "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty" | "/dev/fd/1" | "/dev/fd/2") {
            return (Safe, "write to stdio".into());
        }
        if t.starts_with("/dev/") {
            return (Destructive, format!("write to device {t}"));
        }
        let Some(p) = self.abs(Path::new(t), ctx.cwd.as_deref()) else {
            return (Review, format!("write to unresolved path {t}"));
        };
        let d = self.display(&p);
        if self.protected.matches(&p) {
            return (Destructive, format!("write to secret path: {d}"));
        }
        if self.is_control(&p) {
            return (Destructive, format!("write to protected control file: {d}"));
        }
        if self.all_forms_in(&self.worktree, &p) {
            return (Safe, format!("write inside worktree: {d}"));
        }
        if self.temps.iter().any(|r| paths::forms(&p).iter().all(|f| r.contains(f))) {
            return (Review, format!("write to temp dir: {d}"));
        }
        (Destructive, format!("write outside worktree: {d}"))
    }

    /// Shell read of a path argument.
    fn shell_read(&self, w: &Word, ctx: &Ctx) -> (RiskClass, String) {
        if w.dynamic {
            return (Review, format!("read of dynamic path {}", w.text));
        }
        let Some(p) = self.abs(Path::new(&w.text), ctx.cwd.as_deref()) else {
            return (Review, format!("read of unresolved path {}", w.text));
        };
        if w.glob {
            for hit in glob_hits(&p) {
                if self.protected.matches(&hit) {
                    return (Destructive, format!("glob matches secret path: {}", self.display(&hit)));
                }
            }
        }
        self.read_class(Some(&p))
    }

    /// Protected-path check for arbitrary argument words (any command).
    fn arg_secret(&self, w: &Word, ctx: &Ctx) -> Option<String> {
        if w.dynamic {
            return None;
        }
        let mut base = vec![w.text.as_str()];
        if let Some((k, v)) = w.text.split_once('=')
            && !k.contains('/')
        {
            base.push(v);
        }
        let mut cands = Vec::new();
        for b in base {
            cands.push(b);
            if let Some(r) = b.strip_prefix('@') {
                cands.push(r); // curl -d @file, -F f=@file
            }
        }
        for c in cands {
            if c.is_empty() || c.starts_with('-') || c.contains('\n') {
                continue;
            }
            let cwd = ctx.cwd.clone().unwrap_or_else(|| self.worktree_root.clone());
            let Some(p) = self.abs(Path::new(c), Some(&cwd)) else { continue };
            if self.protected.matches(&p) {
                return Some(format!("secret path: {}", self.display(&p)));
            }
            if w.glob {
                for hit in glob_hits(&p) {
                    if self.protected.matches(&hit) {
                        return Some(format!("glob matches secret path: {}", self.display(&hit)));
                    }
                }
            }
        }
        None
    }

    fn call_paths(&self, call: &ToolCall) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = call.locations.clone();
        if let Some(d) = &call.diff {
            v.push(d.path.clone());
            if let Some(o) = &d.old_path {
                v.push(o.clone());
            }
        }
        if let Some(raw) = call.raw_input.as_deref() {
            let t = raw.trim();
            if t.starts_with('{') {
                if let Ok(serde_json::Value::Object(m)) = serde_json::from_str::<serde_json::Value>(t) {
                    for k in [
                        "path", "file_path", "filePath", "filepath", "abs_path", "absolute_path", "notebook_path", "target",
                        "source", "destination", "dest", "from", "to", "old_path", "new_path", "directory", "dir",
                    ] {
                        match m.get(k) {
                            Some(serde_json::Value::String(s)) if !s.is_empty() => v.push(PathBuf::from(s)),
                            Some(serde_json::Value::Array(a)) => {
                                v.extend(a.iter().filter_map(|x| x.as_str()).map(PathBuf::from));
                            }
                            _ => {}
                        }
                    }
                }
            } else if v.is_empty() && !t.is_empty() && !t.contains('\n') && call.kind != ToolKind::Search {
                v.push(PathBuf::from(t));
            }
        }
        v
    }

    // ---------------------------------------------------------------- shell

    /// Evaluates a script; returns what its last pipeline writes to stdout.
    fn eval_script(&self, src: &str, ctx: &mut Ctx, acc: &mut Acc) -> Flow {
        if ctx.depth > shell::MAX_DEPTH {
            acc.add(Review, "shell nesting too deep");
            return Flow::Unknown;
        }
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains(":(){:|:&};:") || compact.contains(":(){:|:&}") {
            acc.add(Destructive, "fork bomb");
        }
        let script: Script = shell::parse(src, ctx.depth);
        for w in &script.warnings {
            match w {
                Warning::Unterminated(what) => acc.add(Review, format!("malformed shell input (unterminated {what})")),
                Warning::EscapeObfuscation => acc.add(Review, "escape-sequence obfuscation (\\x / octal)"),
                Warning::Ifs => acc.add(Review, "$IFS obfuscation"),
                Warning::TooLong => acc.add(Review, "command too long to analyze fully"),
                Warning::TooDeep => acc.add(Review, "shell nesting too deep"),
                Warning::FunctionDef(n) => {
                    ctx.functions.insert(n.clone());
                }
            }
        }
        let mut out = Flow::None;
        for p in &script.pipelines {
            out = self.eval_pipeline(p, ctx, acc);
        }
        out
    }

    fn eval_pipeline(&self, p: &shell::Pipeline, ctx: &mut Ctx, acc: &mut Acc) -> Flow {
        let mut flow = Flow::None;
        let mut names: Vec<String> = Vec::new();
        for cmd in &p.cmds {
            let mut words: Vec<Word> = Vec::new();
            for w in &cmd.words {
                words.extend(self.expand(w, ctx, acc));
            }
            let mut stdin = std::mem::replace(&mut flow, Flow::None);
            for r in &cmd.redirs {
                match r.kind {
                    RedirKind::Write | RedirKind::Append | RedirKind::ReadWrite => {
                        let t = self.expand_joined(&r.target, ctx, acc);
                        let (c, why) = self.shell_write(&t, ctx);
                        acc.add(c, why);
                        if r.kind == RedirKind::ReadWrite {
                            let (c, why) = self.shell_read(&t, ctx);
                            acc.add(c, why);
                        }
                    }
                    RedirKind::Read => {
                        let t = self.expand_joined(&r.target, ctx, acc);
                        let (c, why) = self.shell_read(&t, ctx);
                        acc.add(c, why);
                        stdin = Flow::Unknown;
                    }
                    RedirKind::HereDoc => {
                        let body = r.body.clone().unwrap_or_default();
                        let t = self.expand_joined(&body, ctx, acc);
                        stdin = if t.dynamic { Flow::Unknown } else { Flow::Literal(t.text) };
                    }
                    RedirKind::HereString => {
                        let t = self.expand_joined(&r.target, ctx, acc);
                        stdin = if t.dynamic { Flow::Unknown } else { Flow::Literal(t.text) };
                    }
                    RedirKind::Dup => {}
                }
            }
            names.push(
                words
                    .iter()
                    .find(|w| !matches!(w.text.as_str(), "{" | "!" | "(" | "}"))
                    .map(|w| w.text.clone())
                    .unwrap_or_default(),
            );
            flow = self.eval_cmd(words, stdin, ctx, acc);
        }
        // Fork bomb: a defined function piped into itself.
        for w in names.windows(2) {
            if w[0] == w[1] && ctx.functions.contains(&w[0]) {
                acc.add(Destructive, "fork bomb (recursive function pipe)");
            }
        }
        flow
    }

    fn expand_joined(&self, w: &RawWord, ctx: &mut Ctx, acc: &mut Acc) -> Word {
        let ws = self.expand(w, ctx, acc);
        let mut out = Word::lit("");
        for (i, x) in ws.iter().enumerate() {
            if i > 0 {
                out.text.push(' ');
            }
            out.text.push_str(&x.text);
            out.dynamic |= x.dynamic;
            out.glob |= x.glob;
            out.tainted |= x.tainted;
        }
        out
    }

    /// Expands a word: evaluates substitutions (classifying them), resolves what it
    /// statically can, and performs field splitting on unquoted expansion results.
    fn expand(&self, w: &RawWord, ctx: &mut Ctx, acc: &mut Acc) -> Vec<Word> {
        let mut fields: Vec<Word> = Vec::new();
        let mut cur = Word::lit("");
        let mut cur_has = false;
        let push_split = |text: &str, cur: &mut Word, cur_has: &mut bool, fields: &mut Vec<Word>, base: &Word| {
            let mut first = true;
            let starts_ws = text.starts_with(char::is_whitespace);
            for piece in text.split_whitespace() {
                if !first || (starts_ws && *cur_has) {
                    if *cur_has {
                        fields.push(std::mem::replace(cur, Word::lit("")));
                    }
                    *cur_has = false;
                }
                cur.text.push_str(piece);
                cur.dynamic |= base.dynamic;
                cur.tainted |= base.tainted;
                *cur_has = true;
                first = false;
            }
            if text.ends_with(char::is_whitespace) && *cur_has {
                fields.push(std::mem::replace(cur, Word::lit("")));
                *cur_has = false;
            }
        };
        for part in &w.parts {
            match part {
                Part::Lit { text, quoted } => {
                    if !quoted && text.contains(['*', '?', '[']) {
                        cur.glob = true;
                    }
                    cur.text.push_str(text);
                    cur_has = true;
                }
                Part::Sub { script, quoted } => {
                    let mut sub = ctx.child();
                    let flow = self.eval_script(script, &mut sub, acc);
                    let (text, dynamic, tainted) = match flow {
                        Flow::Literal(t) => (t.trim_end_matches('\n').to_string(), false, false),
                        Flow::Decoded(Some(t)) => (t.trim_end_matches('\n').to_string(), false, true),
                        Flow::Decoded(None) | Flow::Fetch => ("$(...)".to_string(), true, true),
                        Flow::None => (String::new(), false, false),
                        Flow::Unknown => ("$(...)".to_string(), true, false),
                    };
                    let base = Word { text: String::new(), dynamic, glob: false, tainted };
                    if *quoted || dynamic {
                        cur.text.push_str(&text);
                        cur.dynamic |= dynamic;
                        cur.tainted |= tainted;
                        cur_has = true;
                    } else {
                        push_split(&text, &mut cur, &mut cur_has, &mut fields, &base);
                    }
                }
                Part::Expr { name, subs, quoted: _ } => {
                    let mut tainted = false;
                    for s in subs {
                        let mut sub = ctx.child();
                        let f = self.eval_script(s, &mut sub, acc);
                        tainted |= matches!(f, Flow::Fetch | Flow::Decoded(_));
                    }
                    match name.as_str() {
                        "HOME" => cur.text.push_str(&self.home.to_string_lossy()),
                        "PWD" if ctx.cwd.is_some() => {
                            cur.text.push_str(&ctx.cwd.as_ref().map(|c| c.to_string_lossy().into_owned()).unwrap_or_default())
                        }
                        _ => {
                            cur.text.push_str(&format!("${name}"));
                            cur.dynamic = true;
                        }
                    }
                    cur.tainted |= tainted;
                    cur_has = true;
                }
            }
        }
        if cur_has || fields.is_empty() && !w.parts.is_empty() {
            fields.push(cur);
        }
        fields
    }

    fn eval_cmd(&self, words: Vec<Word>, stdin: Flow, ctx: &mut Ctx, acc: &mut Acc) -> Flow {
        let mut words = words;
        // Leading assignments.
        let mut i = 0;
        while i < words.len() {
            let w = &words[i];
            let Some((name, _)) = w.text.split_once('=') else { break };
            if name.is_empty() || !name.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) || name.starts_with(|c: char| c.is_ascii_digit()) {
                break;
            }
            if dangerous_env_name(name) {
                acc.add(Review, format!("sets sensitive variable {name}"));
            }
            i += 1;
        }
        words.drain(..i);
        // Reserved words / grouping.
        while let Some(f) = words.first() {
            if !f.dynamic && matches!(f.text.as_str(), "!" | "{" | "}" | "if" | "then" | "else" | "elif" | "fi" | "do" | "done" | "while" | "until" | "esac" | "in") {
                words.remove(0);
            } else {
                break;
            }
        }
        if words.is_empty() {
            return Flow::None;
        }
        if !words[0].dynamic && matches!(words[0].text.as_str(), "for" | "case" | "select" | "function") {
            return Flow::None;
        }

        // Unwrap wrappers.
        let mut stdin = stdin;
        for _ in 0..16 {
            if words.is_empty() {
                return Flow::None;
            }
            let name = cmd_name(&words[0]);
            match name.as_str() {
                "sudo" | "doas" => {
                    acc.add(Review, "runs with elevated privileges (sudo)");
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        let f = words[j].text.as_str();
                        if f == "--" {
                            j += 1;
                            break;
                        }
                        if matches!(f, "-u" | "-g" | "-h" | "-p" | "-C" | "-U" | "-r" | "-t" | "-D" | "-R" | "-T") {
                            j += 1;
                        }
                        if matches!(f, "-i" | "-s" | "--login" | "--shell") && j + 1 >= words.len() {
                            acc.add(Review, "interactive root shell");
                        }
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "env" => {
                    let mut j = 1;
                    let mut split: Option<String> = None;
                    while j < words.len() {
                        let t = words[j].text.clone();
                        if t == "--" {
                            j += 1;
                            break;
                        } else if matches!(t.as_str(), "-u" | "-C" | "-P" | "--unset" | "--chdir") {
                            j += 2;
                        } else if t == "-S" || t == "--split-string" {
                            split = words.get(j + 1).map(|w| w.text.clone());
                            j += 2;
                            break;
                        } else if let Some(s) = t.strip_prefix("--split-string=") {
                            split = Some(s.to_string());
                            j += 1;
                            break;
                        } else if t.starts_with('-') && t.len() > 1 {
                            j += 1;
                        } else if t.contains('=') && !t.starts_with('=') {
                            let n = t.split('=').next().unwrap_or("");
                            if dangerous_env_name(n) {
                                acc.add(Review, format!("sets sensitive variable {n}"));
                            }
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    let rest: Vec<Word> = words.drain(j.min(words.len())..).collect();
                    if let Some(s) = split {
                        let mut sub = ctx.child();
                        let joined = format!("{} {}", s, rest.iter().map(|w| shell::quote(&w.text)).collect::<Vec<_>>().join(" "));
                        return self.eval_script(&joined, &mut sub, acc);
                    }
                    if rest.is_empty() {
                        if words.len() == 1 {
                            acc.add(Safe, "prints environment");
                        }
                        return Flow::Unknown;
                    }
                    words = rest;
                }
                "nohup" | "builtin" | "chronic" | "unbuffer" => {
                    words.remove(0);
                }
                "exec" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        if words[j].text == "-a" {
                            j += 1;
                        }
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "command" => {
                    if words.get(1).is_some_and(|w| w.text == "-v" || w.text == "-V") {
                        acc.add(Safe, "command lookup");
                        return Flow::Unknown;
                    }
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "time" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "nice" | "ionice" | "taskpolicy" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        if matches!(words[j].text.as_str(), "-n" | "-c" | "-p" | "-b" | "-d" | "-g" | "-t" | "-l") {
                            j += 1;
                        }
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "timeout" | "gtimeout" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        if matches!(words[j].text.as_str(), "-s" | "-k" | "--signal" | "--kill-after") {
                            j += 1;
                        }
                        j += 1;
                    }
                    j += 1; // duration
                    words.drain(..j.min(words.len()));
                }
                "stdbuf" | "caffeinate" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        if matches!(words[j].text.as_str(), "-i" | "-o" | "-e" | "-t" | "-w") {
                            j += 1;
                        }
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                }
                "xargs" | "parallel" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        let f = words[j].text.as_str();
                        if matches!(f, "-I" | "-L" | "-n" | "-P" | "-s" | "-E" | "-d" | "-a" | "-J" | "-R" | "-S" | "-j") {
                            j += 1;
                        }
                        j += 1;
                    }
                    words.drain(..j.min(words.len()));
                    if words.is_empty() {
                        words.push(Word::lit("echo"));
                    }
                    // Arguments come from stdin.
                    let taint = matches!(stdin, Flow::Fetch | Flow::Decoded(_));
                    words.push(Word { text: "$(stdin)".into(), dynamic: true, glob: false, tainted: taint });
                }
                "watch" => {
                    let mut j = 1;
                    while j < words.len() && words[j].is_flag() {
                        if matches!(words[j].text.as_str(), "-n" | "--interval") {
                            j += 1;
                        }
                        j += 1;
                    }
                    let joined: Vec<String> = words.iter().skip(j).map(|w| w.text.clone()).collect();
                    acc.add(Review, "repeats a command (watch)");
                    let mut sub = ctx.child();
                    return self.eval_script(&joined.join(" "), &mut sub, acc);
                }
                _ => break,
            }
        }
        if words.is_empty() {
            return Flow::None;
        }
        if words[0].dynamic {
            acc.add(Review, format!("dynamic command name {}", words[0].text));
            if words[0].tainted {
                acc.add(Destructive, "executes decoded or downloaded text as a command");
            }
            for w in &words[1..] {
                if let Some(r) = self.arg_secret(w, ctx) {
                    acc.add(Destructive, r);
                }
            }
            return Flow::Unknown;
        }

        let name = cmd_name(&words[0]);
        let args = &words[1..];
        if !ctx.downloaded.is_empty() {
            let runs_downloaded = |w: &Word| {
                !w.dynamic && self.abs(Path::new(&w.text), ctx.cwd.as_deref()).is_some_and(|p| ctx.downloaded.contains(&p))
            };
            let interp = SHELLS.contains(&name.as_str()) || INTERPRETERS.contains(&name.as_str()) || matches!(name.as_str(), "source" | ".");
            if runs_downloaded(&words[0]) || (interp && args.iter().any(runs_downloaded)) {
                acc.add(Destructive, "executes a file downloaded in the same command (download-then-run)");
            }
        }
        let mut local = Acc::new();
        let flow = self.command(&name, args, &mut stdin, ctx, &mut local, acc);

        // Trust only lowers Review to Safe; never touches Destructive.
        if local.class == Review && self.trusted(&words) {
            local.class = Safe;
            local.reason = "trusted command (policy)".into();
        }
        acc.add(local.class, local.reason);

        if ctx.functions.contains(&words[0].text) {
            acc.add(Review, format!("calls shell function {}", words[0].text));
        }
        if !matches!(name.as_str(), "echo" | "printf" | "grep" | "egrep" | "fgrep" | "rg" | "ag" | "jq" | "find" | "awk" | "sed" | "git") {
            for w in args {
                if let Some(r) = self.arg_secret(w, ctx) {
                    acc.add(Destructive, r);
                }
            }
        }
        if args.iter().any(|w| w.tainted) && (SHELLS.contains(&name.as_str()) || INTERPRETERS.contains(&name.as_str()) || name == "eval") {
            acc.add(Destructive, "decoded or downloaded payload executed");
        }
        flow
    }

    fn trusted(&self, words: &[Word]) -> bool {
        if words.iter().any(|w| w.dynamic) {
            return false;
        }
        self.policy.trusted_commands.iter().any(|t| {
            let toks: Vec<&str> = t.split_whitespace().collect();
            !toks.is_empty()
                && toks.len() <= words.len()
                && toks.iter().zip(words).enumerate().all(|(i, (t, w))| if i == 0 { cmd_name(w) == t.to_lowercase() } else { w.text == *t })
        })
    }

    /// Per-command rules. `local` gets the command's own class (subject to trust);
    /// `acc` gets things trust can never override (secret paths, nested scripts).
    fn command(&self, name: &str, args: &[Word], stdin: &mut Flow, ctx: &mut Ctx, local: &mut Acc, acc: &mut Acc) -> Flow {
        let positionals = || args.iter().filter(|w| !w.is_flag()).collect::<Vec<_>>();
        let has_flag = |pred: &dyn Fn(&str) -> bool| args.iter().any(|w| !w.dynamic && pred(&w.text));
        let short_has = |c: char| has_flag(&|t: &str| t.starts_with('-') && !t.starts_with("--") && t[1..].contains(c));

        match name {
            // ---------------------------------------------------------- pure / readers
            "cd" | "pushd" => {
                local.add(Safe, "change directory");
                ctx.cwd = match args.iter().find(|w| !w.is_flag()) {
                    None => Some(self.home.clone()),
                    Some(w) if w.dynamic || w.text == "-" => None,
                    Some(w) => self.abs(Path::new(&w.text), ctx.cwd.as_deref()),
                };
                return Flow::None;
            }
            "popd" => {
                ctx.cwd = None;
                local.add(Safe, "change directory");
                return Flow::None;
            }
            "echo" | "printf" => {
                local.add(Safe, "prints text");
                return literal_print(name, args).map(Flow::Literal).unwrap_or(Flow::Unknown);
            }
            "date" if has_flag(&|t| t == "-s" || t.starts_with("--set")) => {
                local.add(Review, "sets system clock");
                return Flow::Unknown;
            }
            n if SAFE_PURE.contains(&n) => {
                local.add(Safe, "read-only command");
                return Flow::Unknown;
            }
            "find" => {
                self.find_rule(args, ctx, local, acc);
                return Flow::Unknown;
            }
            "base64" | "xxd" | "openssl" | "uudecode" | "gunzip" | "zcat" | "gzip" | "rev" | "bzip2" | "xz" => {
                let decode = match name {
                    "base64" => short_has('d') || short_has('D') || has_flag(&|t| t == "--decode"),
                    "xxd" => short_has('r'),
                    "openssl" => args.iter().any(|w| w.text == "-d" || w.text == "base64" || w.text == "enc"),
                    "gzip" | "bzip2" | "xz" => short_has('d') || has_flag(&|t| t == "--decompress"),
                    _ => true,
                };
                if name == "base64" {
                    local.add(Safe, "encodes/decodes text");
                    for w in positionals() {
                        let (c, r) = self.shell_read(w, ctx);
                        local.add(c, r);
                    }
                } else {
                    local.add(Review, format!("{name} transforms data"));
                }
                if decode {
                    let text = match (name, &*stdin) {
                        ("base64", Flow::Literal(t)) => base64_decode(t),
                        ("rev", Flow::Literal(t)) => Some(t.lines().map(|l| l.chars().rev().collect::<String>()).collect::<Vec<_>>().join("\n")),
                        ("xxd", Flow::Literal(t)) if short_has('p') => hex_decode(t),
                        _ => None,
                    };
                    return Flow::Decoded(text);
                }
                return Flow::Unknown;
            }
            n if SAFE_READERS.contains(&n) => {
                if matches!(n, "rg") && has_flag(&|t| t.starts_with("--pre")) {
                    local.add(Review, "rg --pre runs a preprocessor");
                }
                if n == "sort" || n == "tree" {
                    let mut it = args.iter();
                    while let Some(w) = it.next() {
                        if w.text == "-o" {
                            if let Some(t) = it.next() {
                                let (c, r) = self.shell_write(t, ctx);
                                local.add(c, r);
                            }
                        } else if let Some(t) = w.text.strip_prefix("--output=") {
                            let (c, r) = self.shell_write(&Word { text: t.into(), ..w.clone() }, ctx);
                            local.add(c, r);
                        }
                    }
                }
                local.add(Safe, "read-only command");
                let mut pos = positionals();
                if n == "cat" && pos.is_empty() {
                    return std::mem::replace(stdin, Flow::None);
                }
                let pattern_first = matches!(n, "grep" | "egrep" | "fgrep" | "rg" | "ag" | "jq")
                    && !has_flag(&|t| t == "-e" || t == "-f" || t.starts_with("--regexp") || t.starts_with("--file"));
                if pattern_first && !pos.is_empty() {
                    pos.remove(0);
                }
                // Values of flags that take an argument would otherwise look like paths;
                // they are still covered by the secret check below.
                for w in pos {
                    if w.text.chars().all(|c| c.is_ascii_digit()) {
                        continue;
                    }
                    let (c, r) = self.shell_read(w, ctx);
                    local.add(c, r);
                    if c == Destructive {
                        acc.add(c, local.reason.clone());
                    }
                }
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- deletion
            "rm" | "unlink" | "rmdir" | "srm" | "shred" | "trash" => {
                self.rm_rule(name, args, ctx, local);
                return Flow::None;
            }

            // ---------------------------------------------------------- file mutation
            "touch" | "mkdir" | "tee" | "truncate" => {
                local.add(if name == "truncate" { Review } else { Safe }, format!("{name} inside worktree"));
                let mut skip_next = false;
                for w in args {
                    if skip_next {
                        skip_next = false;
                        continue;
                    }
                    if w.is_flag() {
                        skip_next = matches!(w.text.as_str(), "-s" | "-r" | "-m" | "-t" | "-d" | "-A");
                        continue;
                    }
                    let (c, r) = self.shell_write(w, ctx);
                    local.add(c, r);
                }
                return Flow::Unknown;
            }
            "cp" | "mv" | "ln" | "install" | "rsync" | "ditto" => {
                local.add(if matches!(name, "cp" | "mv") { Safe } else { Review }, format!("{name} inside worktree"));
                let mut pos: Vec<&Word> = Vec::new();
                let mut target_dir: Option<&Word> = None;
                let mut it = args.iter();
                while let Some(w) = it.next() {
                    if w.text == "-t" || w.text == "--target-directory" {
                        target_dir = it.next();
                    } else if let Some(t) = w.text.strip_prefix("--target-directory=") {
                        let _ = t;
                        target_dir = Some(w);
                    } else if !w.is_flag() {
                        pos.push(w);
                    }
                }
                if name == "rsync" && pos.iter().any(|w| w.text.contains(':')) {
                    local.add(Review, "rsync to/from a remote host");
                }
                if name == "rsync" && has_flag(&|t| t.starts_with("--delete") || t == "--remove-source-files") {
                    local.add(Destructive, "rsync --delete removes files");
                }
                let (srcs, dests): (Vec<&Word>, Vec<&Word>) = match (target_dir, name) {
                    (Some(t), _) => (pos.clone(), vec![t]),
                    (None, "mv") => (vec![], pos.clone()),
                    (None, _) if pos.len() >= 2 => (pos[..pos.len() - 1].to_vec(), vec![pos[pos.len() - 1]]),
                    (None, _) => (vec![], pos.clone()),
                };
                for w in srcs {
                    let (c, r) = self.shell_read(w, ctx);
                    local.add(if c == Safe { Safe } else { c }, r);
                }
                for w in dests {
                    let w = match w.text.strip_prefix("--target-directory=") {
                        Some(t) => Word { text: t.into(), ..w.clone() },
                        None => w.clone(),
                    };
                    let (c, r) = self.shell_write(&w, ctx);
                    local.add(c, r);
                }
                return Flow::None;
            }
            "chmod" | "chown" | "chgrp" | "chflags" | "xattr" => {
                if short_has('R') || has_flag(&|t| t == "--recursive") {
                    local.add(Destructive, format!("recursive {name}"));
                } else {
                    local.add(Review, format!("changes file metadata ({name})"));
                }
                for (i, w) in positionals().into_iter().enumerate() {
                    if i == 0 && name != "xattr" {
                        continue; // mode / owner
                    }
                    let (c, r) = self.shell_write(w, ctx);
                    if c > Safe {
                        local.add(c, r);
                    }
                }
                return Flow::None;
            }
            "sed" | "perl" if has_flag(&|t| t.starts_with("-i") || (t.starts_with("-") && !t.starts_with("--") && t.contains('i') && name == "perl")) => {
                local.add(Review, format!("{name} in-place edit"));
                for w in positionals().into_iter().skip(1) {
                    let (c, r) = self.shell_write(w, ctx);
                    if c > Safe {
                        local.add(c, r);
                    }
                }
                if name == "perl" {
                    self.inline_code(args, local);
                }
                return Flow::Unknown;
            }
            "dd" => {
                local.add(Review, "raw copy (dd)");
                for w in args {
                    if let Some(of) = w.text.strip_prefix("of=") {
                        if of.starts_with("/dev/") && !matches!(of, "/dev/null" | "/dev/stdout" | "/dev/stderr") {
                            local.add(Destructive, format!("dd writes to device {of}"));
                        } else {
                            let (c, r) = self.shell_write(&Word { text: of.into(), ..w.clone() }, ctx);
                            local.add(c, r);
                        }
                    } else if let Some(i) = w.text.strip_prefix("if=") {
                        let (c, r) = self.shell_read(&Word { text: i.into(), ..w.clone() }, ctx);
                        if c > Safe {
                            local.add(c, r);
                        }
                    }
                }
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- git & build tools
            "git" => {
                self.git_rule(args, ctx, local);
                return Flow::Unknown;
            }
            "cargo" => {
                let pos: Vec<&Word> = args.iter().filter(|w| !w.is_flag() && !w.text.starts_with('+')).collect();
                let sub = pos.first().map(|w| w.text.as_str()).unwrap_or("");
                if has_flag(&|t| t.starts_with("--config")) {
                    local.add(Review, "cargo --config override");
                }
                match sub {
                    "check" | "c" | "build" | "b" | "test" | "t" | "clippy" | "fmt" | "doc" | "tree" | "metadata" | "bench" | "nextest" => {
                        local.add(Safe, format!("cargo {sub}"))
                    }
                    "publish" | "yank" | "owner" => local.add(Destructive, format!("cargo {sub} affects the public registry")),
                    _ => local.add(Review, format!("cargo {sub}")),
                }
                return Flow::Unknown;
            }
            "npm" | "pnpm" | "yarn" | "bun" => {
                self.js_pm_rule(name, args, local);
                return Flow::Unknown;
            }
            "npx" | "pnpx" | "bunx" => {
                local.add(Review, format!("{name} downloads and runs a package"));
                return Flow::Unknown;
            }
            "pytest" | "py.test" | "tsc" | "eslint" | "vitest" | "jest" | "mypy" | "ruff" | "black" if name != "black" || has_flag(&|t| t == "--check") => {
                if name == "ruff" && args.first().is_some_and(|w| w.text != "check" && w.text != "format") {
                    local.add(Review, "ruff");
                } else {
                    local.add(Safe, format!("{name} (test/lint)"));
                }
                return Flow::Unknown;
            }
            "prettier" => {
                if has_flag(&|t| matches!(t, "--check" | "-c" | "--list-different" | "-l")) && !has_flag(&|t| t == "--write" || t == "-w") {
                    local.add(Safe, "prettier --check");
                } else {
                    local.add(Review, "prettier rewrites files");
                }
                return Flow::Unknown;
            }
            "go" => {
                match args.first().map(|w| w.text.as_str()).unwrap_or("") {
                    "test" | "build" | "vet" | "fmt" | "version" | "env" | "list" => local.add(Safe, "go build/test"),
                    s => local.add(Review, format!("go {s}")),
                }
                return Flow::Unknown;
            }
            "make" | "gmake" => {
                let pos: Vec<&str> = args.iter().filter(|w| !w.is_flag()).map(|w| w.text.as_str()).collect();
                let bad_flags = has_flag(&|t| t.starts_with("-C") || t.starts_with("-f") || t.starts_with("--file") || t.starts_with("--directory"));
                if !bad_flags && !pos.is_empty() && pos.iter().all(|t| matches!(*t, "test" | "check" | "lint")) {
                    local.add(Safe, "make test");
                } else {
                    local.add(Review, "make runs arbitrary recipes");
                }
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- interpreters
            n if SHELLS.contains(&n) => return self.shell_rule(n, args, stdin, ctx, local, acc),
            "eval" => {
                local.add(Review, "eval");
                let text = args.iter().map(|w| w.text.clone()).collect::<Vec<_>>().join(" ");
                if args.iter().any(|w| w.dynamic) {
                    local.add(Review, "eval of dynamic text");
                }
                let mut sub = ctx.child();
                return self.eval_script(&text, &mut sub, acc);
            }
            "source" | "." => {
                local.add(Review, "sources a script");
                return Flow::Unknown;
            }
            n if INTERPRETERS.contains(&n) || n.starts_with("python3.") => {
                if matches!(stdin, Flow::Fetch) {
                    acc.add(Destructive, format!("remote script piped to {n}"));
                } else if matches!(stdin, Flow::Decoded(_)) {
                    acc.add(Destructive, format!("decoded payload piped to {n}"));
                }
                if n.starts_with("python") && args.len() >= 2 && args[0].text == "-m" && matches!(args[1].text.as_str(), "pytest" | "unittest" | "mypy") {
                    local.add(Safe, "python test runner");
                    return Flow::Unknown;
                }
                local.add(Review, format!("runs {n} code"));
                self.inline_code(args, acc);
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- SQL / data stores
            n if SQL_CLIENTS.contains(&n) => {
                self.sql_rule(n, args, stdin, local);
                return Flow::Unknown;
            }
            "mongo" | "mongosh" => {
                let text: String = args.iter().map(|w| w.text.as_str()).chain(match stdin {
                    Flow::Literal(t) => Some(t.as_str()),
                    _ => None,
                }).collect::<Vec<_>>().join(" ");
                static M: OnceLock<Regex> = OnceLock::new();
                if re(&M, r"(?i)dropDatabase|\.drop\(|deleteMany\(\s*\{\s*\}\s*\)|remove\(\s*\{\s*\}\s*\)").is_match(&text) {
                    local.add(Destructive, "destructive MongoDB operation");
                } else {
                    local.add(Review, "database access");
                }
                return Flow::Unknown;
            }
            "redis-cli" => {
                if args.iter().any(|w| matches!(w.text.to_uppercase().as_str(), "FLUSHALL" | "FLUSHDB")) {
                    local.add(Destructive, "redis FLUSHALL/FLUSHDB");
                } else {
                    local.add(Review, "database access");
                }
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- network
            "curl" | "wget" | "http" | "https" | "xh" | "aria2c" | "fetch" => {
                local.add(Review, format!("network access ({name})"));
                let mut it = args.iter();
                let mut to_file = false;
                while let Some(w) = it.next() {
                    let out = match w.text.as_str() {
                        "-o" | "--output" | "-O" | "--output-document" if !(name == "curl" && w.text == "-O") => it.next().cloned(),
                        t => t
                            .strip_prefix("--output=")
                            .or_else(|| t.strip_prefix("--output-document="))
                            .map(|v| Word { text: v.into(), ..w.clone() }),
                    };
                    if let Some(o) = out {
                        to_file = true;
                        if o.text == "-" {
                            to_file = false;
                            continue;
                        }
                        let (c, r) = self.shell_write(&o, ctx);
                        local.add(c, r);
                        if let Some(p) = self.abs(Path::new(&o.text), ctx.cwd.as_deref()) {
                            ctx.downloaded.insert(p);
                        }
                    }
                }
                return if to_file { Flow::None } else { Flow::Fetch };
            }
            "ssh" | "scp" | "sftp" | "nc" | "ncat" | "netcat" | "socat" | "telnet" | "ftp" => {
                local.add(Review, format!("network access ({name})"));
                return Flow::Unknown;
            }

            // ---------------------------------------------------------- system
            n if n.starts_with("mkfs") || n.starts_with("newfs") || n == "fdisk" || n == "gpt" || n == "wipefs" => {
                local.add(Destructive, format!("formats a disk ({n})"));
            }
            "diskutil" => {
                let sub = args.first().map(|w| w.text.to_lowercase()).unwrap_or_default();
                if sub.starts_with("erase") || sub.starts_with("partition") || sub.contains("zero") || sub.contains("random") || sub.starts_with("secureerase") || sub.starts_with("reformat") || (sub == "apfs" && args.get(1).is_some_and(|w| { let s = w.text.to_lowercase(); s.starts_with("delete") || s.starts_with("erase") })) {
                    local.add(Destructive, format!("diskutil {sub}"));
                } else {
                    local.add(Review, format!("diskutil {sub}"));
                }
            }
            "shutdown" | "reboot" | "halt" | "poweroff" => local.add(Destructive, format!("{name} the machine")),
            "launchctl" => local.add(Destructive, "launchctl modifies system services"),
            "csrutil" | "nvram" | "spctl" | "systemsetup" | "bless" | "fdesetup" | "dscl" => local.add(Destructive, format!("{name} modifies system security settings")),
            "security" => local.add(Destructive, "keychain access (security)"),
            "tmutil" if args.first().is_some_and(|w| w.text.starts_with("delete")) => local.add(Destructive, "deletes Time Machine backups"),
            "crontab" => {
                if short_has('r') {
                    local.add(Destructive, "crontab -r removes all cron jobs");
                } else {
                    local.add(Review, "edits cron jobs");
                }
            }
            "kill" => {
                let mut rest = args.iter().peekable();
                if let Some(w) = rest.peek()
                    && w.text.starts_with('-')
                {
                    let sig = rest.next().map(|w| w.text.clone()).unwrap_or_default();
                    if sig == "-s" || sig == "-n" {
                        rest.next();
                    }
                }
                if rest.any(|w| w.text == "-1" || w.text == "0" || w.text == "-0") {
                    local.add(Destructive, "kill every process (kill -1)");
                } else {
                    local.add(Review, "kills processes");
                }
            }
            "pkill" => {
                if short_has('f') || has_flag(&|t| t == "--full") {
                    local.add(Destructive, "pkill -f matches arbitrary command lines");
                } else {
                    local.add(Review, "kills processes");
                }
            }
            "killall" => local.add(Review, "kills processes"),

            // ---------------------------------------------------------- publish / cloud
            "gh" => {
                let a: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
                match a.as_slice() {
                    ["release", "create", ..] | ["release", "delete", ..] | ["repo", "delete", ..] | ["repo", "archive", ..] | ["secret", ..] => {
                        local.add(Destructive, format!("gh {}", a.iter().take(2).copied().collect::<Vec<_>>().join(" ")))
                    }
                    _ => local.add(Review, "GitHub CLI"),
                }
            }
            "docker" | "podman" => {
                let a: Vec<&str> = args.iter().filter(|w| !w.is_flag()).map(|w| w.text.as_str()).collect();
                match a.as_slice() {
                    ["system", "prune", ..] | ["volume", "prune", ..] | ["volume", "rm", ..] | ["image", "prune", ..] | ["builder", "prune", ..] => {
                        local.add(Destructive, format!("{name} {} {}", a[0], a[1]))
                    }
                    _ => local.add(Review, format!("{name} command")),
                }
            }
            "kubectl" | "oc" => {
                if args.iter().any(|w| w.text == "delete" || w.text == "drain") {
                    local.add(Destructive, "kubectl delete");
                } else {
                    local.add(Review, "kubectl command");
                }
            }
            "helm" => {
                if args.iter().any(|w| matches!(w.text.as_str(), "uninstall" | "delete" | "del" | "un")) {
                    local.add(Destructive, "helm uninstall");
                } else {
                    local.add(Review, "helm command");
                }
            }
            "terraform" | "tofu" | "terragrunt" | "pulumi" => {
                let destroy = args.iter().any(|w| w.text == "destroy");
                let apply = args.iter().any(|w| w.text == "apply" || w.text == "up");
                let auto = has_flag(&|t| t.starts_with("-auto-approve") || t.starts_with("--auto-approve") || t == "--yes" || t == "-y");
                if destroy || (apply && auto) || (destroy && auto) {
                    local.add(Destructive, format!("{name} destroy/apply without review"));
                } else {
                    local.add(Review, format!("{name} command"));
                }
            }
            "aws" => {
                let a: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
                let rec = has_flag(&|t| t == "--recursive" || t == "--force");
                let is_s3_rm = a.windows(2).any(|p| p[0] == "s3" && matches!(p[1], "rm" | "rb"));
                let del = a.iter().any(|t| t.starts_with("delete-") || t.starts_with("terminate-") || *t == "delete");
                if (is_s3_rm && rec) || del {
                    local.add(Destructive, "aws destructive operation");
                } else {
                    local.add(Review, "aws command");
                }
            }
            "gcloud" | "gsutil" | "az" | "doctl" | "flyctl" | "fly" | "heroku" => {
                if args.iter().any(|w| matches!(w.text.as_str(), "delete" | "rm" | "destroy" | "destroy-app")) {
                    local.add(Destructive, format!("{name} delete"));
                } else {
                    local.add(Review, format!("{name} command"));
                }
            }
            "twine" | "gem" | "poetry" | "flit" | "hatch" | "pod" | "dotnet" | "mvn" | "gradle" | "vsce" | "ovsx" => {
                if args.iter().any(|w| matches!(w.text.as_str(), "upload" | "push" | "publish" | "deploy" | "trunk")) {
                    local.add(Destructive, format!("{name} publishes artifacts"));
                } else {
                    local.add(Review, format!("{name} command"));
                }
            }
            _ => local.add(Review, format!("unknown command: {name}")),
        }
        Flow::Unknown
    }

    fn rm_rule(&self, name: &str, args: &[Word], ctx: &Ctx, local: &mut Acc) {
        let mut targets: Vec<&Word> = Vec::new();
        let mut recursive = matches!(name, "srm");
        let mut end_opts = false;
        for w in args {
            if !end_opts && w.text == "--" {
                end_opts = true;
                continue;
            }
            if !end_opts && w.is_flag() {
                let t = w.text.as_str();
                if t == "--recursive" || t == "--no-preserve-root" || (!t.starts_with("--") && (t.contains('r') || t.contains('R'))) {
                    recursive = true;
                }
                continue;
            }
            targets.push(w);
        }
        if name == "shred" {
            local.add(Destructive, "shred destroys file contents");
        }
        if recursive {
            local.add(Destructive, format!("recursive delete ({name} -r)"));
        }
        if targets.is_empty() {
            local.add(Review, format!("{name} without targets"));
        }
        for w in targets {
            let t = w.text.as_str();
            if w.dynamic {
                local.add(Destructive, format!("{name} of unresolved target {t}"));
                continue;
            }
            if w.glob {
                local.add(Destructive, format!("{name} with wildcard {t}"));
            }
            if t == "/" || t == "~" || t.starts_with("~/") && t.len() <= 3 || t == "." || t == "*" {
                local.add(Destructive, format!("{name} targets {t}"));
            }
            if t.split('/').any(|c| c == "..") {
                local.add(Destructive, format!("{name} target escapes with ..: {t}"));
            }
            match self.abs(Path::new(t), ctx.cwd.as_deref()) {
                None => local.add(Destructive, format!("{name} of unresolved target {t}")),
                Some(p) => {
                    let d = self.display(&p);
                    if self.protected.matches(&p) {
                        local.add(Destructive, format!("deletes secret path: {d}"));
                    } else if self.is_control(&p) {
                        local.add(Destructive, format!("deletes protected control file: {d}"));
                    } else if !self.all_forms_in(&self.worktree, &p) || self.worktree.0.iter().any(|r| paths::normalize(r) == p) {
                        local.add(Destructive, format!("delete outside worktree: {d}"));
                    } else {
                        local.add(Review, format!("delete inside worktree: {d}"));
                    }
                }
            }
        }
    }

    fn find_rule(&self, args: &[Word], ctx: &mut Ctx, local: &mut Acc, acc: &mut Acc) {
        local.add(Safe, "find (read-only)");
        let mut i = 0;
        // starting points
        while i < args.len() {
            let t = &args[i].text;
            if t.starts_with('-') || t == "(" || t == "!" || t == "\\(" {
                break;
            }
            let (c, r) = self.shell_read(&args[i], ctx);
            local.add(c, r);
            i += 1;
        }
        while i < args.len() {
            let t = args[i].text.as_str();
            match t {
                "-delete" => local.add(Destructive, "find -delete"),
                "-exec" | "-execdir" | "-ok" | "-okdir" => {
                    let mut inner: Vec<Word> = Vec::new();
                    i += 1;
                    while i < args.len() && args[i].text != ";" && args[i].text != "+" {
                        let w = &args[i];
                        if w.text.contains("{}") {
                            inner.push(Word { text: "$(found)".into(), dynamic: true, glob: false, tainted: false });
                        } else {
                            inner.push(w.clone());
                        }
                        i += 1;
                    }
                    local.add(Review, format!("find {t} runs a command"));
                    let mut sub = Acc::new();
                    let mut c2 = Ctx { cwd: None, ..ctx.child() };
                    self.eval_cmd(inner, Flow::None, &mut c2, &mut sub);
                    local.add(sub.class, format!("find {t}: {}", sub.reason));
                }
                "-fprint" | "-fprint0" | "-fprintf" | "-fls" => {
                    if let Some(w) = args.get(i + 1) {
                        let (c, r) = self.shell_write(w, ctx);
                        local.add(c.max(Review), r);
                    }
                }
                _ => {}
            }
            i += 1;
        }
        let _ = acc;
    }

    fn git_rule(&self, args: &[Word], ctx: &Ctx, local: &mut Acc) {
        let mut i = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            match t {
                "-C" => {
                    if let Some(w) = args.get(i + 1) {
                        let (c, r) = self.shell_read(w, ctx);
                        if c > Safe {
                            local.add(Review, format!("git -C elsewhere: {r}"));
                        }
                    }
                    i += 2;
                }
                "-c" => {
                    local.add(Review, "git -c config override");
                    i += 2;
                }
                _ if t.starts_with("--git-dir") || t.starts_with("--work-tree") || t.starts_with("--exec-path") || t.starts_with("--config-env") => {
                    local.add(Review, format!("git {t}"));
                    i += if t.contains('=') { 1 } else { 2 };
                }
                _ if t.starts_with('-') => i += 1,
                _ => break,
            }
        }
        let Some(sub) = args.get(i) else {
            local.add(Safe, "git");
            return;
        };
        let sub = sub.text.as_str();
        let rest: Vec<&str> = args[i + 1..].iter().map(|w| w.text.as_str()).collect();
        let has = |f: &str| rest.contains(&f);
        let short = |c: char| rest.iter().any(|t| t.starts_with('-') && !t.starts_with("--") && t[1..].contains(c));
        let output_flag = rest.iter().any(|t| t.starts_with("--output") || *t == "--ext-diff");
        match sub {
            "status" | "log" | "diff" | "show" | "blame" | "rev-parse" | "ls-files" | "ls-tree" | "cat-file" | "describe" | "shortlog" | "grep" | "merge-base" | "rev-list" | "show-ref" | "whatchanged" | "count-objects" | "version" | "help" | "annotate" | "for-each-ref" | "name-rev" | "check-ignore" | "var" => {
                if output_flag {
                    local.add(Review, format!("git {sub} with --output/--ext-diff"));
                } else {
                    local.add(Safe, format!("git {sub} (read-only)"));
                }
            }
            "branch" => {
                if has("-D") || ((has("-d") || has("--delete")) && (has("-f") || has("--force"))) || short('D') {
                    local.add(Destructive, "git branch -D force-deletes a branch");
                } else if has("-d") || has("--delete") || has("-m") || has("-M") || has("-c") || has("-C") || has("-f") || has("--force") || has("-u") || rest.iter().any(|t| t.starts_with("--set-upstream") || t.starts_with("--edit-description")) {
                    local.add(Review, "git branch modification");
                } else {
                    let positional = rest.iter().any(|t| !t.starts_with('-'));
                    let listing = rest.iter().any(|t| matches!(*t, "--list" | "-l" | "--contains" | "--merged" | "--no-merged" | "--points-at"));
                    if positional && !listing {
                        local.add(Review, "git branch creates a branch");
                    } else {
                        local.add(Safe, "git branch (list)");
                    }
                }
            }
            "push" => {
                let force = rest.iter().any(|t| {
                    *t == "-f" || t.starts_with("--force") || *t == "--mirror" || *t == "--delete" || *t == "-d" || *t == "--prune"
                        || (t.starts_with('-') && !t.starts_with("--") && (t.contains('f') || t.contains('d')))
                        || (!t.starts_with('-') && (t.starts_with('+') || t.starts_with(':') || t.contains(":+")))
                });
                if force {
                    local.add(Destructive, "git push --force / delete rewrites remote history");
                } else {
                    local.add(Review, "git push");
                }
            }
            "reset" => {
                if has("--hard") || has("--merge") || has("--keep") {
                    local.add(Destructive, "git reset --hard discards work");
                } else {
                    local.add(Review, "git reset");
                }
            }
            "clean" => {
                if short('f') || has("--force") {
                    local.add(Destructive, "git clean -f deletes untracked files");
                } else if short('n') || has("--dry-run") {
                    local.add(Safe, "git clean --dry-run");
                } else {
                    local.add(Review, "git clean");
                }
            }
            "checkout" | "restore" => {
                let after_dd: Vec<&str> = rest.iter().skip_while(|t| **t != "--").skip(1).copied().collect();
                let pathish = |t: &&str| matches!(*t, "." | ":/" | "*" | "./" | ":(top)") || t.contains('*');
                let only_staged = sub == "restore" && (has("--staged") || has("-S")) && !(has("--worktree") || has("-W"));
                if has("-f") || has("--force") || (sub == "checkout" && short('f')) {
                    local.add(Destructive, format!("git {sub} --force discards changes"));
                } else if !only_staged && (rest.iter().filter(|t| !t.starts_with('-')).any(pathish) || after_dd.iter().any(pathish)) {
                    local.add(Destructive, format!("git {sub} . discards all uncommitted changes"));
                } else if !only_staged && sub == "checkout" && !after_dd.is_empty() {
                    local.add(Review, "git checkout -- <files> discards changes");
                } else {
                    local.add(Review, format!("git {sub}"));
                }
            }
            "stash" => match rest.first().copied() {
                Some("list") | Some("show") => local.add(Safe, "git stash list"),
                Some("drop") | Some("clear") => local.add(Destructive, "git stash drop/clear discards work"),
                _ => local.add(Review, "git stash"),
            },
            "tag" => {
                if rest.is_empty() || has("-l") || has("--list") {
                    local.add(Safe, "git tag (list)");
                } else {
                    local.add(Review, "git tag");
                }
            }
            "remote" => match rest.first().copied() {
                None | Some("-v") | Some("--verbose") | Some("show") | Some("get-url") => local.add(Safe, "git remote (list)"),
                _ => local.add(Review, "git remote modification"),
            },
            "config" => {
                if rest.iter().any(|t| t.starts_with("--get") || matches!(*t, "--list" | "-l")) {
                    local.add(Safe, "git config (read)");
                } else {
                    local.add(Review, "git config modification");
                }
            }
            "reflog" => match rest.first().copied() {
                Some("expire") | Some("delete") => local.add(Destructive, "git reflog expire/delete destroys recovery data"),
                _ => local.add(Safe, "git reflog"),
            },
            "filter-branch" | "filter-repo" => local.add(Destructive, format!("git {sub} rewrites history")),
            "gc" | "prune" => {
                if sub == "prune" || rest.iter().any(|t| t.starts_with("--prune")) {
                    local.add(Destructive, "git prune destroys unreachable objects");
                } else {
                    local.add(Review, "git gc");
                }
            }
            "update-ref" if has("-d") => local.add(Destructive, "git update-ref -d deletes a ref"),
            _ => local.add(Review, format!("git {sub}")),
        }
    }

    fn js_pm_rule(&self, name: &str, args: &[Word], local: &mut Acc) {
        let pos: Vec<&str> = args.iter().filter(|w| !w.is_flag()).map(|w| w.text.as_str()).collect();
        let sub = pos.first().copied().unwrap_or("");
        let script = pos.get(1).copied().unwrap_or("");
        let safe_script = |s: &str| matches!(s, "test" | "lint" | "build");
        if matches!(sub, "publish" | "unpublish") || (name == "yarn" && sub == "npm" && script == "publish") || (sub == "dist-tag" || sub == "deprecate" || sub == "owner") {
            local.add(Destructive, format!("{name} {sub} affects the public registry"));
        } else if matches!(sub, "test" | "t" | "tst") || (matches!(sub, "run" | "run-script") && safe_script(script)) || (name != "npm" && safe_script(sub)) {
            local.add(Safe, format!("{name} test/lint/build"));
        } else {
            local.add(Review, format!("{name} {sub}"));
        }
    }

    fn shell_rule(&self, n: &str, args: &[Word], stdin: &mut Flow, ctx: &mut Ctx, local: &mut Acc, acc: &mut Acc) -> Flow {
        let mut i = 0;
        let mut has_c = false;
        let args: &[Word] = if n == "busybox" {
            // busybox sh ...
            if args.first().is_some_and(|w| SHELLS.contains(&w.text.as_str())) { &args[1..] } else { args }
        } else {
            args
        };
        while i < args.len() {
            let t = args[i].text.as_str();
            if t == "--" {
                i += 1;
                break;
            }
            if (t.starts_with('-') || t.starts_with('+')) && t.len() > 1 && !args[i].dynamic {
                if t.starts_with("--") {
                    if matches!(t, "--rcfile" | "--init-file" | "--init-command" | "-C") {
                        i += 1;
                    }
                    if t == "--command" {
                        has_c = true;
                    }
                } else {
                    if t[1..].contains('c') {
                        has_c = true;
                    }
                    if t[1..].contains('o') || t[1..].contains('O') {
                        i += 1;
                    }
                }
                i += 1;
                continue;
            }
            break;
        }
        if has_c {
            match args.get(i) {
                Some(w) if !w.dynamic => {
                    local.add(Safe, format!("{n} -c"));
                    let mut sub = ctx.child();
                    let f = self.eval_script(&w.text, &mut sub, acc);
                    if w.tainted {
                        acc.add(Destructive, format!("decoded or downloaded payload executed by {n} -c"));
                    }
                    if matches!(stdin, Flow::Fetch | Flow::Decoded(_)) {
                        acc.add(Destructive, format!("remote/decoded data piped to {n}"));
                    }
                    return f;
                }
                Some(w) => {
                    local.add(Review, format!("{n} -c with dynamic script"));
                    if w.tainted {
                        acc.add(Destructive, format!("decoded or downloaded payload executed by {n} -c"));
                    }
                }
                None => local.add(Review, format!("{n} -c without script")),
            }
            if matches!(stdin, Flow::Fetch | Flow::Decoded(_)) {
                acc.add(Destructive, format!("remote/decoded data piped to {n}"));
            }
            return Flow::Unknown;
        }
        if let Some(w) = args.get(i) {
            local.add(Review, format!("{n} runs script file {}", w.text));
            if matches!(stdin, Flow::Fetch | Flow::Decoded(_)) {
                acc.add(Destructive, format!("remote/decoded data piped to {n}"));
            }
            return Flow::Unknown;
        }
        // Reads the script from stdin.
        match std::mem::replace(stdin, Flow::None) {
            Flow::Fetch => acc.add(Destructive, format!("remote script piped to {n} (curl | sh)")),
            Flow::Decoded(text) => {
                acc.add(Destructive, format!("decoded payload piped to {n}"));
                if let Some(t) = text {
                    let mut sub = ctx.child();
                    self.eval_script(&t, &mut sub, acc);
                }
            }
            Flow::Literal(t) => {
                local.add(Review, format!("script piped to {n}"));
                let mut sub = ctx.child();
                self.eval_script(&t, &mut sub, acc);
            }
            Flow::None => local.add(Review, format!("interactive {n}")),
            Flow::Unknown => local.add(Review, format!("{n} reads script from stdin")),
        }
        Flow::Unknown
    }

    fn sql_rule(&self, n: &str, args: &[Word], stdin: &Flow, local: &mut Acc) {
        let mut texts: Vec<String> = Vec::new();
        let mut file = false;
        let mut i = 0;
        let mut positional = 0;
        while i < args.len() {
            let t = args[i].text.as_str();
            let next = args.get(i + 1).map(|w| w.text.clone());
            if matches!(t, "-c" | "-e" | "--command" | "--execute" | "-Q" | "-q" | "--query") {
                if let Some(x) = next {
                    texts.push(x);
                }
                i += 2;
                continue;
            }
            if let Some(v) = t.strip_prefix("--command=").or_else(|| t.strip_prefix("--execute=")).or_else(|| t.strip_prefix("--query=")) {
                texts.push(v.to_string());
            } else if (t.starts_with("-c") || t.starts_with("-e")) && t.len() > 2 && !t.starts_with("--") {
                texts.push(t[2..].to_string());
            } else if matches!(t, "-f" | "--file" | "-i" | "--init") || t.starts_with("--file=") {
                file = true;
            } else if !t.starts_with('-') {
                positional += 1;
                // sqlite3/duckdb: first positional is the db file, the rest are SQL.
                if matches!(n, "sqlite3" | "sqlite" | "duckdb" | "litecli") && positional > 1 {
                    texts.push(t.to_string());
                }
            }
            i += 1;
        }
        if let Flow::Literal(t) = stdin {
            texts.push(t.clone());
        }
        for t in &texts {
            if let Some(r) = dangerous_sql(t) {
                local.add(Destructive, r);
            }
        }
        if file || matches!(stdin, Flow::Unknown | Flow::Fetch | Flow::Decoded(_)) {
            local.add(Review, format!("{n} runs SQL from a file or stream"));
        }
        local.add(Review, format!("database access ({n})"));
    }

    fn inline_code(&self, args: &[Word], acc: &mut Acc) {
        static D: OnceLock<Regex> = OnceLock::new();
        static S: OnceLock<Regex> = OnceLock::new();
        let code: String = args.iter().map(|w| w.text.as_str()).collect::<Vec<_>>().join(" ");
        if re(&D, r"(?i)\brm\s+-[a-z]*r|rmtree|rmSync|rimraf|fs\.rm\(|remove_dir_all|unlink\(|os\.remove|File\.delete|FileUtils\.rm_r|DROP\s+(TABLE|DATABASE)").is_match(&code) {
            acc.add(Destructive, "destructive operation in inline script");
        }
        if re(&S, r"(?i)\.ssh\b|\.aws\b|\.gnupg|id_rsa|id_ed25519|keychains|\.netrc|\.npmrc|\.kube/|\.docker/config|gcloud/|\.claude/|\.codex/").is_match(&code) {
            acc.add(Destructive, "inline script references a secret path");
        }
    }
}

fn finish(acc: Acc) -> (RiskClass, String) {
    let reason = if acc.reason.is_empty() { "safe".to_string() } else { acc.reason };
    (acc.class, reason)
}

/// Lowercased basename of the command word: `/bin/RM` -> `rm` (APFS is case-insensitive).
fn cmd_name(w: &Word) -> String {
    let t = w.text.rsplit('/').next().unwrap_or(&w.text);
    t.to_lowercase()
}

fn dangerous_env_name(n: &str) -> bool {
    let u = n.to_uppercase();
    matches!(u.as_str(), "PATH" | "IFS" | "BASH_ENV" | "ENV" | "PROMPT_COMMAND" | "NODE_OPTIONS" | "PYTHONPATH" | "PYTHONSTARTUP" | "PERL5OPT" | "RUBYOPT" | "GIT_SSH" | "GIT_SSH_COMMAND" | "GIT_EXEC_PATH" | "GIT_DIR" | "GIT_WORK_TREE" | "GIT_CONFIG" | "HOME" | "SHELLOPTS" | "BASHOPTS" | "ZDOTDIR")
        || u.starts_with("LD_")
        || u.starts_with("DYLD_")
        || u.starts_with("GIT_CONFIG_")
}

fn literal_print(name: &str, args: &[Word]) -> Option<String> {
    if args.iter().any(|w| w.dynamic) {
        return None;
    }
    let mut a: Vec<&str> = args.iter().map(|w| w.text.as_str()).collect();
    if name == "echo" {
        let mut nl = true;
        while let Some(f) = a.first() {
            if f.len() > 1 && f.starts_with('-') && f[1..].chars().all(|c| matches!(c, 'n' | 'e' | 'E')) {
                if f.contains('n') {
                    nl = false;
                }
                a.remove(0);
            } else {
                break;
            }
        }
        let mut s = a.join(" ");
        if nl {
            s.push('\n');
        }
        Some(s)
    } else {
        let fmt = *a.first()?;
        let rest = &a[1..];
        let fmt = fmt.replace("\\n", "\n").replace("\\t", "\t");
        if fmt == "%s" || fmt == "%s\n" {
            return Some(rest.join(""));
        }
        if fmt.contains('%') {
            return None;
        }
        Some(fmt)
    }
}

fn dangerous_sql(sql: &str) -> Option<String> {
    static DROP: OnceLock<Regex> = OnceLock::new();
    static TRUNC: OnceLock<Regex> = OnceLock::new();
    static DEL: OnceLock<Regex> = OnceLock::new();
    static UPD: OnceLock<Regex> = OnceLock::new();
    static ALTER: OnceLock<Regex> = OnceLock::new();
    static WHERE: OnceLock<Regex> = OnceLock::new();
    for stmt in sql.split(';') {
        if re(&DROP, r"(?i)\bDROP\s+(TABLE|DATABASE|SCHEMA|VIEW|INDEX|USER|ROLE|FUNCTION|PROCEDURE|TRIGGER|SEQUENCE|TYPE|EXTENSION|MATERIALIZED|OWNED|COLUMN)\b").is_match(stmt) {
            return Some("SQL DROP".into());
        }
        if re(&TRUNC, r"(?i)\bTRUNCATE\b").is_match(stmt) {
            return Some("SQL TRUNCATE".into());
        }
        if re(&ALTER, r"(?i)\bALTER\s+TABLE\b[\s\S]*\bDROP\b").is_match(stmt) {
            return Some("SQL ALTER TABLE ... DROP".into());
        }
        let has_where = re(&WHERE, r"(?i)\bWHERE\b").is_match(stmt);
        if re(&DEL, r"(?i)\bDELETE\s+FROM\b").is_match(stmt) && !has_where {
            return Some("SQL DELETE without WHERE".into());
        }
        if re(&UPD, r"(?i)\bUPDATE\s+\S+\s+SET\b").is_match(stmt) && !has_where {
            return Some("SQL UPDATE without WHERE".into());
        }
    }
    None
}

fn base64_decode(s: &str) -> Option<String> {
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    String::from_utf8(out).ok()
}

fn hex_decode(s: &str) -> Option<String> {
    let h: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    if !h.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::new();
    for p in h.chunks(2) {
        let st = std::str::from_utf8(p).ok()?;
        out.push(u8::from_str_radix(st, 16).ok()?);
    }
    String::from_utf8(out).ok()
}

/// Entries in the directory matching a glob in the final path component.
fn glob_hits(p: &Path) -> Vec<PathBuf> {
    let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else { return vec![] };
    let pat = name.to_string_lossy().to_lowercase();
    let Ok(rd) = std::fs::read_dir(dir) else { return vec![] };
    let mut v = Vec::new();
    for e in rd.flatten().take(10_000) {
        let n = e.file_name().to_string_lossy().to_lowercase();
        if n.starts_with('.') && !pat.starts_with('.') {
            continue;
        }
        let pat2: String = pat.chars().map(|c| if c == '?' { '*' } else { c }).collect();
        let pat2 = if pat2.contains('[') { pat2.split('[').next().unwrap_or("").to_string() + "*" } else { pat2 };
        if paths::glob_match(&pat2, &n) {
            v.push(e.path());
        }
    }
    v
}

/// The command text of an Execute call: raw_input (string, or JSON `{command}` /
/// argv array as used by Codex), falling back to the title.
fn command_text(call: &ToolCall) -> Option<String> {
    if let Some(raw) = call.raw_input.as_deref() {
        let t = raw.trim();
        if (t.starts_with('{') || t.starts_with('['))
            && let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
        {
            {
                let pick = |v: &serde_json::Value| -> Option<String> {
                    match v {
                        serde_json::Value::String(s) => Some(s.clone()),
                        serde_json::Value::Array(a) => Some(a.iter().map(|x| shell::quote(&x.as_str().map(str::to_string).unwrap_or_else(|| x.to_string()))).collect::<Vec<_>>().join(" ")),
                        _ => None,
                    }
                };
                match &v {
                    serde_json::Value::Object(m) => {
                        for k in ["command", "cmd", "script", "commandLine", "argv", "args"] {
                            if let Some(s) = m.get(k).and_then(pick) {
                                return Some(s);
                            }
                        }
                        return None;
                    }
                    serde_json::Value::Array(_) => return pick(&v),
                    _ => {}
                }
            }
        }
        if !t.is_empty() {
            return Some(raw.to_string());
        }
    }
    let title = call.title.trim();
    if title.is_empty() { None } else { Some(title.to_string()) }
}

fn json_string_field(raw: &str, keys: &[&str]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()).map(str::to_string))
}

#[cfg(test)]
mod tests;
