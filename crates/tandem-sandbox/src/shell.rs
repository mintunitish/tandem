//! A small, defensive POSIX-shell lexer/parser used only for *classification*.
//!
//! It never executes anything and never panics: malformed input yields a best-effort
//! parse plus [`Warning`]s, and the classifier treats warnings as "Review at minimum".
//!
//! What it understands:
//! - command separators `;` `&&` `||` `&` newline, pipes `|` `|&`, subshell parens;
//! - single quotes, double quotes, backslash escapes, line continuations, `$'..'` ANSI-C
//!   strings (decoded, so `$'\x72\x6d'` becomes `rm`), `$".."`;
//! - `$VAR`, `${...}`, `$(...)`, backticks, `$((...))`, `<(...)`/`>(...)`;
//! - redirections incl. fd prefixes (`2>&1`), `&>`, here-docs (bodies captured) and
//!   here-strings;
//! - comments, and function definitions `name() { ...; }` (recorded).
//!
//! Command substitutions are *not* parsed here; their source text is kept in
//! [`Part::Sub`] and the classifier parses and evaluates them recursively.

/// Hard limits that keep classification linear-ish on hostile input.
pub const MAX_INPUT: usize = 256 * 1024;
pub const MAX_DEPTH: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    /// Literal text. `quoted` means it came from quotes or a backslash escape
    /// (so it is not subject to globbing or field splitting).
    Lit { text: String, quoted: bool },
    /// `$(...)`, backticks, `<(...)`, `>(...)`: the inner script source.
    Sub { script: String, quoted: bool },
    /// `$NAME`, `${...}`, `$((...))`. `name` is the variable name when known, or a
    /// marker like `(arith)`. `subs` are command substitutions nested inside.
    Expr { name: String, subs: Vec<String>, quoted: bool },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RawWord {
    pub parts: Vec<Part>,
}

impl RawWord {
    /// Literal text if the word has no expansions.
    pub fn literal(&self) -> Option<String> {
        let mut s = String::new();
        for p in &self.parts {
            match p {
                Part::Lit { text, .. } => s.push_str(text),
                _ => return None,
            }
        }
        Some(s)
    }
    pub fn is_quoted_anywhere(&self) -> bool {
        self.parts.iter().any(|p| match p {
            Part::Lit { quoted, .. } | Part::Sub { quoted, .. } | Part::Expr { quoted, .. } => *quoted,
        })
    }
    fn push_lit(&mut self, c: char, quoted: bool) {
        if let Some(Part::Lit { text, quoted: q }) = self.parts.last_mut()
            && *q == quoted
        {
            text.push(c);
            return;
        }
        self.parts.push(Part::Lit { text: c.to_string(), quoted });
    }
    fn push_str(&mut self, s: &str, quoted: bool) {
        for c in s.chars() {
            self.push_lit(c, quoted);
        }
        if s.is_empty() && quoted {
            // Preserve `''` as an (empty) quoted word.
            self.parts.push(Part::Lit { text: String::new(), quoted: true });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirKind {
    /// `>`, `>|`, `&>`, `>&file`
    Write,
    /// `>>`, `&>>`
    Append,
    /// `<`
    Read,
    /// `<>`
    ReadWrite,
    /// `>&N`, `<&N`, `>&-`
    Dup,
    /// `<<`, `<<-`: `target` is the delimiter, `body` the captured text.
    HereDoc,
    /// `<<<`: `target` is the string.
    HereString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redir {
    pub fd: Option<u32>,
    pub kind: RedirKind,
    pub target: RawWord,
    pub body: Option<RawWord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SimpleCmd {
    pub words: Vec<RawWord>,
    pub redirs: Vec<Redir>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pipeline {
    pub cmds: Vec<SimpleCmd>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Warning {
    Unterminated(&'static str),
    /// `\xNN`, `\uNNNN` or octal escapes: classic obfuscation.
    EscapeObfuscation,
    /// `$IFS` used, typically to smuggle spaces.
    Ifs,
    FunctionDef(String),
    TooLong,
    TooDeep,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    pub pipelines: Vec<Pipeline>,
    pub warnings: Vec<Warning>,
}

impl Script {
    pub fn functions(&self) -> impl Iterator<Item = &str> {
        self.warnings.iter().filter_map(|w| match w {
            Warning::FunctionDef(n) => Some(n.as_str()),
            _ => None,
        })
    }
}

/// Parse a shell script. `depth` is the substitution nesting level of the caller.
pub fn parse(src: &str, depth: usize) -> Script {
    let mut script = Script::default();
    if depth > MAX_DEPTH {
        script.warnings.push(Warning::TooDeep);
        return script;
    }
    let src = if src.len() > MAX_INPUT {
        script.warnings.push(Warning::TooLong);
        let mut end = MAX_INPUT;
        while !src.is_char_boundary(end) {
            end -= 1;
        }
        &src[..end]
    } else {
        src
    };
    if has_escape_obfuscation(src) {
        script.warnings.push(Warning::EscapeObfuscation);
    }
    let mut lx = Lexer { c: src.chars().collect(), pos: 0, warnings: Vec::new() };
    lx.run(&mut script);
    script.warnings.append(&mut lx.warnings);
    script
}

/// `\x41`, `A`, `\U...`, or 3-digit octal escapes anywhere in the text.
fn has_escape_obfuscation(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'\\' {
            let n = b[i + 1];
            if (n == b'x' || n == b'X') && b.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) {
                return true;
            }
            if (n == b'u' || n == b'U') && b.get(i + 2).is_some_and(|c| c.is_ascii_hexdigit()) {
                return true;
            }
            if (b'0'..=b'7').contains(&n)
                && b.get(i + 2).is_some_and(|c| (b'0'..=b'7').contains(c))
                && b.get(i + 3).is_some_and(|c| (b'0'..=b'7').contains(c))
            {
                return true;
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    false
}

struct PendingHeredoc {
    pipeline: usize,
    cmd: usize,
    redir: usize,
    delim: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Lexer {
    c: Vec<char>,
    pos: usize,
    warnings: Vec<Warning>,
}

fn is_blank(c: char) -> bool {
    c == ' ' || c == '\t' || c == '\r'
}
fn is_meta(c: char) -> bool {
    matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>' | '\n') || is_blank(c)
}
fn is_ident_start(c: char) -> bool {
    c == '_' || c.is_ascii_alphabetic()
}
fn is_ident(c: char) -> bool {
    c == '_' || c.is_ascii_alphanumeric()
}

impl Lexer {
    fn peek(&self) -> Option<char> {
        self.c.get(self.pos).copied()
    }
    fn peek_at(&self, off: usize) -> Option<char> {
        self.c.get(self.pos + off).copied()
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn slice(&self, a: usize, b: usize) -> String {
        let b = b.min(self.c.len());
        let a = a.min(b);
        self.c[a..b].iter().collect()
    }

    fn run(&mut self, script: &mut Script) {
        let mut pipe = Pipeline::default();
        let mut cmd = SimpleCmd::default();
        let mut pending: Vec<PendingHeredoc> = Vec::new();

        fn end_cmd(pipe: &mut Pipeline, cmd: &mut SimpleCmd) {
            if !cmd.words.is_empty() || !cmd.redirs.is_empty() {
                pipe.cmds.push(std::mem::take(cmd));
            }
        }
        fn end_pipe(script: &mut Script, pipe: &mut Pipeline, cmd: &mut SimpleCmd) {
            end_cmd(pipe, cmd);
            if !pipe.cmds.is_empty() {
                script.pipelines.push(std::mem::take(pipe));
            }
        }

        while let Some(ch) = self.peek() {
            if is_blank(ch) {
                self.pos += 1;
                continue;
            }
            match ch {
                '#' => {
                    while let Some(c) = self.peek() {
                        if c == '\n' {
                            break;
                        }
                        self.pos += 1;
                    }
                }
                '\\' if self.peek_at(1) == Some('\n') => self.pos += 2,
                '\n' => {
                    self.pos += 1;
                    end_pipe(script, &mut pipe, &mut cmd);
                    for h in pending.drain(..) {
                        let body = self.read_heredoc_body(&h);
                        if let Some(r) = script
                            .pipelines
                            .get_mut(h.pipeline)
                            .and_then(|p| p.cmds.get_mut(h.cmd))
                            .and_then(|c| c.redirs.get_mut(h.redir))
                        {
                            r.body = Some(body);
                        }
                    }
                }
                ';' => {
                    self.pos += 1;
                    self.eat(';');
                    self.eat('&');
                    end_pipe(script, &mut pipe, &mut cmd);
                }
                '&' => {
                    if self.peek_at(1) == Some('&') {
                        self.pos += 2;
                        end_pipe(script, &mut pipe, &mut cmd);
                    } else if self.peek_at(1) == Some('>') {
                        self.pos += 2;
                        let kind = if self.eat('>') { RedirKind::Append } else { RedirKind::Write };
                        let target = self.read_redir_target();
                        cmd.redirs.push(Redir { fd: None, kind, target, body: None });
                    } else {
                        self.pos += 1;
                        end_pipe(script, &mut pipe, &mut cmd);
                    }
                }
                '|' => {
                    if self.peek_at(1) == Some('|') {
                        self.pos += 2;
                        end_pipe(script, &mut pipe, &mut cmd);
                    } else {
                        self.pos += 1;
                        self.eat('&');
                        end_cmd(&mut pipe, &mut cmd);
                    }
                }
                '(' => {
                    // `name ( )` => function definition.
                    let save = self.pos;
                    self.pos += 1;
                    while self.peek().is_some_and(is_blank) {
                        self.pos += 1;
                    }
                    if self.peek() == Some(')') && cmd.words.len() == 1 && cmd.redirs.is_empty() {
                        self.pos += 1;
                        let name = cmd.words[0].literal().unwrap_or_default();
                        self.warnings.push(Warning::FunctionDef(name));
                        cmd = SimpleCmd::default();
                        end_pipe(script, &mut pipe, &mut cmd);
                    } else {
                        self.pos = save + 1;
                        end_pipe(script, &mut pipe, &mut cmd);
                    }
                }
                ')' => {
                    self.pos += 1;
                    end_pipe(script, &mut pipe, &mut cmd);
                }
                '<' | '>' if self.peek_at(1) == Some('(') => {
                    let w = self.read_word();
                    cmd.words.push(w);
                }
                '<' | '>' => {
                    if let Some(r) = self.read_redir(None) {
                        if r.kind == RedirKind::HereDoc {
                            let (delim, quoted) = match r.target.literal() {
                                Some(d) => (d, r.target.is_quoted_anywhere()),
                                None => (String::new(), true),
                            };
                            pending.push(PendingHeredoc {
                                pipeline: script.pipelines.len(),
                                cmd: pipe.cmds.len(),
                                redir: cmd.redirs.len(),
                                delim,
                                strip_tabs: r.fd == Some(u32::MAX),
                                quoted,
                            });
                            let mut r = r;
                            if r.fd == Some(u32::MAX) {
                                r.fd = None;
                            }
                            cmd.redirs.push(r);
                        } else {
                            cmd.redirs.push(r);
                        }
                    }
                }
                _ => {
                    let w = self.read_word();
                    // fd prefix: `2>file`, `2>&1`
                    if let Some(l) = w.literal()
                        && !l.is_empty()
                        && l.len() <= 4
                        && !w.is_quoted_anywhere()
                        && l.chars().all(|c| c.is_ascii_digit())
                        && matches!(self.peek(), Some('<') | Some('>'))
                        && self.peek_at(1) != Some('(')
                    {
                        if let Some(r) = self.read_redir(l.parse().ok()) {
                            cmd.redirs.push(r);
                        }
                        continue;
                    }
                    if w.parts.is_empty() {
                        // Defensive: never loop without progress.
                        self.pos += 1;
                        continue;
                    }
                    cmd.words.push(w);
                }
            }
        }
        end_pipe(script, &mut pipe, &mut cmd);
        if !pending.is_empty() {
            self.warnings.push(Warning::Unterminated("here-doc"));
        }
    }

    /// Reads a redirection operator at `pos`. Here-docs with `<<-` are flagged by
    /// returning `fd == Some(u32::MAX)` (internal marker, cleared by the caller).
    fn read_redir(&mut self, fd: Option<u32>) -> Option<Redir> {
        let ch = self.peek()?;
        self.pos += 1;
        let kind = if ch == '>' {
            if self.eat('>') {
                RedirKind::Append
            } else if self.eat('|') {
                RedirKind::Write
            } else if self.eat('&') {
                // `>&N`, `>&-` are dups; `>&file` writes.
                while self.peek().is_some_and(is_blank) {
                    self.pos += 1;
                }
                let t = self.read_word();
                let is_dup = t
                    .literal()
                    .is_some_and(|l| l == "-" || (!l.is_empty() && l.chars().all(|c| c.is_ascii_digit())));
                return Some(Redir {
                    fd,
                    kind: if is_dup { RedirKind::Dup } else { RedirKind::Write },
                    target: t,
                    body: None,
                });
            } else {
                RedirKind::Write
            }
        } else if self.eat('<') {
            if self.eat('<') {
                RedirKind::HereString
            } else {
                let strip = self.eat('-');
                let target = self.read_redir_target();
                return Some(Redir {
                    fd: if strip { Some(u32::MAX) } else { fd },
                    kind: RedirKind::HereDoc,
                    target,
                    body: None,
                });
            }
        } else if self.eat('>') {
            RedirKind::ReadWrite
        } else if self.eat('&') {
            let t = self.read_redir_target();
            return Some(Redir { fd, kind: RedirKind::Dup, target: t, body: None });
        } else {
            RedirKind::Read
        };
        let target = self.read_redir_target();
        Some(Redir { fd, kind, target, body: None })
    }

    fn read_redir_target(&mut self) -> RawWord {
        while self.peek().is_some_and(is_blank) {
            self.pos += 1;
        }
        match self.peek() {
            Some(c) if !is_meta(c) || (matches!(c, '<' | '>') && self.peek_at(1) == Some('(')) => self.read_word(),
            _ => {
                self.warnings.push(Warning::Unterminated("redirection target"));
                RawWord::default()
            }
        }
    }

    fn read_heredoc_body(&mut self, h: &PendingHeredoc) -> RawWord {
        let mut body = String::new();
        let mut found = false;
        while self.pos < self.c.len() {
            let start = self.pos;
            while self.peek().is_some_and(|c| c != '\n') {
                self.pos += 1;
            }
            let line = self.slice(start, self.pos);
            self.eat('\n');
            let cmp = if h.strip_tabs { line.trim_start_matches('\t') } else { line.as_str() };
            if cmp.trim_end_matches('\r') == h.delim {
                found = true;
                break;
            }
            body.push_str(cmp);
            body.push('\n');
        }
        if !found {
            self.warnings.push(Warning::Unterminated("here-doc"));
        }
        if h.quoted {
            let mut w = RawWord::default();
            w.push_str(&body, true);
            w
        } else {
            let mut sub = Lexer { c: body.chars().collect(), pos: 0, warnings: Vec::new() };
            let w = sub.read_expanding_text();
            self.warnings.append(&mut sub.warnings);
            w
        }
    }

    /// Reads to EOF with double-quote-like semantics (here-doc bodies, `${}` bodies).
    fn read_expanding_text(&mut self) -> RawWord {
        let mut w = RawWord::default();
        while let Some(c) = self.peek() {
            match c {
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        Some(n @ ('$' | '`' | '\\')) => {
                            self.pos += 1;
                            w.push_lit(n, true);
                        }
                        Some('\n') => self.pos += 1,
                        _ => w.push_lit('\\', true),
                    }
                }
                '$' => self.read_dollar(&mut w, true),
                '`' => self.read_backtick(&mut w, true),
                _ => {
                    self.pos += 1;
                    w.push_lit(c, true);
                }
            }
        }
        w
    }

    fn read_word(&mut self) -> RawWord {
        let mut w = RawWord::default();
        while let Some(c) = self.peek() {
            match c {
                c if is_blank(c) || c == '\n' => break,
                ';' | '&' | '|' | ')' => break,
                '(' => {
                    // Only reachable mid-word (e.g. `a(`); treat as operator boundary.
                    break;
                }
                '<' | '>' => {
                    if self.peek_at(1) == Some('(') {
                        // process substitution
                        self.pos += 2;
                        let inner = self.read_until_close_paren();
                        w.parts.push(Part::Sub { script: inner, quoted: false });
                    } else {
                        break;
                    }
                }
                '\\' => {
                    self.pos += 1;
                    match self.peek() {
                        Some('\n') => self.pos += 1,
                        Some(n) => {
                            self.pos += 1;
                            w.push_lit(n, true);
                        }
                        None => w.push_lit('\\', true),
                    }
                }
                '\'' => {
                    self.pos += 1;
                    let start = self.pos;
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.pos += 1;
                    }
                    let s = self.slice(start, self.pos);
                    if !self.eat('\'') {
                        self.warnings.push(Warning::Unterminated("single quote"));
                    }
                    w.push_str(&s, true);
                }
                '"' => {
                    self.pos += 1;
                    self.read_dquote(&mut w);
                }
                '$' => self.read_dollar(&mut w, false),
                '`' => self.read_backtick(&mut w, false),
                _ => {
                    self.pos += 1;
                    w.push_lit(c, false);
                }
            }
        }
        w
    }

    fn read_dquote(&mut self, w: &mut RawWord) {
        let mut any = false;
        loop {
            match self.peek() {
                None => {
                    self.warnings.push(Warning::Unterminated("double quote"));
                    break;
                }
                Some('"') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(n @ ('$' | '`' | '"' | '\\')) => {
                            self.pos += 1;
                            w.push_lit(n, true);
                        }
                        Some('\n') => self.pos += 1,
                        _ => w.push_lit('\\', true),
                    }
                    any = true;
                }
                Some('$') => {
                    self.read_dollar(w, true);
                    any = true;
                }
                Some('`') => {
                    self.read_backtick(w, true);
                    any = true;
                }
                Some(c) => {
                    self.pos += 1;
                    w.push_lit(c, true);
                    any = true;
                }
            }
        }
        if !any {
            w.parts.push(Part::Lit { text: String::new(), quoted: true });
        }
    }

    fn read_backtick(&mut self, w: &mut RawWord, quoted: bool) {
        self.pos += 1;
        let mut inner = String::new();
        loop {
            match self.peek() {
                None => {
                    self.warnings.push(Warning::Unterminated("backtick"));
                    break;
                }
                Some('`') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(n @ ('`' | '$' | '\\')) => {
                            self.pos += 1;
                            inner.push(n);
                        }
                        _ => inner.push('\\'),
                    }
                }
                Some(c) => {
                    self.pos += 1;
                    inner.push(c);
                }
            }
        }
        w.parts.push(Part::Sub { script: inner, quoted });
    }

    /// At `$`.
    fn read_dollar(&mut self, w: &mut RawWord, quoted: bool) {
        self.pos += 1;
        match self.peek() {
            Some('\'') if !quoted => {
                self.pos += 1;
                let s = self.read_ansi_c();
                w.push_str(&s, true);
            }
            Some('"') if !quoted => {
                self.pos += 1;
                self.read_dquote(w);
            }
            Some('(') if self.peek_at(1) == Some('(') => {
                self.pos += 2;
                let inner = self.read_until_close_paren();
                // inner includes the trailing `)` of `))`; harmless.
                let subs = self.subs_in(&inner);
                self.eat(')');
                w.parts.push(Part::Expr { name: "(arith)".into(), subs, quoted });
            }
            Some('(') => {
                self.pos += 1;
                let inner = self.read_until_close_paren();
                w.parts.push(Part::Sub { script: inner, quoted });
            }
            Some('{') => {
                self.pos += 1;
                let inner = self.read_until_close_brace();
                let name: String = inner
                    .trim_start_matches(['!', '#'])
                    .chars()
                    .take_while(|c| is_ident(*c) || matches!(c, '@' | '*' | '?' | '$'))
                    .collect();
                if inner.contains("IFS") {
                    self.warnings.push(Warning::Ifs);
                }
                let subs = self.subs_in(&inner);
                let simple = inner == name;
                w.parts.push(Part::Expr {
                    name: if simple { name } else { format!("{{{name}}}") },
                    subs,
                    quoted,
                });
            }
            Some(c) if is_ident_start(c) => {
                let start = self.pos;
                while self.peek().is_some_and(is_ident) {
                    self.pos += 1;
                }
                let name = self.slice(start, self.pos);
                if name == "IFS" {
                    self.warnings.push(Warning::Ifs);
                }
                w.parts.push(Part::Expr { name, subs: vec![], quoted });
            }
            Some(c) if c.is_ascii_digit() || matches!(c, '@' | '*' | '#' | '?' | '$' | '!' | '-') => {
                self.pos += 1;
                w.parts.push(Part::Expr { name: c.to_string(), subs: vec![], quoted });
            }
            _ => w.push_lit('$', quoted),
        }
    }

    /// Command substitutions nested in `${...}` / `$((...))` text.
    fn subs_in(&mut self, text: &str) -> Vec<String> {
        if !text.contains('$') && !text.contains('`') {
            return vec![];
        }
        let mut sub = Lexer { c: text.chars().collect(), pos: 0, warnings: Vec::new() };
        let w = sub.read_expanding_text();
        self.warnings.append(&mut sub.warnings);
        let mut out = Vec::new();
        for p in w.parts {
            match p {
                Part::Sub { script, .. } => out.push(script),
                Part::Expr { subs, .. } => out.extend(subs),
                Part::Lit { .. } => {}
            }
        }
        out
    }

    fn read_ansi_c(&mut self) -> String {
        let mut s = String::new();
        loop {
            match self.peek() {
                None => {
                    self.warnings.push(Warning::Unterminated("ansi-c quote"));
                    break;
                }
                Some('\'') => {
                    self.pos += 1;
                    break;
                }
                Some('\\') => {
                    self.pos += 1;
                    let Some(n) = self.peek() else {
                        s.push('\\');
                        continue;
                    };
                    self.pos += 1;
                    match n {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        'a' => s.push('\u{7}'),
                        'b' => s.push('\u{8}'),
                        'e' | 'E' => s.push('\u{1b}'),
                        'f' => s.push('\u{c}'),
                        'v' => s.push('\u{b}'),
                        '\\' | '\'' | '"' | '?' => s.push(n),
                        'x' => {
                            let v = self.read_radix(16, 2);
                            s.push(v.and_then(char::from_u32).unwrap_or('\u{fffd}'));
                        }
                        'u' | 'U' => {
                            let v = self.read_radix(16, if n == 'u' { 4 } else { 8 });
                            s.push(v.and_then(char::from_u32).unwrap_or('\u{fffd}'));
                        }
                        '0'..='7' => {
                            self.pos -= 1;
                            let v = self.read_radix(8, 3);
                            s.push(v.and_then(char::from_u32).unwrap_or('\u{fffd}'));
                        }
                        'c' => {
                            if let Some(x) = self.peek() {
                                self.pos += 1;
                                s.push(char::from_u32((x as u32) & 0x1f).unwrap_or('\u{fffd}'));
                            }
                        }
                        other => {
                            s.push('\\');
                            s.push(other);
                        }
                    }
                }
                Some(c) => {
                    self.pos += 1;
                    s.push(c);
                }
            }
        }
        s
    }

    fn read_radix(&mut self, radix: u32, max: usize) -> Option<u32> {
        let mut v: u32 = 0;
        let mut n = 0;
        while n < max {
            match self.peek().and_then(|c| c.to_digit(radix)) {
                Some(d) => {
                    v = v.saturating_mul(radix).saturating_add(d);
                    self.pos += 1;
                    n += 1;
                }
                None => break,
            }
        }
        if n == 0 { None } else { Some(v) }
    }

    /// After an opening `(`; returns inner text and consumes the matching `)`.
    fn read_until_close_paren(&mut self) -> String {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.pos += 2,
                '\'' => {
                    self.pos += 1;
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                '"' => {
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        if c == '\\' {
                            self.pos += 2;
                            continue;
                        }
                        if c == '"' {
                            break;
                        }
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                '(' => {
                    depth += 1;
                    self.pos += 1;
                }
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        let s = self.slice(start, self.pos);
                        self.pos += 1;
                        return s;
                    }
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        self.pos = self.pos.min(self.c.len());
        self.warnings.push(Warning::Unterminated("command substitution"));
        self.slice(start, self.c.len())
    }

    /// After `${`; returns inner text and consumes the matching `}`.
    fn read_until_close_brace(&mut self) -> String {
        let start = self.pos;
        let mut depth = 1usize;
        while let Some(c) = self.peek() {
            match c {
                '\\' => self.pos += 2,
                '\'' => {
                    self.pos += 1;
                    while self.peek().is_some_and(|c| c != '\'') {
                        self.pos += 1;
                    }
                    self.pos += 1;
                }
                '{' => {
                    depth += 1;
                    self.pos += 1;
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        let s = self.slice(start, self.pos);
                        self.pos += 1;
                        return s;
                    }
                    self.pos += 1;
                }
                _ => self.pos += 1,
            }
        }
        self.pos = self.pos.min(self.c.len());
        self.warnings.push(Warning::Unterminated("parameter expansion"));
        self.slice(start, self.c.len())
    }
}

/// Quote a single argument for POSIX sh (used to rebuild argv arrays into a script).
pub fn quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '=' | ':' | ',' | '+' | '@' | '%'))
    {
        return arg.to_string();
    }
    format!("'{}'", arg.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &Script) -> Vec<Vec<String>> {
        s.pipelines
            .iter()
            .flat_map(|p| p.cmds.iter())
            .map(|c| c.words.iter().map(|w| w.literal().unwrap_or_else(|| "<dyn>".into())).collect())
            .collect()
    }

    #[test]
    fn splits_operators() {
        let s = parse("a b; c && d || e | f & g\nh", 0);
        assert_eq!(s.pipelines.len(), 6);
        assert_eq!(words(&s), vec![vec!["a", "b"], vec!["c"], vec!["d"], vec!["e"], vec!["f"], vec!["g"], vec!["h"]]);
    }

    #[test]
    fn quotes_and_escapes() {
        let s = parse(r#"r'm' "-r"f \/ $'\x72m'"#, 0);
        assert_eq!(words(&s), vec![vec!["rm", "-rf", "/", "rm"]]);
        assert!(s.warnings.contains(&Warning::EscapeObfuscation));
    }

    #[test]
    fn substitutions_captured() {
        let s = parse("echo $(rm -rf /) `ls` ${X:-$(id)}", 0);
        let c = &s.pipelines[0].cmds[0];
        assert!(matches!(&c.words[1].parts[0], Part::Sub { script, .. } if script == "rm -rf /"));
        assert!(matches!(&c.words[2].parts[0], Part::Sub { script, .. } if script == "ls"));
        assert!(matches!(&c.words[3].parts[0], Part::Expr { subs, .. } if subs == &vec!["id".to_string()]));
    }

    #[test]
    fn redirs_and_heredoc() {
        let s = parse("cat <<'EOF' > out 2>&1\nrm -rf /\nEOF\necho hi", 0);
        let c = &s.pipelines[0].cmds[0];
        assert_eq!(c.redirs.len(), 3);
        assert_eq!(c.redirs[0].kind, RedirKind::HereDoc);
        assert_eq!(c.redirs[0].body.as_ref().and_then(|b| b.literal()).as_deref(), Some("rm -rf /\n"));
        assert_eq!(c.redirs[1].kind, RedirKind::Write);
        assert_eq!(c.redirs[2].kind, RedirKind::Dup);
        assert_eq!(s.pipelines.len(), 2);
    }

    #[test]
    fn function_def() {
        let s = parse(":(){ :|:& };:", 0);
        assert!(s.warnings.contains(&Warning::FunctionDef(":".into())));
    }

    #[test]
    fn never_panics_on_garbage() {
        for s in ["$(", "${", "'", "\"", "`", "<<", "2>", "$'\\", "$((", "\\", "a(", ")", "<(", "$'\\x'", "$'\\u{'"] {
            let _ = parse(s, 0);
        }
    }
}
