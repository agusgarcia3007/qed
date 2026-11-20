//! Visual language of the human renderer. One rule: the model's words are the content;
//! everything the harness adds is dim chrome. A single accent (gold ∎) marks qed itself,
//! green/red only mark outcomes.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const RST: &str = "\x1b[0m";
pub const OK: &str = "\x1b[32m";
pub const BAD: &str = "\x1b[31m";
pub const ACCENT: &str = "\x1b[38;5;220m";
const CODE: &str = "\x1b[38;5;180m";

pub fn stdout_tty() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    // SAFETY: isatty only inspects the descriptor.
    *T.get_or_init(|| unsafe { libc::isatty(1) == 1 })
}

fn cwd() -> &'static PathBuf {
    static C: OnceLock<PathBuf> = OnceLock::new();
    C.get_or_init(|| std::env::current_dir().unwrap_or_default())
}

/// Path as the user thinks of it: relative to cwd, or with ~ for home.
pub fn rel(p: &str) -> String {
    let path = std::path::Path::new(p);
    if let Ok(r) = path.strip_prefix(cwd()) {
        let s = r.to_string_lossy();
        return if s.is_empty() {
            ".".into()
        } else {
            s.into_owned()
        };
    }
    home(p)
}

pub fn home(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// Cut to `w` display columns with an ellipsis.
pub fn fit(s: &str, w: usize) -> String {
    if dw(s) <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut n = 0;
    for c in s.chars() {
        if n + cw(c) + 1 > w {
            break;
        }
        n += cw(c);
        out.push(c);
    }
    out.push('…');
    out
}

fn first_line(s: &str) -> &str {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
}

/// One dim line per tool call, without colors; the caller wraps it.
pub fn tool_line(name: &str, i: &Value) -> String {
    let s = |k: &str| i[k].as_str().unwrap_or("");
    match name {
        "Bash" => format!("$ {}", first_line(s("command"))),
        "Read" => format!("read {}", rel(s("file_path"))),
        "Grep" => format!("grep {}", s("pattern")),
        "Glob" => format!("glob {}", s("pattern")),
        "WebSearch" => format!("search {}", s("query")),
        "WebFetch" => format!("fetch {}", s("url")),
        "Write" => format!(
            "+ {} · {} lines",
            rel(s("file_path")),
            s("content").lines().count()
        ),
        "Edit" => {
            let (a, d) = edit_counts(s("old_string"), s("new_string"));
            format!("~ {} +{} −{}", rel(s("file_path")), a, d)
        }
        "TodoWrite" => "plan updated".into(),
        "Task" | "Agent" => format!("subagent {}", s("description")),
        n => n.to_string(),
    }
}

/// The changed lines of an Edit, red/green, at most `max` rows (each fit to `w`).
pub fn edit_preview(i: &Value, w: usize, max: usize) -> String {
    let old = i["old_string"].as_str().unwrap_or("");
    let new = i["new_string"].as_str().unwrap_or("");
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let rows: Vec<(char, &str)> = a[pre..a.len() - suf]
        .iter()
        .map(|l| ('-', *l))
        .chain(b[pre..b.len() - suf].iter().map(|l| ('+', *l)))
        .collect();
    let mut out = String::new();
    for (sign, l) in rows.iter().take(max) {
        let col = if *sign == '-' { "\x1b[38;5;174m" } else { "\x1b[38;5;114m" };
        let l = l.replace('\t', "    ");
        out.push_str(&format!("    {}{} {}{}\n", col, sign, fit(&l, w.saturating_sub(2)), RST));
    }
    if rows.len() > max {
        out.push_str(&format!("    {}… {} more{}\n", DIM, rows.len() - max, RST));
    }
    out
}

/// Lines added/removed by an Edit, ignoring the common prefix/suffix.
pub fn edit_counts(old: &str, new: &str) -> (usize, usize) {
    let a: Vec<&str> = old.lines().collect();
    let b: Vec<&str> = new.lines().collect();
    let pre = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suf = a[pre..]
        .iter()
        .rev()
        .zip(b[pre..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (b.len() - pre - suf, a.len() - pre - suf)
}

/// Display columns of one char: 0 for combining marks, 2 for wide (CJK, emoji), else 1.
pub fn cw(c: char) -> usize {
    let u = c as u32;
    match u {
        0x0300..=0x036f | 0x200b..=0x200f | 0xfe00..=0xfe0f | 0x20d0..=0x20ff => 0,
        0x1100..=0x115f
        | 0x2e80..=0xa4cf
        | 0xac00..=0xd7a3
        | 0xf900..=0xfaff
        | 0xfe30..=0xfe4f
        | 0xff00..=0xff60
        | 0xffe0..=0xffe6
        | 0x1f300..=0x1f64f
        | 0x1f900..=0x1faff
        | 0x20000..=0x3fffd => 2,
        _ => 1,
    }
}

/// Display width of a string without escape sequences.
pub fn dw(s: &str) -> usize {
    s.chars().map(cw).sum()
}

/// Streaming markdown for the model's voice. Text arrives in arbitrary deltas; words are
/// written as soon as they are complete, wrapped at word boundaries by qed itself (so the
/// row count is known and nothing is ever redrawn), with block prefixes (headings,
/// bullets, numbers, quotes) decided from the first chars of each line and inline
/// **bold** / `code` applied as state. Fenced code and tables are written per line.
#[derive(Default)]
pub struct Md {
    /// Visual column of the cursor on the current row.
    pub col: usize,
    /// Rows started (newlines written) so far.
    pub rows: usize,
    indent: usize,
    head: String,
    started: bool,
    word: String,
    bold: bool,
    code: bool,
    fence: bool,
    raw: Option<String>,
    base: &'static str,
}

impl Md {
    pub fn at_bol(&self) -> bool {
        self.col == 0 && self.head.is_empty() && self.raw.is_none() && self.word.is_empty()
    }

    /// The SGR state to re-apply after something else printed mid-line.
    pub fn sgr(&self) -> String {
        let mut s = String::from(RST);
        s.push_str(self.base);
        if self.bold {
            s.push_str(BOLD);
        }
        if self.code {
            s.push_str(CODE);
        }
        s
    }

    pub fn push(&mut self, t: &str, w: usize) -> String {
        let mut out = String::new();
        for c in t.chars() {
            self.ch(c, w, &mut out);
        }
        out
    }

    /// Finish the current line if anything is on it.
    pub fn end(&mut self, w: usize) -> String {
        let mut out = String::new();
        if !self.at_bol() {
            self.newline(w, &mut out);
        }
        out
    }

    /// Write out what is pending (a pause in the stream) without ending the line. A
    /// trailing `*` or backtick is kept back: it may be half of a mark.
    pub fn flush(&mut self, w: usize) -> String {
        let mut out = String::new();
        if self.raw.is_some() {
            return out;
        }
        if !self.started && !self.head.is_empty() {
            self.decide(true, w, &mut out);
        }
        if self.started && !self.word.is_empty() {
            let keep = self.word.len() - self.word.trim_end_matches(['*', '`']).len();
            let tail = self.word.split_off(self.word.len() - keep);
            self.emit_word(w, &mut out);
            self.word = tail;
        }
        out
    }

    fn ch(&mut self, c: char, w: usize, out: &mut String) {
        if c == '\n' {
            self.newline(w, out);
            return;
        }
        if let Some(r) = self.raw.as_mut() {
            r.push(c);
            return;
        }
        if !self.started {
            if self.head.is_empty() && self.fence {
                self.raw = Some(c.to_string());
                return;
            }
            self.head.push(c);
            self.decide(false, w, out);
            return;
        }
        if c == ' ' {
            self.emit_word(w, out);
            if self.col > self.indent && self.col < w {
                out.push(' ');
                self.col += 1;
            }
        } else {
            self.word.push(c);
        }
    }

    /// Classify the line from its first chars once that is unambiguous (or forced).
    fn decide(&mut self, force: bool, w: usize, out: &mut String) {
        let h = self.head.trim_start().to_string();
        let lead = self.head.len() - h.len();
        if h.is_empty() {
            if force || lead > 12 {
                self.start(lead, "", 0, "", w, out);
            }
            return;
        }
        let first = h.chars().next().unwrap_or(' ');
        let rest_after = |n: usize| h.get(n..).unwrap_or("").to_string();
        // (prefix shown, marker chars consumed, base style)
        let mut pick: Option<(String, usize, &'static str)> = None;
        let mut wait = false;
        match first {
            '`' if h.starts_with("```") || (!force && "``".starts_with(&h)) => {
                if h.starts_with("```") {
                    self.raw = Some(std::mem::take(&mut self.head));
                    return;
                }
                wait = true;
            }
            '|' => {
                self.raw = Some(std::mem::take(&mut self.head));
                return;
            }
            '#' => {
                let n = h.chars().take_while(|&c| c == '#').count();
                match h.chars().nth(n) {
                    None if !force && n <= 6 => wait = true,
                    Some(' ') if n <= 6 => pick = Some((String::new(), n + 1, BOLD)),
                    _ => {}
                }
            }
            '-' | '*' | '+' => match h.chars().nth(1) {
                None if !force => wait = true,
                Some(' ') => pick = Some((format!("{}•{} ", DIM, RST), 2, "")),
                Some(c) if c == first && first != '+' => {
                    // `---` / `***` rule, or `**bold`.
                    if first == '-' || h.starts_with("***") {
                        self.raw = Some(std::mem::take(&mut self.head));
                        return;
                    }
                    if !force && h.len() < 3 {
                        wait = true;
                    }
                }
                _ => {}
            },
            '>' => match h.chars().nth(1) {
                None if !force => wait = true,
                Some(' ') => pick = Some((format!("{}│{} ", DIM, RST), 2, DIM)),
                _ => pick = Some((format!("{}│{} ", DIM, RST), 1, DIM)),
            },
            '0'..='9' => {
                let n = h.chars().take_while(|c| c.is_ascii_digit()).count();
                match (h.chars().nth(n), h.chars().nth(n + 1)) {
                    (None, _) if !force && n < 4 => wait = true,
                    (Some('.') | Some(')'), None) if !force => wait = true,
                    (Some('.') | Some(')'), Some(' ')) => {
                        pick = Some((format!("{}{}{} ", DIM, &h[..n + 1], RST), n + 2, ""))
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if wait && !force {
            return;
        }
        let (prefix, used, base) = pick.unwrap_or((String::new(), 0, ""));
        let pw = if used == 0 { 0 } else { dw(&strip_sgr(&prefix)) };
        let rest = rest_after(used);
        self.head.clear();
        self.start(lead, &prefix, pw, base, w, out);
        for c in rest.chars() {
            self.ch(c, w, out);
        }
    }

    fn start(&mut self, lead: usize, prefix: &str, pw: usize, base: &'static str, w: usize, out: &mut String) {
        let lead = lead.min(w / 3);
        out.push_str(&" ".repeat(lead));
        out.push_str(prefix);
        out.push_str(base);
        self.base = base;
        self.col = lead + pw;
        self.indent = self.col;
        self.started = true;
        self.head.clear();
    }

    fn wrap(&mut self, out: &mut String) {
        out.push_str(RST);
        out.push('\n');
        out.push_str(&" ".repeat(self.indent));
        out.push_str(&self.sgr()[RST.len()..]);
        self.rows += 1;
        self.col = self.indent;
    }

    /// Visible width of a word once its marks are applied.
    fn vis(&self, word: &str) -> usize {
        let (mut b, mut c) = (self.bold, self.code);
        let mut n = 0;
        let ch: Vec<char> = word.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            if let Some(k) = mark_at(&ch, i, b, c) {
                if k == 2 {
                    b = !b;
                } else {
                    c = !c;
                }
                i += k;
                continue;
            }
            n += cw(ch[i]);
            i += 1;
        }
        n
    }

    fn emit_word(&mut self, w: usize, out: &mut String) {
        if self.word.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word);
        if self.col > self.indent && self.col + self.vis(&word) > w {
            // Drop the space that preceded the word on the previous row.
            if out.ends_with(' ') {
                out.pop();
            }
            self.wrap(out);
        }
        let ch: Vec<char> = word.chars().collect();
        let mut i = 0;
        while i < ch.len() {
            if let Some(k) = mark_at(&ch, i, self.bold, self.code) {
                if k == 2 {
                    self.bold = !self.bold;
                } else {
                    self.code = !self.code;
                }
                out.push_str(&self.sgr());
                i += k;
                continue;
            }
            let x = cw(ch[i]);
            if self.col + x > w && self.col > self.indent {
                self.wrap(out);
            }
            out.push(ch[i]);
            self.col += x;
            i += 1;
        }
    }

    fn newline(&mut self, w: usize, out: &mut String) {
        if let Some(r) = self.raw.take() {
            self.raw_line(&r, w, out);
        } else {
            if !self.started && !self.head.is_empty() {
                self.decide(true, w, out);
            }
            if let Some(r) = self.raw.take() {
                self.raw_line(&r, w, out);
            }
            self.emit_word(w, out);
        }
        if self.bold || self.code || !self.base.is_empty() {
            out.push_str(RST);
        }
        out.push('\n');
        self.rows += 1;
        self.col = 0;
        self.indent = 0;
        self.started = false;
        self.bold = false;
        self.code = false;
        self.base = "";
        self.head.clear();
    }

    /// Fences, code, tables and rules: one line at a time, hard-wrapped at `w`.
    fn raw_line(&mut self, line: &str, w: usize, out: &mut String) {
        let t = line.trim_start();
        let (style, shown): (&str, String) = if t.starts_with("```") {
            self.fence = !self.fence;
            let lang = t.trim_start_matches('`').trim();
            let rule = "─".repeat(w.min(40).saturating_sub(dw(lang) + 1));
            if self.fence && !lang.is_empty() {
                (DIM, format!("{} {}", lang, rule))
            } else {
                (DIM, "─".repeat(w.min(40)))
            }
        } else if self.fence {
            (CODE, format!("  {}", line))
        } else if t.starts_with('|') {
            let sep = t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '));
            if sep {
                (DIM, t.replace('|', "┼").replace(['-', ':'], "─").trim_matches('┼').to_string())
            } else {
                let cells: Vec<String> = t
                    .trim_matches('|')
                    .split('|')
                    .map(|c| strip_marks(c.trim()))
                    .collect();
                ("", cells.join(&format!(" {}│{} ", DIM, RST)))
            }
        } else {
            (DIM, "─".repeat(w.min(40)))
        };
        out.push_str(style);
        let mut col = 0;
        let mut in_esc = false;
        for c in shown.chars() {
            if c == '\x1b' {
                in_esc = true;
            }
            if in_esc {
                out.push(c);
                if c == 'm' {
                    in_esc = false;
                    out.push_str(style);
                }
                continue;
            }
            let x = cw(c);
            if col + x > w {
                out.push_str(RST);
                out.push('\n');
                out.push_str(style);
                self.rows += 1;
                col = 0;
            }
            out.push(c);
            col += x;
        }
        out.push_str(RST);
        self.col = col;
    }
}

/// Length of an inline mark starting at `i` (2 = `**`, 1 = backtick), if it is one:
/// an opening mark needs a non-space after it, a closing one a non-space before it.
fn mark_at(ch: &[char], i: usize, bold: bool, code: bool) -> Option<usize> {
    let solid = |j: Option<&char>| j.is_some_and(|c| !c.is_whitespace());
    if ch[i] == '`' {
        return Some(1);
    }
    if !code && ch[i] == '*' && ch.get(i + 1) == Some(&'*') {
        let ok = if bold {
            i > 0
        } else {
            solid(ch.get(i + 2))
        };
        return ok.then_some(2);
    }
    None
}

/// Inline marks removed (for table cells, which are not wrapped).
fn strip_marks(s: &str) -> String {
    s.replace("**", "").replace('`', "")
}

fn strip_sgr(s: &str) -> String {
    let mut out = String::new();
    let mut esc = false;
    for c in s.chars() {
        match (esc, c) {
            (_, '\x1b') => esc = true,
            (true, 'm') => esc = false,
            (true, _) => {}
            _ => out.push(c),
        }
    }
    out
}

/// Relative age of a past instant (ms since the epoch): now, 5m ago, 3h ago, 2d ago.
pub fn ago(ms: u128) -> String {
    let s = (crate::store::now_ms().saturating_sub(ms) / 1000) as u64;
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m ago", s / 60),
        3600..=86399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86400),
    }
}

/// Dollars like a person reads them: $1.24, $0.012, $0.0004, $0.
pub fn money(x: f64) -> String {
    match x {
        x if x <= 0.0 => "$0".into(),
        x if x >= 1.0 => format!("${:.2}", x),
        x if x >= 0.01 => format!("${:.3}", x),
        x => format!("${:.4}", x),
    }
}

/// Token counts: 840, 12.4k, 1.2M.
pub fn tokens(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

/// Token counts short enough for a status line: 950, 9.5k, 23k, 200k, 1M, 1.5M.
fn ktok(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=9_999 => format!("{:.1}k", n as f64 / 1e3),
        10_000..=999_999 => format!("{}k", (n + 500) / 1000),
        _ => {
            let m = format!("{:.1}", n as f64 / 1e6);
            format!("{}M", m.trim_end_matches(".0"))
        }
    }
}

/// Context meter: `ctx 23k/200k (12%)`, or `ctx 23k` when the window is unknown.
pub fn ctx(used: u64, window: u64) -> String {
    if window == 0 {
        return format!("ctx {}", ktok(used));
    }
    let pct = (used as f64 * 100.0 / window as f64).round() as u64;
    format!("ctx {}/{} ({}%)", ktok(used), ktok(window), pct)
}

/// Seconds like a person reads them: 0.4s, 3.2s, 42s, 3m05s.
pub fn secs(s: f64) -> String {
    match s {
        x if x < 10.0 => format!("{:.1}s", x),
        x if x < 60.0 => format!("{:.0}s", x),
        x => format!("{}m{:02}s", (x / 60.0) as u64, (x % 60.0) as u64),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn context_meter_reads_at_a_glance() {
        assert_eq!(ctx(23_456, 200_000), "ctx 23k/200k (12%)");
        assert_eq!(ctx(9_500, 1_000_000), "ctx 9.5k/1M (1%)");
        assert_eq!(ctx(180_000, 200_000), "ctx 180k/200k (90%)");
        assert_eq!(ctx(1_500_000, 0), "ctx 1.5M");
        assert_eq!(ctx(950, 0), "ctx 950");
    }

    fn md(chunks: &[&str], w: usize) -> String {
        let mut m = Md::default();
        let mut out = String::new();
        for c in chunks {
            out.push_str(&m.push(c, w));
        }
        out.push_str(&m.end(w));
        strip_sgr(&out)
    }

    #[test]
    fn markdown_streams_and_wraps() {
        assert_eq!(md(&["## Pl", "an\n"], 80), "Plan\n");
        assert_eq!(md(&["- usa `x` y *", "*y**\n"], 80), "• usa x y y\n");
        assert_eq!(md(&["a ** b\n"], 80), "a ** b\n");
        assert_eq!(md(&["12. paso\n"], 80), "12. paso\n");
        assert_eq!(md(&["```rust\nlet a = 1;\n```\n"], 80), "rust ───────────────────────────────────\n  let a = 1;\n────────────────────────────────────────\n");
        assert_eq!(md(&["uno dos tres cuatro\n"], 9), "uno dos\ntres\ncuatro\n");
        assert_eq!(md(&["- uno dos tres\n"], 9), "• uno dos\n  tres\n");
        assert_eq!(md(&["| a | **b** |\n|---|---|\n"], 80), "a │ b\n─┼─\n".replace("─┼─", "───┼───").as_str());
        assert_eq!(dw("año 日本"), 8);
    }

    #[test]
    fn tool_lines_are_short_and_relative() {
        let p = cwd().join("src/a.rs");
        let e = json!({"file_path": p, "old_string": "a\nb\nc", "new_string": "a\nB\nB2\nc"});
        assert_eq!(tool_line("Edit", &e), "~ src/a.rs +2 −1");
        assert_eq!(
            tool_line("Bash", &json!({"command": "\ncargo test\nmore"})),
            "$ cargo test"
        );
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(secs(3.21), "3.2s");
        assert_eq!(secs(125.0), "2m05s");
    }
}
