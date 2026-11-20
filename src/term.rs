//! Terminal input without dependencies: raw mode via termios, a small line editor
//! (history, cursor keys, bracketed paste, slash completion) and a key watcher that
//! runs during a turn (Esc/Ctrl-C cancel, typed text is queued for the next turn).

use crate::engine::CANCEL;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub static TYPEAHEAD: Mutex<String> = Mutex::new(String::new());
pub static QUEUE: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// While a permission card is up, the key watcher hands the next key here.
static ASKING: AtomicBool = AtomicBool::new(false);
static ANSWER: Mutex<Option<u8>> = Mutex::new(None);

/// Wait for one answer key during a turn (Esc = n; Ctrl-C = n and cancel the turn).
pub fn ask_key() -> u8 {
    if let Ok(mut a) = ANSWER.lock() {
        *a = None;
    }
    ASKING.store(true, Ordering::SeqCst);
    let k = loop {
        if let Some(k) = ANSWER.lock().ok().and_then(|mut a| a.take()) {
            break k;
        }
        if CANCEL.load(Ordering::SeqCst) {
            break b'n';
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    ASKING.store(false, Ordering::SeqCst);
    k
}

/// Dim hint row under the empty prompt (settings and shortcuts); erased on submit.
pub static FOOTER: Mutex<String> = Mutex::new(String::new());

pub const COMMANDS: &[(&str, &str)] = &[
    ("/model", "model"),
    ("/effort", "reasoning effort"),
    ("/mode", "native · min · adaptive"),
    ("/check", "command that verifies the work"),
    ("/budget", "cap in USD [and seconds]"),
    (
        "/candidates",
        "1 = single attempt · 3 = deep mode (parallel + selection)",
    ),
    ("/critic", "independent reviewer on/off"),
    ("/alt", "alternative branch on/off"),
    ("/web", "web search on/off"),
    ("/subagents", "Claude Code subagents (Explore…) on/off"),
    ("/goal", "keep working until an objective is met · /goal clear"),
    ("/undo", "revert the files changed by the last turn"),
    ("/perm", "ask before shell/web · auto"),
    ("/config", "current settings"),
    ("/new", "new session"),
    ("/resume", "resume a session (pick with ↑↓)"),
    ("/status", "evidence and valid checks"),
    ("/diff", "session changes"),
    ("/cost", "session cost"),
    ("/fresh", "new session with summarized context"),
    ("/logo", "∎"),
    ("/help", "help"),
    ("/quit", "quit"),
];

pub const MODELS: &[&str] = &[
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "claude-opus-5-5",
    "claude-sonnet-5-5",
    "claude-haiku-5-5",
    "claude-fable-5-2",
];
pub const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

/// A user or project skill (`.claude/skills/<name>/SKILL.md`) or custom command
/// (`.claude/commands/<name>.md`), invoked as `/<name> [args]`.
pub struct Skill {
    pub name: String,
    pub desc: String,
    pub file: PathBuf,
    /// Skill directory (for references next to SKILL.md); None for plain commands.
    pub dir: Option<PathBuf>,
}

static SKILLS: std::sync::OnceLock<Vec<Skill>> = std::sync::OnceLock::new();

/// Scan project then user skills/commands once; the project wins on a name clash and
/// builtin commands always win.
pub fn load_skills(root: &std::path::Path) {
    let mut bases = vec![root.join(".claude")];
    if let Some(h) = std::env::var_os("HOME") {
        bases.push(PathBuf::from(h).join(".claude"));
    }
    let mut out: Vec<Skill> = Vec::new();
    let mut add = |name: String, file: PathBuf, dir: Option<PathBuf>| {
        let cmd = format!("/{}", name);
        if name.is_empty()
            || COMMANDS.iter().any(|(c, _)| *c == cmd)
            || out.iter().any(|s| s.name == name)
        {
            return;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            return;
        };
        out.push(Skill {
            name,
            desc: front_desc(&text),
            file,
            dir,
        });
    };
    for b in &bases {
        let mut found: Vec<(String, PathBuf, Option<PathBuf>)> = Vec::new();
        for e in std::fs::read_dir(b.join("skills"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let f = e.path().join("SKILL.md");
            if f.is_file() {
                found.push((e.file_name().to_string_lossy().into(), f, Some(e.path())));
            }
        }
        for e in std::fs::read_dir(b.join("commands"))
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md") {
                let n = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                found.push((n, p, None));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        for (n, f, d) in found {
            add(n, f, d);
        }
    }
    // Installed plugins (Claude Code's `/plugin:skill` names): user scope, or this project.
    if let Some(h) = std::env::var_os("HOME") {
        let reg = PathBuf::from(h).join(".claude/plugins/installed_plugins.json");
        let v: serde_json::Value = std::fs::read_to_string(reg)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        for (key, installs) in v["plugins"].as_object().into_iter().flatten() {
            let plugin = key.split('@').next().unwrap_or(key);
            for i in installs.as_array().into_iter().flatten() {
                let here = i["projectPath"]
                    .as_str()
                    .is_none_or(|p| std::path::Path::new(p) == root);
                let Some(dir) = i["installPath"].as_str().filter(|_| here) else {
                    continue;
                };
                let dir = PathBuf::from(dir);
                for e in std::fs::read_dir(dir.join("skills")).into_iter().flatten().flatten() {
                    let f = e.path().join("SKILL.md");
                    if f.is_file() {
                        let n = format!("{}:{}", plugin, e.file_name().to_string_lossy());
                        add(n, f, Some(e.path()));
                    }
                }
                for e in std::fs::read_dir(dir.join("commands")).into_iter().flatten().flatten() {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "md") {
                        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                        add(format!("{}:{}", plugin, stem), p, None);
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = SKILLS.set(out);
}

pub fn skills() -> &'static [Skill] {
    SKILLS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

fn find_skill(cmd: &str) -> Option<&'static Skill> {
    let n = cmd.strip_prefix('/')?;
    skills().iter().find(|s| s.name == n)
}

/// Split YAML front matter from the body: (front matter, body).
fn split_front(text: &str) -> (&str, &str) {
    if let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    {
        if let Some(i) = rest.find("\n---") {
            let body = rest[i + 4..].trim_start_matches(['\r', '\n']);
            return (&rest[..i], body);
        }
    }
    ("", text)
}

/// `description:` from the front matter (single line, quoted, or a folded block).
fn front_desc(text: &str) -> String {
    let (fm, _) = split_front(text);
    let mut lines = fm.lines();
    while let Some(l) = lines.next() {
        let Some(v) = l.strip_prefix("description:") else {
            continue;
        };
        let v = v.trim();
        if v.is_empty() || v.starts_with('>') || v.starts_with('|') {
            let parts: Vec<&str> = lines
                .by_ref()
                .take_while(|l| l.starts_with(' ') || l.starts_with('\t'))
                .map(str::trim)
                .collect();
            return parts.join(" ");
        }
        return v.trim_matches(|c| c == '"' || c == '\'').to_string();
    }
    String::new()
}

/// `/<skill> args` -> the prompt sent to the model: the invocation line (so the session
/// title and recap read naturally), then the skill body with its arguments. None if
/// `input` is not a known skill. Needed because lean mode disables Claude Code's own
/// slash commands and skills.
pub fn expand_skill(input: &str) -> Option<String> {
    let (cmd, args) = input
        .split_once(' ')
        .map(|(a, b)| (a, b.trim()))
        .unwrap_or((input, ""));
    let s = find_skill(cmd)?;
    let text = std::fs::read_to_string(&s.file).ok()?;
    let (_, body) = split_front(&text);
    let mut p = format!("{}\n\n", input.trim());
    match &s.dir {
        Some(d) => p.push_str(&format!(
            "Use the \"{}\" skill below (base directory: {}).\n\n",
            s.name,
            d.display()
        )),
        None => p.push_str(&format!("Run the \"{}\" command below.\n\n", s.name)),
    }
    if body.contains("$ARGUMENTS") {
        p.push_str(&body.replace("$ARGUMENTS", args));
    } else {
        p.push_str(body.trim_end());
        if !args.is_empty() {
            p.push_str(&format!("\n\nARGUMENTS: {}", args));
        }
    }
    Some(p)
}

/// Completions for the current input: (full replacement, description).
pub fn suggest(buf: &str) -> Vec<(String, String)> {
    if !buf.starts_with('/') {
        return Vec::new();
    }
    let Some((cmd, arg)) = buf.split_once(' ') else {
        let q = &buf[1..];
        if q.contains('/') {
            return Vec::new();
        }
        let all = COMMANDS
            .iter()
            .map(|(c, d)| (c.to_string(), d.to_string(), true))
            .chain(skills().iter().map(|s| (format!("/{}", s.name), s.desc.clone(), false)));
        let mut hits: Vec<(u8, bool, String, String)> = all
            .filter_map(|(c, d, builtin)| rank(q, &c[1..], &d).map(|r| (r, !builtin, c, d)))
            .collect();
        // Best match first; builtins before skills on a tie; otherwise keep their order.
        hits.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        return hits.into_iter().map(|(_, _, c, d)| (c, d)).collect();
    };
    let opts: &[&str] = match cmd {
        "/model" => MODELS,
        "/effort" => EFFORTS,
        "/mode" => &["native", "min", "adaptive"],
        "/critic" | "/alt" | "/web" | "/subagents" => &["on", "off"],
        "/perm" => &["ask", "auto"],
        "/goal" => &["clear"],
        "/check" => &["off"],
        "/candidates" => &["1", "2", "3", "5"],
        _ => &[],
    };
    opts.iter()
        .filter(|o| o.starts_with(arg) && **o != arg)
        .map(|o| (format!("{} {}", cmd, o), String::new()))
        .collect()
}

/// How well `q` matches a command name (lower is better): prefix, word prefix
/// (`/design` → `/frontend-design`, `/use` → `/figma:figma-use`), substring, in-order
/// letters (`/imprv` → `/improve`), then the description. None = no match.
fn rank(q: &str, name: &str, desc: &str) -> Option<u8> {
    if q.is_empty() {
        return Some(0);
    }
    let q = q.to_lowercase();
    let n = name.to_lowercase();
    if n.starts_with(&q) {
        return Some(0);
    }
    if n.split([':', '-', '_']).any(|w| w.starts_with(&q)) {
        return Some(1);
    }
    if n.contains(&q) {
        return Some(2);
    }
    let mut it = n.chars();
    if q.chars().all(|c| it.any(|x| x == c)) {
        return Some(3);
    }
    if q.len() >= 3 && desc.to_lowercase().contains(&q) {
        return Some(4);
    }
    None
}

/// Commands whose completion should wait for an argument instead of running.
fn takes_arg(cmd: &str) -> bool {
    matches!(
        cmd,
        "/model"
            | "/candidates"
            | "/effort"
            | "/mode"
            | "/check"
            | "/budget"
            | "/critic"
            | "/alt"
            | "/web"
            | "/subagents"
            | "/fresh"
            | "/perm"
            | "/goal"
    )
}

pub fn is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

/// Raw mode guard: no canonical input, no echo, no signals (Ctrl-C arrives as a byte).
pub struct Raw(libc::termios);

impl Raw {
    pub fn on() -> Option<Raw> {
        // SAFETY: termios is plain data; tcgetattr fills it for fd 0, checked for errors.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(0, &mut t) != 0 {
                return None;
            }
            let orig = t;
            t.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG | libc::IEXTEN);
            t.c_iflag &= !(libc::IXON | libc::ICRNL);
            t.c_cc[libc::VMIN] = 1;
            t.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &t);
            Some(Raw(orig))
        }
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        // SAFETY: restoring the attributes captured in `on`.
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &self.0);
        }
    }
}

/// Wait up to `ms` for a byte on stdin.
fn read_byte(ms: i32) -> Option<u8> {
    let mut p = libc::pollfd {
        fd: 0,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: poll/read on fd 0 into a 1-byte stack buffer.
    unsafe {
        if libc::poll(&mut p, 1, ms) <= 0 {
            return None;
        }
        let mut b = 0u8;
        if libc::read(0, (&mut b as *mut u8).cast(), 1) == 1 {
            Some(b)
        } else {
            None
        }
    }
}

fn read_utf8(first: u8) -> Option<char> {
    let n = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    let mut buf = vec![first];
    for _ in 1..n {
        buf.push(read_byte(50)?);
    }
    std::str::from_utf8(&buf).ok()?.chars().next()
}

pub fn width() -> usize {
    // SAFETY: TIOCGWINSZ fills a winsize struct for stdout.
    unsafe {
        let mut w: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut w) == 0 && w.ws_col > 0 {
            w.ws_col as usize
        } else {
            80
        }
    }
}

pub fn height() -> usize {
    // SAFETY: TIOCGWINSZ fills a winsize struct for stdout.
    unsafe {
        let mut w: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut w) == 0 && w.ws_row > 0 {
            w.ws_row as usize
        } else {
            24
        }
    }
}

enum Key {
    Char(char),
    Enter,
    Back,
    Del,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Tab,
    Ctrl(u8),
    Esc,
    Paste(String),
}

fn key() -> Option<Key> {
    let b = read_byte(-1)?;
    Some(match b {
        b'\r' | b'\n' => Key::Enter,
        0x7f | 0x08 => Key::Back,
        b'\t' => Key::Tab,
        0x1b => match read_byte(30) {
            None => Key::Esc,
            Some(b'[') | Some(b'O') => {
                let mut seq = String::new();
                while let Some(c) = read_byte(30) {
                    seq.push(c as char);
                    if c.is_ascii_alphabetic() || c == b'~' {
                        break;
                    }
                }
                match seq.as_str() {
                    "A" => Key::Up,
                    "B" => Key::Down,
                    "C" => Key::Right,
                    "D" => Key::Left,
                    "H" | "1~" => Key::Home,
                    "F" | "4~" => Key::End,
                    "3~" => Key::Del,
                    "200~" => Key::Paste(read_paste()),
                    _ => return key(),
                }
            }
            Some(_) => return key(),
        },
        0x01..=0x1a => Key::Ctrl(b + b'a' - 1),
        _ => Key::Char(read_utf8(b)?),
    })
}

fn read_paste() -> String {
    let mut raw = Vec::new();
    while let Some(b) = read_byte(200) {
        raw.push(b);
        if raw.ends_with(b"\x1b[201~") {
            raw.truncate(raw.len() - 6);
            break;
        }
    }
    String::from_utf8_lossy(&raw)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

fn history_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".qed_history"))
}

pub fn history() -> Vec<String> {
    let s = history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    s.lines().map(|l| l.replace("\\n", "\n")).collect()
}

fn save_history(line: &str) {
    if let Some(p) = history_path() {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
        {
            let _ = writeln!(f, "{}", line.replace('\n', "\\n"));
        }
    }
}

struct View<'a> {
    prompt: &'a str,
    plen: usize,
    menu: &'a [(String, String)],
    sel: usize,
    placeholder: bool,
}

const PLACEHOLDER: &str = "task, /command or !shell";

const MENU_MAX: usize = 8;

fn redraw(v: &View, buf: &[char], cur: usize) {
    // shortcut: assumes 1 column per char (CJK/emoji misalign); fine for a prompt line.
    let tw = width();
    let w = tw.saturating_sub(v.plen + 1).max(10);
    let shown: Vec<char> = buf
        .iter()
        .map(|&c| if c == '\n' { '⏎' } else { c })
        .collect();
    let start = cur.saturating_sub(w.saturating_sub(1));
    let end = (start + w).min(shown.len());
    let vis: String = shown[start..end].iter().collect();
    // Ghost text: the rest of the selected completion, dimmed after the cursor.
    let typed: String = buf.iter().collect();
    let ghost = match v.menu.get(v.sel) {
        Some((c, _)) if cur == buf.len() && c.starts_with(&typed) => c[typed.len()..].to_string(),
        _ if buf.is_empty() && v.placeholder => PLACEHOLDER.to_string(),
        _ => String::new(),
    };
    let mut o = std::io::stdout().lock();
    let _ = write!(
        o,
        "\r\x1b[2K{}{}\x1b[2m{}\x1b[0m\x1b[J",
        v.prompt, vis, ghost
    );
    let n = v.menu.len().min(MENU_MAX);
    let first = v.sel.saturating_sub(MENU_MAX - 1);
    let nw = v
        .menu
        .iter()
        .skip(first)
        .take(n)
        .map(|(c, _)| crate::ui::dw(c.rsplit(' ').next().unwrap_or(c)))
        .max()
        .unwrap_or(0)
        .clamp(12, 28);
    let mut rows = 0;
    for (i, (c, d)) in v.menu.iter().enumerate().skip(first).take(n) {
        let name = crate::ui::fit(c.rsplit(' ').next().unwrap_or(c), nw);
        let pad = " ".repeat(nw - crate::ui::dw(&name));
        let d = crate::ui::fit(d, tw.saturating_sub(nw + 6));
        if i == v.sel {
            let _ = write!(
                o,
                "\r\n\x1b[2K\x1b[1m❯ {}{}\x1b[0m  \x1b[2m{}\x1b[0m",
                name, pad, d
            );
        } else {
            let _ = write!(o, "\r\n\x1b[2K  {}{}  \x1b[2m{}\x1b[0m", name, pad, d);
        }
        rows += 1;
    }
    if v.menu.len() > n {
        let below = v.menu.len() - first - n;
        let msg = match (first, below) {
            (0, b) => format!("↓ {} more · type to filter", b),
            (a, 0) => format!("↑ {} more", a),
            (a, b) => format!("↑ {} · ↓ {} more", a, b),
        };
        let _ = write!(o, "\r\n\x1b[2K  \x1b[2m{}\x1b[0m", msg);
        rows += 1;
    }
    if rows == 0 && v.placeholder {
        let f = FOOTER.lock().map(|f| f.clone()).unwrap_or_default();
        if !f.is_empty() {
            let _ = write!(o, "\r\n\x1b[2K  \x1b[2m{}\x1b[0m", crate::ui::fit(&f, tw.saturating_sub(3)));
            rows = 1;
        }
    }
    if rows > 0 {
        let _ = write!(o, "\x1b[{}A", rows);
    }
    let col = v.plen + cur - start;
    let _ = write!(o, "\r");
    if col > 0 {
        let _ = write!(o, "\x1b[{}C", col);
    }
    let _ = o.flush();
}

/// Read one line. None = EOF (Ctrl-D on empty, Ctrl-C on empty).
pub fn read_line(
    prompt: &str,
    plen: usize,
    prefill: &str,
    hist: &mut Vec<String>,
) -> Option<String> {
    let _raw = Raw::on()?;
    print!("\x1b[?2004h");
    let mut buf: Vec<char> = prefill.chars().collect();
    let mut cur = buf.len();
    let mut hi = hist.len();
    let mut draft: Vec<char> = Vec::new();
    let mut sel = 0usize;
    let mut hidden = false;
    let menu_for = |buf: &[char], hidden: bool| {
        if hidden {
            Vec::new()
        } else {
            suggest(&buf.iter().collect::<String>())
        }
    };
    let mut menu = menu_for(&buf, hidden);
    redraw(
        &View {
            prompt,
            plen,
            menu: &menu,
            sel,
            placeholder: true,
        },
        &buf,
        cur,
    );
    let res = loop {
        let Some(k) = key() else { break None };
        let before: String = buf.iter().collect();
        match k {
            // Enter on a partial command accepts the selection; runs it unless it needs an argument.
            Key::Enter if !menu.is_empty() => {
                let pick = menu[sel.min(menu.len() - 1)].0.clone();
                // Typed in full: run it as is (its no-argument form shows the setting).
                let exact = sel == 0 && pick == buf.iter().collect::<String>();
                let needs_arg = !exact && !pick.contains(' ') && takes_arg(&pick);
                buf = if needs_arg {
                    format!("{} ", pick)
                } else {
                    pick
                }
                .chars()
                .collect();
                cur = buf.len();
                if !needs_arg {
                    break Some(buf.iter().collect::<String>());
                }
            }
            Key::Enter if cur > 0 && buf[cur - 1] == '\\' => {
                buf[cur - 1] = '\n';
            }
            Key::Enter => break Some(buf.iter().collect::<String>()),
            Key::Tab if !menu.is_empty() => {
                let pick = menu[sel.min(menu.len() - 1)].0.clone();
                // Skills take optional arguments: Tab leaves room for them, Enter runs.
                let space =
                    !pick.contains(' ') && (takes_arg(&pick) || find_skill(&pick).is_some());
                buf = if space { format!("{} ", pick) } else { pick }
                    .chars()
                    .collect();
                cur = buf.len();
            }
            Key::Up if !menu.is_empty() => sel = if sel == 0 { menu.len() - 1 } else { sel - 1 },
            Key::Down if !menu.is_empty() => sel = (sel + 1) % menu.len(),
            Key::Esc if !menu.is_empty() => hidden = true,
            Key::Char(c) => {
                buf.insert(cur, c);
                cur += 1;
            }
            Key::Paste(s) => {
                for c in s.chars() {
                    buf.insert(cur, c);
                    cur += 1;
                }
            }
            Key::Back if cur > 0 => {
                cur -= 1;
                buf.remove(cur);
            }
            Key::Del | Key::Ctrl(b'd') if cur < buf.len() => {
                buf.remove(cur);
            }
            Key::Ctrl(b'd') if buf.is_empty() => break None,
            Key::Ctrl(b'c') if buf.is_empty() => break None,
            Key::Ctrl(b'c') => {
                buf.clear();
                cur = 0;
            }
            Key::Ctrl(b'u') => {
                buf.drain(..cur);
                cur = 0;
            }
            Key::Ctrl(b'k') => buf.truncate(cur),
            Key::Ctrl(b'w') => {
                let mut s = cur;
                while s > 0 && buf[s - 1] == ' ' {
                    s -= 1;
                }
                while s > 0 && buf[s - 1] != ' ' {
                    s -= 1;
                }
                buf.drain(s..cur);
                cur = s;
            }
            Key::Left if cur > 0 => cur -= 1,
            Key::Right if cur < buf.len() => cur += 1,
            Key::Home | Key::Ctrl(b'a') => cur = 0,
            Key::End | Key::Ctrl(b'e') => cur = buf.len(),
            Key::Ctrl(b'l') => print!("\x1b[2J\x1b[H"),
            Key::Up if hi > 0 => {
                if hi == hist.len() {
                    draft = buf.clone();
                }
                hi -= 1;
                buf = hist[hi].chars().collect();
                cur = buf.len();
            }
            Key::Down if hi < hist.len() => {
                hi += 1;
                buf = if hi == hist.len() {
                    draft.clone()
                } else {
                    hist[hi].chars().collect()
                };
                cur = buf.len();
            }
            Key::Esc => {
                buf.clear();
                cur = 0;
            }
            _ => {}
        }
        let after: String = buf.iter().collect();
        if after != before {
            hidden = false;
            sel = 0;
        }
        menu = menu_for(&buf, hidden);
        redraw(
            &View {
                prompt,
                plen,
                menu: &menu,
                sel,
                placeholder: true,
            },
            &buf,
            cur,
        );
    };
    // Final line without menu or ghost text.
    redraw(
        &View {
            prompt,
            plen,
            menu: &[],
            sel: 0,
            placeholder: false,
        },
        &buf,
        buf.len(),
    );
    print!("\x1b[?2004l\r\n");
    let _ = std::io::stdout().flush();
    if let Some(l) = &res {
        if !l.trim().is_empty() && hist.last() != Some(l) {
            hist.push(l.clone());
            save_history(l);
        }
    }
    res
}

/// Arrow-key chooser drawn under the cursor and erased afterwards. Each item is
/// (main text, dim detail). ↑↓/jk move, 1-9 jump, Enter picks, Esc/q/Ctrl-C cancel.
pub fn pick(title: &str, items: &[(String, String)]) -> Option<usize> {
    if items.is_empty() {
        return None;
    }
    let _raw = Raw::on()?;
    let n = items.len().min(9);
    let draw = |sel: usize| {
        let tw = width();
        let mut o = std::io::stdout().lock();
        let _ = write!(
            o,
            "\r\x1b[J\x1b[2m{}\x1b[0m",
            crate::ui::fit(title, tw.saturating_sub(1))
        );
        for (i, (main, detail)) in items.iter().take(n).enumerate() {
            let room = tw.saturating_sub(detail.chars().count() + 8);
            let main = crate::ui::fit(main, room);
            let (mark, style) = if i == sel {
                ("❯", "\x1b[1m")
            } else {
                (" ", "")
            };
            let _ = write!(
                o,
                "\r\n{}{} {}\x1b[0m  \x1b[2m{}\x1b[0m",
                style, mark, main, detail
            );
        }
        let _ = write!(o, "\x1b[{}A\r", n);
        let _ = o.flush();
    };
    let mut sel = 0;
    draw(sel);
    let res = loop {
        match key() {
            None | Some(Key::Esc) | Some(Key::Ctrl(b'c')) | Some(Key::Char('q')) => break None,
            Some(Key::Enter) => break Some(sel),
            Some(Key::Up) | Some(Key::Char('k')) => sel = (sel + n - 1) % n,
            Some(Key::Down) | Some(Key::Char('j')) | Some(Key::Tab) => sel = (sel + 1) % n,
            Some(Key::Char(c)) if ('1'..='9').contains(&c) && (c as usize - '1' as usize) < n => {
                break Some(c as usize - '1' as usize)
            }
            _ => {}
        }
        draw(sel);
    };
    let mut o = std::io::stdout().lock();
    let _ = write!(o, "\r\x1b[J");
    let _ = o.flush();
    res
}

/// Watches the keyboard during a turn: Esc / Ctrl-C cancel; typed text becomes
/// typeahead, and Enter queues it as the next prompt.
pub struct KeyWatch {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl KeyWatch {
    pub fn start() -> KeyWatch {
        let stop = Arc::new(AtomicBool::new(false));
        let s = stop.clone();
        let handle = std::thread::spawn(move || {
            let Some(_raw) = Raw::on() else { return };
            while !s.load(Ordering::SeqCst) {
                let Some(b) = read_byte(50) else { continue };
                if ASKING.load(Ordering::SeqCst) {
                    let k = match b {
                        0x03 => {
                            CANCEL.store(true, Ordering::SeqCst);
                            b'n'
                        }
                        0x1b => {
                            while read_byte(5).is_some() {}
                            b'n'
                        }
                        b => b,
                    };
                    if let Ok(mut a) = ANSWER.lock() {
                        *a = Some(k);
                    }
                    continue;
                }
                match b {
                    0x03 => CANCEL.store(true, Ordering::SeqCst),
                    0x1b => match read_byte(30) {
                        None => CANCEL.store(true, Ordering::SeqCst),
                        Some(_) => while read_byte(5).is_some() {},
                    },
                    b'\r' | b'\n' => {
                        if let Ok(mut t) = TYPEAHEAD.lock() {
                            let line = std::mem::take(&mut *t);
                            if !line.trim().is_empty() {
                                if let Ok(mut q) = QUEUE.lock() {
                                    q.push(line);
                                }
                            }
                        }
                    }
                    0x7f | 0x08 => {
                        if let Ok(mut t) = TYPEAHEAD.lock() {
                            t.pop();
                        }
                    }
                    0x00..=0x1f => {}
                    _ => {
                        if let (Some(c), Ok(mut t)) = (read_utf8(b), TYPEAHEAD.lock()) {
                            t.push(c);
                        }
                    }
                }
            }
        });
        KeyWatch {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for KeyWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}
