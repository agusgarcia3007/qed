//! One model call = one `claude -p` process in stream-json mode.
//! Claude Code's events are normalized into one qed event stream that feeds
//! both the human renderer and headless JSON, and is always written to the trace.

use crate::store::{clip, now_ms, Session};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub static CANCEL: AtomicBool = AtomicBool::new(false);
/// Model calls in flight. A count, not a flag: deep mode runs candidates in parallel, and
/// the first one to finish must not make Ctrl-C look like "between turns" for the rest.
pub static IN_TURN: AtomicUsize = AtomicUsize::new(0);

/// Counts one model call in flight until dropped.
pub struct TurnGuard(());

impl TurnGuard {
    pub fn enter() -> Self {
        IN_TURN.fetch_add(1, Ordering::SeqCst);
        TurnGuard(())
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        IN_TURN.fetch_sub(1, Ordering::SeqCst);
    }
}

pub fn in_turn() -> bool {
    IN_TURN.load(Ordering::SeqCst) > 0
}
/// Set by the REPL: author turns go to a persistent process (stream-json input).
pub static PERSIST: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq)]
pub enum Out {
    Human,
    Json,
    Quiet,
}

#[derive(Clone)]
pub struct Opts {
    pub model: String,
    pub effort: String,
    pub tools: String,
    pub perm: String,
    /// Skip user-level MCP servers, skills and settings (measured: ~268k -> ~9.5k prompt tokens).
    pub lean: bool,
    pub append: Option<String>,
    pub resume: Option<String>,
    pub schema: Option<String>,
    pub cwd: PathBuf,
    pub deadline: Option<Instant>,
    pub max_usd: Option<f64>,
    pub session_dir: Option<PathBuf>,
    /// Main author in the REPL: keep one Claude Code process alive across turns.
    pub live: bool,
    /// Ask the person before Bash / WebFetch (live process only; edits stay automatic).
    pub ask: bool,
}

#[derive(Default, Debug)]
pub struct Turn {
    pub session_id: Option<String>,
    pub text: String,
    pub structured: Option<Value>,
    pub cost: f64,
    pub is_error: bool,
    pub cancelled: bool,
    pub timed_out: bool,
    pub num_turns: u64,
    pub spawn_ms: u64,
    pub first_event_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub total_ms: u64,
    pub tools_used: Vec<String>,
    /// Total prompt tokens over all requests of the call (fresh + cached).
    pub ctx_tokens: u64,
    pub out_tokens: u64,
    /// Prompt size of the last request: how full the context window is now.
    pub ctx_now: u64,
    /// The model's context window, as reported by Claude Code (0 if unknown).
    pub ctx_window: u64,
}

const DENY: &str = "Read(./.env) Read(./.env.*) Read(~/.ssh/**) Read(~/.aws/**) Bash(cat .env*) Bash(printenv*) Bash(env)";

/// Human duration from microseconds: 840µs, 11.5ms, 3.2s.
pub fn fmt_us(us: u64) -> String {
    match us {
        0..=999 => format!("{}µs", us),
        1_000..=999_999 => format!("{:.1}ms", us as f64 / 1e3),
        _ => format!("{:.1}s", us as f64 / 1e6),
    }
}

pub fn claude_bin() -> String {
    std::env::var("QED_CLAUDE").unwrap_or_else(|_| "claude".into())
}

fn build_cmd(o: &Opts, stream_in: bool) -> Command {
    let mut c = Command::new(claude_bin());
    c.args([
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
    ]);
    c.args([
        "--model",
        &o.model,
        "--effort",
        &o.effort,
    ]);
    if !(o.ask && stream_in) {
        c.args(["--permission-mode", &o.perm]);
    }
    // In ask mode Bash and WebFetch go through the person (checks stay automatic).
    let allowed = if o.ask && stream_in {
        let mut a: Vec<&str> = o
            .tools
            .split(',')
            .filter(|t| !matches!(*t, "Bash" | "WebFetch"))
            .collect();
        a.push("Bash(qed check:*)");
        a.join(",")
    } else {
        o.tools.clone()
    };
    c.args([
        "--tools",
        &o.tools,
        "--allowedTools",
        &allowed,
        "--disallowedTools",
        DENY,
    ]);
    if o.ask && stream_in {
        // acceptEdits would wave through mkdir/touch/rm in the project; edits are already
        // allowed by --allowedTools, so `default` asks for exactly shell and web fetches.
        c.args(["--permission-prompts", "host", "--permission-prompt-tool", "stdio"]);
        c.args(["--permission-mode", "default"]);
    }
    if o.lean {
        c.args([
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--setting-sources",
            "project,local",
        ]);
    }
    if let Some(a) = &o.append {
        c.args(["--append-system-prompt", a]);
    }
    if let Some(r) = &o.resume {
        c.args(["--resume", r]);
    }
    if let Some(s) = &o.schema {
        c.args(["--json-schema", s]);
    }
    if stream_in {
        c.args(["--input-format", "stream-json"]);
    } else if let Some(m) = o.max_usd {
        // Per-process cap: only meaningful for one-shot calls.
        c.args(["--max-budget-usd", &format!("{:.2}", m.max(0.01))]);
    }
    // `qed check` must be callable by the model and must record into this session.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            let path = std::env::var("PATH").unwrap_or_default();
            c.env("PATH", format!("{}:{}", d.display(), path));
        }
    }
    if let Some(sd) = &o.session_dir {
        c.env("QED_SESSION", sd);
    }
    c.current_dir(&o.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

fn stop(child: &mut Child) {
    // SIGINT first so Claude Code flushes its session file (keeps --resume working).
    // SAFETY: kill(2) on a pid we own; no memory is touched.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let until = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(30));
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// A tool call's row on screen, so its result can be marked in place.
struct ToolRow {
    row: usize,
    text: String,
    at: Instant,
    width: usize,
}

pub struct Emitter<'a> {
    pub out: Out,
    pub sess: Option<&'a mut Session>,
    phase: String,
    since: Instant,
    last_out: Instant,
    spin: Option<usize>,
    /// The spinner sits on the row below a half-written line (cursor goes back after).
    spin_below: bool,
    /// The model's voice: streaming markdown, wrapped by qed.
    md: ui::Md,
    /// Rows written so far (everything qed prints during a turn is at most one row wide).
    rows: usize,
    tools: std::collections::HashMap<String, ToolRow>,
    /// The last thing printed was a tool row (prose after it gets a blank line).
    after_tool: bool,
    /// The last row printed was a check verdict (the closing ∎ joins it without a gap).
    after_check: bool,
    /// Output tokens streamed in the current model call (spinner).
    pub streamed: u64,
    /// Context in use (prompt size of the latest request) and the window, for the spinner.
    pub ctx: u64,
    pub ctx_window: u64,
}

const SPIN: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
use crate::ui::{self, ACCENT, BAD, DIM, OK, RST};

/// Width used for the model's prose: the terminal minus one column (no pending-wrap
/// surprises); unbounded when not writing to a terminal.
fn text_width() -> usize {
    if ui::stdout_tty() {
        crate::term::width().saturating_sub(1).max(20)
    } else {
        usize::MAX / 4
    }
}

impl<'a> Emitter<'a> {
    pub fn new(out: Out, sess: Option<&'a mut Session>) -> Self {
        Emitter {
            out,
            sess,
            phase: String::new(),
            since: Instant::now(),
            last_out: Instant::now(),
            spin: None,
            spin_below: false,
            md: ui::Md::default(),
            rows: 0,
            tools: Default::default(),
            after_tool: false,
            after_check: false,
            streamed: 0,
            ctx: 0,
            ctx_window: 0,
        }
        .with_ctx_from_session()
    }

    /// Start the context meter where the session left it.
    fn with_ctx_from_session(mut self) -> Self {
        if let Some(s) = self.sess.as_deref() {
            self.ctx = s.meta["ctx"].as_u64().unwrap_or(0);
            self.ctx_window = s.meta["ctx_window"].as_u64().unwrap_or(0);
        }
        self
    }

    /// Set the spinner's phase text directly (for work that isn't a model call).
    pub fn status(&mut self, s: &str) {
        self.phase = s.to_string();
    }

    /// Write and count rows.
    fn put(&mut self, s: &str) {
        self.rows += s.matches('\n').count();
        let mut o = std::io::stdout().lock();
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }

    /// Spinner with the current phase while nothing else is printing (human mode, tty only).
    pub fn tick(&mut self) {
        let mid = !self.md.at_bol();
        let idle = if mid { 700 } else { 150 };
        if self.out != Out::Human
            || !ui::stdout_tty()
            || self.last_out.elapsed() < Duration::from_millis(idle)
        {
            return;
        }
        if self.spin.is_none() && mid {
            // A pause mid-sentence: show the pending word, then spin on the row below.
            let w = text_width();
            let f = self.md.flush(w);
            self.put(&f);
            self.put(&format!("{}\n", RST));
            self.spin_below = true;
        }
        let f = self.spin.map_or(0, |f| (f + 1) % SPIN.len());
        self.spin = Some(f);
        let ta = crate::term::TYPEAHEAD
            .lock()
            .map(|t| t.clone())
            .unwrap_or_default();
        let q = crate::term::QUEUE.lock().map(|q| q.len()).unwrap_or(0);
        let mut extra = String::new();
        if q > 0 {
            extra.push_str(&format!(" · {} queued", q));
        }
        if !ta.is_empty() {
            extra.push_str(&format!(" · › {}", ta));
        }
        let mut toks = if self.streamed > 0 {
            format!(" · ↓ {} tokens", ui::tokens(self.streamed))
        } else {
            String::new()
        };
        if self.ctx > 0 {
            toks.push_str(&format!(" · {}", ui::ctx(self.ctx, self.ctx_window)));
        }
        let line = format!(
            "{} {} · {}{} · esc to cancel{}",
            SPIN[f],
            self.phase,
            ui::secs(self.since.elapsed().as_secs_f64()),
            toks,
            extra
        );
        let line = ui::fit(&line, crate::term::width().saturating_sub(3));
        let mut o = std::io::stdout().lock();
        let _ = write!(o, "\r\x1b[2K  {}{}{}", DIM, line, RST);
        let _ = o.flush();
    }

    fn unspin(&mut self) {
        if self.spin.take().is_some() {
            let mut s = String::from("\r\x1b[2K");
            if std::mem::take(&mut self.spin_below) {
                // Back to the end of the half-written line, in its style.
                s.push_str(&format!("\x1b[1A\x1b[{}G{}", self.md.col + 1, self.md.sgr()));
                self.rows -= 1;
            }
            let mut o = std::io::stdout().lock();
            let _ = o.write_all(s.as_bytes());
            let _ = o.flush();
        }
        self.last_out = Instant::now();
    }

    fn set_phase(&mut self, ev: &Value) {
        let p = match ev["ev"].as_str().unwrap_or("") {
            "start" => {
                self.since = Instant::now();
                self.streamed = 0;
                "starting".into()
            }
            "init" | "tool_result" => "thinking".into(),
            "tool" => {
                let i = &ev["input"];
                match ev["name"].as_str().unwrap_or("") {
                    "Bash" => "running".into(),
                    "Read" | "Grep" | "Glob" => "reading".into(),
                    "WebSearch" | "WebFetch" => "browsing".into(),
                    "Edit" | "Write" => "editing".into(),
                    "Task" | "Agent" => "subagent working".into(),
                    _ => ui::tool_line(ev["name"].as_str().unwrap_or(""), i),
                }
            }
            "critic" => "critic reviewing".into(),
            "alt" => "alt branch".into(),
            _ => return,
        };
        self.phase = p;
    }

    pub fn emit(&mut self, mut ev: Value) {
        ev["t"] = json!(now_ms() as u64);
        if let Some(s) = self.sess.as_mut() {
            // Trace keeps bounded payloads; big text already lives in Claude's own transcript.
            let mut t = ev.clone();
            if let Some(x) = t["text"].as_str() {
                t["text"] = json!(clip(x, 4000));
            }
            s.event(&t);
        }
        match self.out {
            Out::Json => {
                println!("{}", ev);
            }
            Out::Human => self.render(&ev),
            Out::Quiet => {}
        }
    }

    /// Finish the model's current line so chrome starts on a fresh row.
    fn end_line(&mut self) {
        let e = self.md.end(text_width());
        self.put(&e);
    }

    fn line(&mut self, s: &str) {
        self.end_line();
        self.put(&format!("{}\n", s));
        self.after_tool = false;
    }

    /// Dim, indented note from the harness.
    fn note(&mut self, s: &str) {
        let w = crate::term::width().saturating_sub(4);
        self.line(&format!("  {}{}{}", DIM, ui::fit(s, w), RST));
    }

    /// Stream model text.
    fn text(&mut self, t: &str) {
        let r = self.md.push(t, text_width());
        self.put(&r);
    }

    /// A permission card for one tool call; the answer is Claude Code's decision object.
    /// y = allow once, a = allow for the rest of the session, n / Esc = deny.
    pub fn permission(&mut self, req: &Value) -> Value {
        let deny = |m: &str| json!({"behavior": "deny", "message": m});
        if self.out != Out::Human || !crate::term::is_tty() {
            return deny("No person is available to approve this action.");
        }
        let tool = req["tool_name"].as_str().unwrap_or("?");
        let input = req["input"].clone();
        self.unspin();
        self.end_line();
        let w = crate::term::width().saturating_sub(6);
        let mut card = format!(
            "  {}?{} {}allow{} {}\n",
            ACCENT,
            RST,
            ui::BOLD,
            RST,
            ui::fit(&ui::tool_line(tool, &input), w.saturating_sub(8))
        );
        let desc = req["description"].as_str().unwrap_or("");
        if !desc.is_empty() && !ui::tool_line(tool, &input).contains(desc) {
            card.push_str(&format!("    {}{}{}\n", DIM, ui::fit(desc, w), RST));
        }
        card.push_str(&format!(
            "    {b}[y]{r}es  {b}[a]{r}lways (this session)  {b}[n]{r}o\n",
            b = ui::BOLD,
            r = RST
        ));
        let n = card.matches('\n').count();
        self.put(&card);
        let key = crate::term::ask_key();
        // Erase the card; the tool's own row shows the outcome.
        self.put(&format!("\x1b[{}A\r\x1b[J", n));
        self.rows -= n;
        self.last_out = Instant::now();
        match key {
            b'y' | b'Y' | b'\r' => json!({"behavior": "allow", "updatedInput": input}),
            b'a' | b'A' => {
                let mut rules = req["permission_suggestions"].clone();
                for r in rules.as_array_mut().into_iter().flatten() {
                    r["destination"] = json!("session");
                }
                json!({"behavior": "allow", "updatedInput": input, "updatedPermissions": rules})
            }
            _ => deny("The person denied this action. Do not retry it as is; ask or take another route."),
        }
    }

    /// Rewrite a tool's row in place with its outcome (only while it is on screen and
    /// the width it was laid out for still holds).
    fn mark_tool(&mut self, id: &str, failed: bool) {
        let Some(t) = self.tools.remove(id) else { return };
        let secs = t.at.elapsed().as_secs_f64();
        let up = self.rows - t.row;
        if !ui::stdout_tty() || crate::term::width() != t.width || up == 0 || up >= crate::term::height() {
            return;
        }
        if !failed && secs < 1.0 {
            return;
        }
        let mut row = format!("  {}{}", DIM, t.text);
        if secs >= 1.0 {
            row.push_str(&format!(" · {}", ui::secs(secs)));
        }
        row.push_str(RST);
        if failed {
            row.push_str(&format!(" {}✗{}", BAD, RST));
        }
        let back = if self.md.col > 0 {
            format!("\x1b[{}G{}", self.md.col + 1, self.md.sgr())
        } else {
            "\r".into()
        };
        let mut o = std::io::stdout().lock();
        let _ = write!(o, "\x1b[{}A\r\x1b[2K{}\x1b[{}B{}", up, row, up, back);
        let _ = o.flush();
    }

    fn render(&mut self, ev: &Value) {
        let k = ev["ev"].as_str().unwrap_or("");
        self.set_phase(ev);
        if matches!(k, "start" | "init" | "result" | "quota") {
            return;
        }
        self.unspin();
        let was_check = std::mem::take(&mut self.after_check);
        self.after_check = was_check && matches!(k, "check" | "done");
        match k {
            "text" => {
                let t = ev["text"].as_str().unwrap_or("");
                if std::mem::take(&mut self.after_tool) && self.md.at_bol() && !t.trim().is_empty() {
                    // Breathing room between a run of tool lines and prose.
                    self.put("\n");
                }
                self.text(t);
            }
            "tool" => {
                let w = crate::term::width();
                let name = ev["name"].as_str().unwrap_or("?");
                let s = ui::fit(&ui::tool_line(name, &ev["input"]), w.saturating_sub(16));
                let prose = !self.md.at_bol() || (!self.after_tool && self.rows > 0);
                self.end_line();
                if prose && !self.after_tool {
                    self.put("\n");
                }
                let row = self.rows;
                self.put(&format!("  {}{}{}\n", DIM, s, RST));
                if name == "Edit" && ui::stdout_tty() {
                    let d = ui::edit_preview(&ev["input"], w.saturating_sub(6), 8);
                    self.put(&d);
                }
                if let Some(id) = ev["id"].as_str() {
                    self.tools.insert(
                        id.to_string(),
                        ToolRow { row, text: s, at: Instant::now(), width: w },
                    );
                }
                self.after_tool = true;
            }
            "tool_result" => {
                let id = ev["id"].as_str().unwrap_or("");
                self.mark_tool(id, ev["is_error"] == true);
            }
            "check" => {
                let cmd = ev["cmd"].as_str().unwrap_or("");
                // Verdicts form one block, set apart from the work above.
                self.end_line();
                if !self.after_check {
                    self.put("\n");
                }
                let w = crate::term::width().saturating_sub(16);
                let cmd = ui::fit(cmd, w);
                if ev["exit"] == 0 {
                    let again = if ev["reused"] == true {
                        " · same tree"
                    } else {
                        ""
                    };
                    self.line(&format!("{}✓{} {}{}{}{}", OK, RST, cmd, DIM, again, RST));
                } else {
                    self.line(&format!(
                        "{}✗{} {} {}· exit {}{}",
                        BAD, RST, cmd, DIM, ev["exit"], RST
                    ));
                    let t = crate::store::tail(ev["tail"].as_str().unwrap_or(""), 6);
                    for l in t.lines() {
                        self.note(&format!("  {}", l));
                    }
                }
                self.after_check = true;
            }
            "done" => {
                if ev["cancelled"] == true {
                    return;
                }
                let mut stats = format!(
                    "{} · {}",
                    ui::secs(ev["wall_ms"].as_u64().unwrap_or(0) as f64 / 1000.0),
                    ui::money(ev["cost"].as_f64().unwrap_or(0.0))
                );
                if let Some(n) = ev["calls"].as_u64().filter(|&n| n > 1) {
                    stats.push_str(&format!(" · {} calls", n));
                }
                self.end_line();
                if !self.after_check {
                    self.put("\n");
                }
                if ev["passed"] == false {
                    self.put(&format!("{}✗{} {}unverified · {}{}\n", BAD, RST, DIM, stats, RST));
                } else {
                    self.put(&format!("{}∎{} {}{}{}\n", ACCENT, RST, DIM, stats, RST));
                }
                self.after_tool = false;
            }
            "cancelled" => {
                self.end_line();
                self.put("\n");
                self.note("cancelled · session can be resumed");
            }
            "error" => {
                let m = ev["msg"].as_str().unwrap_or("");
                self.line(&format!("{}✗{} {}", BAD, RST, m));
            }
            "info" | "decision" | "alt" => {
                self.note(&format!("◇ {}", ev["msg"].as_str().unwrap_or("")))
            }
            "critic" => self.note(&format!("◇ critic: {}", ev["msg"].as_str().unwrap_or(""))),
            _ => {}
        }
    }
}

/// Fold one Claude Code stream-json line into the turn and the qed event stream.
/// Returns true on the turn's `result`.
fn handle(line: &str, turn: &mut Turn, em: &mut Emitter, t0: Instant) -> bool {
    let ms = |t: Instant| t.duration_since(t0).as_millis() as u64;
    if turn.first_event_ms.is_none() {
        turn.first_event_ms = Some(ms(Instant::now()));
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    // Subagent traffic (parent_tool_use_id set) is not rendered as the main voice.
    let sub = !v["parent_tool_use_id"].is_null();
    match v["type"].as_str().unwrap_or("") {
        "system" if v["subtype"] == "init" => {
            turn.session_id = v["session_id"].as_str().map(String::from);
            em.emit(json!({"ev": "init", "session": turn.session_id, "tools": v["tools"], "init_ms": ms(Instant::now())}));
        }
        "stream_event" if !sub => {
            let e = &v["event"];
            // First token = first content block of any kind (thinking isn't streamed as text).
            let first = e["type"] == "content_block_start" || e["type"] == "content_block_delta";
            if first && turn.ttft_ms.is_none() {
                turn.ttft_ms = Some(ms(Instant::now()));
            }
            // Each request's prompt size, as it starts: the context meter moves during the turn.
            if e["type"] == "message_start" {
                let n = ctx_of(&e["message"]["usage"]);
                if n > 0 {
                    em.ctx = n;
                }
            }
            if e["type"] == "content_block_delta" {
                let d = &e["delta"];
                if let Some(t) = d["text"].as_str() {
                    em.emit(json!({"ev": "text", "text": t}));
                }
                // Live estimate (~4 chars per token) over text, thinking and tool input.
                let n = ["text", "thinking", "partial_json"]
                    .iter()
                    .filter_map(|k| d[*k].as_str())
                    .map(str::len)
                    .sum::<usize>();
                em.streamed += n.div_ceil(4) as u64;
            }
        }
        "assistant" if sub => {
            // A subagent's work shows as the spinner's phase, not in the main voice.
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    let t = ui::tool_line(b["name"].as_str().unwrap_or("?"), &b["input"]);
                    em.status(&format!("subagent: {}", t));
                }
            }
        }
        "assistant" if !sub => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_use" {
                    let name = b["name"].as_str().unwrap_or("?").to_string();
                    turn.tools_used.push(name.clone());
                    em.emit(json!({"ev": "tool", "id": b["id"], "name": name, "input": b["input"]}));
                }
            }
        }
        "user" if !sub => {
            for b in v["message"]["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result" {
                    let txt = tool_result_text(&b["content"]);
                    em.emit(json!({
                        "ev": "tool_result", "id": b["tool_use_id"], "is_error": b["is_error"] == true,
                        "bytes": txt.len(), "preview": clip(&txt, 600)
                    }));
                }
            }
        }
        "rate_limit_event" => {
            let r = &v["rate_limit_info"];
            em.emit(json!({"ev": "quota", "status": r["status"], "utilization": r["utilization"], "window": r["rateLimitType"]}));
        }
        "result" => {
            turn.session_id = v["session_id"]
                .as_str()
                .map(String::from)
                .or(turn.session_id.take());
            turn.text = v["result"].as_str().unwrap_or("").to_string();
            turn.structured = v.get("structured_output").cloned().filter(|x| !x.is_null());
            turn.cost = v["total_cost_usd"].as_f64().unwrap_or(0.0);
            turn.is_error = v["is_error"] == true;
            turn.num_turns = v["num_turns"].as_u64().unwrap_or(0);
            let u = &v["usage"];
            turn.ctx_tokens = ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
                .iter()
                .filter_map(|k| u[*k].as_u64())
                .sum();
            turn.out_tokens = u["output_tokens"].as_u64().unwrap_or(0);
            let last = u["iterations"].as_array().and_then(|a| a.last()).unwrap_or(u);
            turn.ctx_now = ctx_of(last);
            // modelUsage.<model>.contextWindow (Claude Code 2.1.x). The largest one is the
            // main model's: subagents run on the same or smaller windows.
            turn.ctx_window = v["modelUsage"]
                .as_object()
                .into_iter()
                .flat_map(|m| m.values())
                .filter_map(|u| u["contextWindow"].as_u64())
                .max()
                .unwrap_or(0);
            if turn.ctx_now > 0 {
                em.ctx = turn.ctx_now;
            }
            if turn.ctx_window > 0 {
                em.ctx_window = turn.ctx_window;
            }
            return true;
        }
        _ => {}
    }
    false
}

fn finish(turn: &mut Turn, em: &mut Emitter, t0: Instant) {
    turn.total_ms = t0.elapsed().as_millis() as u64;
    em.emit(json!({
        "ev": "result", "session": turn.session_id, "cost": turn.cost, "is_error": turn.is_error,
        "cancelled": turn.cancelled, "timed_out": turn.timed_out, "num_turns": turn.num_turns,
        "spawn_ms": turn.spawn_ms, "first_event_ms": turn.first_event_ms, "ttft_ms": turn.ttft_ms,
        "total_ms": turn.total_ms, "structured": turn.structured,
        "in_tokens": turn.ctx_tokens, "out_tokens": turn.out_tokens
    }));
}

pub fn run(o: &Opts, prompt: &str, em: &mut Emitter) -> Turn {
    if o.live && o.schema.is_none() && PERSIST.load(Ordering::SeqCst) {
        return run_live(o, prompt, em);
    }
    let t0 = Instant::now();
    let ms = |t: Instant| t.duration_since(t0).as_millis() as u64;
    let mut turn = Turn::default();
    em.emit(json!({"ev": "start", "model": o.model, "effort": o.effort, "resume": o.resume, "cwd": o.cwd}));

    let mut child = match build_cmd(o, false).spawn() {
        Ok(c) => c,
        Err(e) => {
            em.emit(json!({"ev": "error", "msg": format!("could not launch {}: {}", claude_bin(), e)}));
            turn.is_error = true;
            return turn;
        }
    };
    turn.spawn_ms = ms(Instant::now());
    if let Some(mut si) = child.stdin.take() {
        let _ = si.write_all(prompt.as_bytes());
    }
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(so) = child.stdout.take() {
        std::thread::spawn(move || {
            for l in BufReader::new(so).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    let stderr = child.stderr.take().map(|se| {
        std::thread::spawn(move || {
            let mut s = String::new();
            let _ = std::io::Read::read_to_string(&mut BufReader::new(se), &mut s);
            s
        })
    });

    // CANCEL is reset by the caller before it starts listening for keys, so an Esc
    // pressed while the turn is starting is not lost.
    let turn_guard = TurnGuard::enter();
    let mut got_result = false;
    loop {
        if CANCEL.load(Ordering::SeqCst) {
            stop(&mut child);
            turn.cancelled = true;
            em.emit(json!({"ev": "cancelled", "session": turn.session_id}));
            break;
        }
        if o.deadline.is_some_and(|d| Instant::now() > d) {
            stop(&mut child);
            turn.timed_out = true;
            em.emit(json!({"ev": "error", "msg": "time budget exhausted", "session": turn.session_id}));
            break;
        }
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(l) => got_result |= handle(&l, &mut turn, em, t0),
            Err(mpsc::RecvTimeoutError::Timeout) => em.tick(),
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    drop(turn_guard);
    let status = child.wait().ok();
    let err = stderr.and_then(|h| h.join().ok()).unwrap_or_default();
    if !got_result && !turn.cancelled && !turn.timed_out {
        turn.is_error = true;
        let code = status.and_then(|s| s.code()).unwrap_or(-1);
        em.emit(json!({"ev": "error", "msg": format!("claude exited without a result (exit {}): {}", code, clip(err.trim(), 500))}));
    }
    finish(&mut turn, em, t0);
    turn
}

/// A Claude Code process kept alive across REPL turns (`--input-format stream-json`):
/// follow-ups skip process start and init, and Esc interrupts the turn with a control
/// request instead of killing the process.
struct Live {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: mpsc::Receiver<String>,
    err: std::sync::Arc<std::sync::Mutex<String>>,
    sig: String,
    session: Option<String>,
    /// Claude Code reports cost cumulatively over the process.
    total: f64,
    /// Spawned ahead of time and not sent anything yet: nothing to save, safe to kill.
    fresh: bool,
}

static LIVE: std::sync::Mutex<Option<Live>> = std::sync::Mutex::new(None);

/// What a live process was started with; any difference needs a new process.
fn live_sig(o: &Opts) -> String {
    format!(
        "{}|{}|{}|{}|{}|{:?}|{:?}|{:?}|{}",
        o.model, o.effort, o.tools, o.perm, o.lean, o.append, o.cwd, o.session_dir, o.ask
    )
}

fn spawn_live(o: &Opts) -> std::io::Result<Live> {
    let mut child = build_cmd(o, true).spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| std::io::Error::other("no stdin"))?;
    let (tx, rx) = mpsc::channel::<String>();
    if let Some(so) = child.stdout.take() {
        std::thread::spawn(move || {
            for l in BufReader::new(so).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
    }
    let err = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(se) = child.stderr.take() {
        let err = err.clone();
        std::thread::spawn(move || {
            for l in BufReader::new(se).lines().map_while(Result::ok) {
                if let Ok(mut e) = err.lock() {
                    e.push_str(&l);
                    e.push('\n');
                    if e.len() > 4000 {
                        let cut = e.len() - 2000;
                        let cut = (cut..e.len()).find(|&i| e.is_char_boundary(i)).unwrap_or(0);
                        e.drain(..cut);
                    }
                }
            }
        });
    }
    Ok(Live { child, stdin, rx, err, sig: live_sig(o), session: o.resume.clone(), total: 0.0, fresh: true })
}

/// Start the main author's process before the person sends the first prompt, so Claude
/// Code's startup and init overlap with typing (measured: init 0.3-0.6s -> 0.05s).
/// No request is made until a message arrives. Leaves a used conversation alone: the next
/// turn restarts it with --resume if anything changed.
pub fn prewarm(o: &Opts) {
    let Ok(mut g) = LIVE.lock() else { return };
    if let Some(l) = g.as_mut() {
        let alive = matches!(l.child.try_wait(), Ok(None));
        if alive && l.sig == live_sig(o) && l.session == o.resume {
            return;
        }
        if alive && !l.fresh {
            return;
        }
        if let Some(mut old) = g.take() {
            let _ = old.child.kill();
            let _ = old.child.wait();
        }
    }
    if let Ok(l) = spawn_live(o) {
        *g = Some(l);
    }
}

/// End the live process (REPL exit, /new): EOF on stdin lets Claude Code save and exit.
pub fn shutdown() {
    if let Some(mut l) = LIVE.lock().ok().and_then(|mut g| g.take()) {
        if l.fresh {
            let _ = l.child.kill();
            let _ = l.child.wait();
            return;
        }
        drop(l.stdin);
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until {
            if let Ok(Some(_)) = l.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        stop(&mut l.child);
    }
}

fn run_live(o: &Opts, prompt: &str, em: &mut Emitter) -> Turn {
    let t0 = Instant::now();
    let mut turn = Turn::default();
    em.emit(json!({"ev": "start", "model": o.model, "effort": o.effort, "resume": o.resume, "cwd": o.cwd, "live": true}));
    let mut slot = LIVE.lock().ok().and_then(|mut g| g.take());
    // Reuse only the process that is carrying on this exact conversation and config.
    // A prewarmed process (fresh) can also start a new conversation.
    let reusable = slot.as_mut().is_some_and(|l| {
        l.sig == live_sig(o)
            && (o.resume.is_some() || l.fresh)
            && l.session == o.resume
            && matches!(l.child.try_wait(), Ok(None))
    });
    if !reusable {
        if let Some(l) = slot.take() {
            *LIVE.lock().unwrap_or_else(|e| e.into_inner()) = Some(l);
            shutdown();
        }
        match spawn_live(o) {
            Ok(l) => slot = Some(l),
            Err(e) => {
                em.emit(json!({"ev": "error", "msg": format!("could not launch {}: {}", claude_bin(), e)}));
                turn.is_error = true;
                return turn;
            }
        }
    }
    let Some(mut l) = slot else { return turn };
    turn.spawn_ms = t0.elapsed().as_millis() as u64;
    l.fresh = false;
    let msg = json!({"type": "user", "message": {"role": "user", "content": prompt}});
    if writeln!(l.stdin, "{}", msg).and_then(|_| l.stdin.flush()).is_err() {
        stop(&mut l.child);
        em.emit(json!({"ev": "error", "msg": "claude process closed its input"}));
        turn.is_error = true;
        return turn;
    }

    let turn_guard = TurnGuard::enter();
    let mut got_result = false;
    let mut alive = true;
    // Interrupt requested at this instant (Esc or deadline): wait for the turn's result.
    let mut stopping: Option<Instant> = None;
    loop {
        let cancel = CANCEL.load(Ordering::SeqCst);
        let late = o.deadline.is_some_and(|d| Instant::now() > d);
        if (cancel || late) && stopping.is_none() {
            turn.cancelled = cancel;
            turn.timed_out = !cancel;
            stopping = Some(Instant::now());
            let req = json!({"type": "control_request", "request_id": format!("qed-{}", now_ms()), "request": {"subtype": "interrupt"}});
            if writeln!(l.stdin, "{}", req).and_then(|_| l.stdin.flush()).is_err() {
                alive = false;
                break;
            }
        }
        if stopping.is_some_and(|s| s.elapsed() > Duration::from_secs(4)) {
            // The process did not wind the turn down: end it (SIGINT keeps --resume working).
            stop(&mut l.child);
            alive = false;
            break;
        }
        match l.rx.recv_timeout(Duration::from_millis(50)) {
            Ok(line) if line.contains("\"can_use_tool\"") => {
                let v: Value = serde_json::from_str(&line).unwrap_or_default();
                if v["type"] == "control_request" {
                    let answer = em.permission(&v["request"]);
                    let resp = json!({"type": "control_response", "response": {
                        "subtype": "success", "request_id": v["request_id"], "response": answer}});
                    if writeln!(l.stdin, "{}", resp).and_then(|_| l.stdin.flush()).is_err() {
                        alive = false;
                        break;
                    }
                }
            }
            Ok(line) => {
                if handle(&line, &mut turn, em, t0) {
                    got_result = true;
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if stopping.is_none() {
                    em.tick();
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                alive = false;
                break;
            }
        }
    }
    drop(turn_guard);
    if got_result {
        let total = turn.cost;
        turn.cost = (total - l.total).max(0.0);
        l.total = total;
    }
    if turn.cancelled {
        // An interrupted turn reports an error result; that is the cancel, not a failure.
        turn.is_error = false;
        em.emit(json!({"ev": "cancelled", "session": turn.session_id}));
    } else if turn.timed_out {
        em.emit(json!({"ev": "error", "msg": "time budget exhausted", "session": turn.session_id}));
    } else if !got_result {
        turn.is_error = true;
        let _ = l.child.wait();
        let err = l.err.lock().map(|e| e.clone()).unwrap_or_default();
        em.emit(json!({"ev": "error", "msg": format!("claude exited without a result: {}", clip(err.trim(), 500))}));
    }
    if alive && got_result {
        l.session = turn.session_id.clone().or(l.session.take());
        if let Ok(mut g) = LIVE.lock() {
            *g = Some(l);
        }
    } else {
        stop(&mut l.child);
    }
    finish(&mut turn, em, t0);
    turn
}

/// Prompt tokens of one request: fresh + cache read + cache write.
fn ctx_of(u: &Value) -> u64 {
    ["input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"]
        .iter()
        .filter_map(|k| u[*k].as_u64())
        .sum()
}

fn tool_result_text(c: &Value) -> String {
    match c {
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_turns_keep_the_count() {
        let a = TurnGuard::enter();
        let b = TurnGuard::enter();
        drop(a);
        assert!(in_turn(), "one candidate finishing doesn't end the other's turn");
        drop(b);
    }

    #[test]
    fn durations_are_human() {
        assert_eq!(fmt_us(840), "840µs");
        assert_eq!(fmt_us(11_524), "11.5ms");
        assert_eq!(fmt_us(3_225_000), "3.2s");
    }
}
