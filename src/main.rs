mod deep;
mod engine;
mod eval;
mod logo;
mod run;
mod store;
mod term;
mod ui;
mod update;

use engine::{Emitter, Out, CANCEL};
use run::{Cfg, Mode};
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use store::{project_root, tree_hash, Session};
use term::{EFFORTS, MODELS};

const HELP: &str = "qed — minimal harness over Claude Code

  qed                         interactive (launches nothing until the first prompt)
  qed \"task\"                  interactive session that starts with this task
                              (one task and exit when not on a terminal)
  qed -p \"task\" | -p -        headless: one JSON event per line (prompt from stdin with -)
  qed resume [id] | qed -c    resume a session (no id: pick with ↑↓; -c: the last one)
  qed check [-t SECS] -- CMD  run a check and record it bound to the exact code state
  qed status [id]             evidence and which checks are still valid
  qed update | qed upgrade    install the latest release (the REPL also does it in the
                              background and offers /restart; QED_NO_UPDATE=1 turns it off)
  qed logo                    ∎ (QED_LOGO=1 also animates it on startup)
  qed eval TASKS [--variants A,B,C,D,D-nocritic,D-noalt,C+sub] [--repeat N] [--out DIR]

flags: --mode native|min|adaptive  --check CMD  --setup CMD  --model M  --effort E
       --budget-s N  --budget-usd N  --candidates N (deep mode)  --no-critic  --no-alt  --no-web  --full (load user MCP/skills)
       --subagents (Claude Code's Agent tool: Explore etc.)  --fresh (new session with a continuation packet)  --json
config: .qed/config.json with the same keys (check, setup, model, effort, budget_s, ...)";

extern "C" fn on_sigint(_: libc::c_int) {
    if engine::in_turn() {
        CANCEL.store(true, Ordering::SeqCst);
    } else {
        // SAFETY: write(2) and _exit(2) are async-signal-safe.
        unsafe {
            let msg = b"\x1b[?25h\n"; // restore cursor if the logo was mid-animation
            libc::write(2, msg.as_ptr().cast(), msg.len());
            libc::_exit(130);
        }
    }
}

static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
static BOOT_US: std::sync::OnceLock<u64> = std::sync::OnceLock::new();

/// Time from process start to ready, frozen at the first call (before any wait on the user).
fn boot_us() -> u64 {
    *BOOT_US.get_or_init(|| {
        START
            .get()
            .map(|t| t.elapsed().as_micros() as u64)
            .unwrap_or(0)
    })
}

fn main() {
    START.get_or_init(std::time::Instant::now);
    update::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().expect("cwd");
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => println!("{}", HELP),
        Some("-V") | Some("--version") => println!("qed {}", env!("CARGO_PKG_VERSION")),
        Some("logo") => logo::play(),
        Some("update") | Some("upgrade") => std::process::exit(update::cmd()),
        Some("check") => std::process::exit(cmd_check(&args[1..], &cwd)),
        Some("status") => cmd_status(&cwd, args.get(1).map(String::as_str)),
        Some("eval") => std::process::exit(eval::main(&args[1..], &cwd)),
        _ => {
            // SAFETY: installing a handler that only touches atomics / async-signal-safe calls.
            unsafe {
                libc::signal(libc::SIGINT, on_sigint as usize);
            }
            std::process::exit(cmd_main(&args, &cwd));
        }
    }
}

pub fn load_cfg(root: &Path) -> Cfg {
    let mut c = Cfg::default();
    let Ok(s) = std::fs::read_to_string(root.join(".qed").join("config.json")) else {
        return c;
    };
    let Ok(v) = serde_json::from_str::<Value>(&s) else {
        eprintln!("qed: invalid .qed/config.json, using defaults");
        return c;
    };
    apply_cfg(&mut c, &v);
    c
}

pub fn apply_cfg(c: &mut Cfg, v: &Value) {
    let s = |k: &str| v[k].as_str().map(String::from);
    if let Some(x) = s("model") {
        c.model = x
    }
    if let Some(x) = s("effort") {
        c.effort = x
    }
    if let Some(x) = s("tools") {
        c.tools = x
    }
    if let Some(x) = s("perm") {
        c.perm = x
    }
    if v["check"].is_string() {
        c.check = s("check")
    }
    if v["setup"].is_string() {
        c.setup = s("setup")
    }
    if let Some(x) = v["budget_s"].as_u64() {
        c.budget_s = x
    }
    if let Some(x) = v["budget_usd"].as_f64() {
        c.budget_usd = x
    }
    if let Some(x) = v["max_rounds"].as_u64() {
        c.max_rounds = x as u32
    }
    if let Some(x) = v["check_timeout_s"].as_u64() {
        c.check_timeout_s = x
    }
    if let Some(x) = v["lean"].as_bool() {
        c.lean = x
    }
    if let Some(x) = v["critic"].as_bool() {
        c.critic = x
    }
    if let Some(x) = v["alt"].as_bool() {
        c.alt = x
    }
    if let Some(x) = v["candidates"].as_u64() {
        c.candidates = x.max(1) as usize;
    }
    if let Some(x) = v["web"].as_bool() {
        c.web = x
    }
    if let Some(x) = v["ask"].as_bool() {
        c.ask = x
    }
    if let Some(x) = v["subagents"].as_bool() {
        c.subagents = x
    }
    if let Some(x) = v["autoupdate"].as_bool() {
        c.autoupdate = x
    }
}

pub fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "native" => Some(Mode::Native),
        "min" => Some(Mode::Min),
        "adaptive" => Some(Mode::Adaptive),
        _ => None,
    }
}

struct Cli {
    cfg: Cfg,
    mode: Mode,
    out: Out,
    prompt: Option<String>,
    resume: Option<Option<String>>,
    /// `qed resume` without id: choose interactively when on a terminal.
    pick: bool,
    fresh: bool,
    /// REPL: id for the next new session, reserved so Claude Code can be prewarmed with it.
    next_id: Option<String>,
}

fn parse(args: &[String], root: &Path) -> Result<Cli, String> {
    let mut c = Cli {
        cfg: load_cfg(root),
        // Measured on hard SWE-bench Verified: the protocol prompt with only user-configured
        // checks matched or beat the heavier modes at the lowest cost (see README).
        mode: Mode::Min,
        out: Out::Human,
        prompt: None,
        resume: None,
        pick: false,
        fresh: false,
        next_id: None,
    };
    let mut i = 0;
    let val = |i: &mut usize| -> Result<String, String> {
        *i += 1;
        args.get(*i)
            .cloned()
            .ok_or_else(|| format!("missing value for {}", args[*i - 1]))
    };
    while i < args.len() {
        match args[i].as_str() {
            "-p" => {
                c.out = Out::Json;
                let p = val(&mut i)?;
                c.prompt = Some(if p == "-" { read_stdin() } else { p });
            }
            "--json" => c.out = Out::Json,
            "--mode" => c.mode = parse_mode(&val(&mut i)?).ok_or("modo: native|min|adaptive")?,
            "--check" => c.cfg.check = Some(val(&mut i)?),
            "--setup" => c.cfg.setup = Some(val(&mut i)?),
            "--model" => c.cfg.model = val(&mut i)?,
            "--effort" => c.cfg.effort = val(&mut i)?,
            "--budget-s" => {
                c.cfg.budget_s = val(&mut i)?
                    .parse()
                    .map_err(|_| "budget-s must be a number")?
            }
            "--budget-usd" => {
                c.cfg.budget_usd = val(&mut i)?
                    .parse()
                    .map_err(|_| "budget-usd must be a number")?
            }
            "--no-critic" => c.cfg.critic = false,
            "--no-alt" => c.cfg.alt = false,
            "--no-web" => {
                c.cfg.web = false;
                c.cfg.tools = c
                    .cfg
                    .tools
                    .split(',')
                    .filter(|t| !t.starts_with("Web"))
                    .collect::<Vec<_>>()
                    .join(",");
            }
            "--full" => c.cfg.lean = false,
            "--ask" => c.cfg.ask = true,
            "--subagents" => c.cfg.subagents = true,
            "--candidates" | "-k" => {
                c.cfg.candidates = val(&mut i)?
                    .parse::<usize>()
                    .map_err(|_| "candidates must be a number")?
                    .max(1)
            }
            "--fresh" => c.fresh = true,
            "-c" | "--continue" => c.resume = Some(None),
            "resume" if i == 0 => {
                let id = args.get(1).filter(|a| !a.starts_with('-')).cloned();
                if id.is_some() {
                    i += 1;
                }
                c.pick = id.is_none();
                c.resume = Some(id);
            }
            a if a.starts_with('-') => return Err(format!("unknown flag: {}", a)),
            a => c.prompt = Some(a.to_string()),
        }
        i += 1;
    }
    Ok(c)
}

fn read_stdin() -> String {
    let mut s = String::new();
    let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
    s
}

fn cmd_main(args: &[String], cwd: &Path) -> i32 {
    let root = project_root(cwd);
    let cli = match parse(args, &root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qed: {}\n\n{}", e, HELP);
            return 2;
        }
    };
    let interactive = cli.prompt.is_none() && cli.out == Out::Human && term::is_tty();
    if cli.pick && interactive {
        boot_us();
        return match choose_session(&root) {
            Some(s) => repl(cli, &root, Some(s), None),
            None => 0,
        };
    }
    let mut sess = match &cli.resume {
        Some(id) => match Session::open(&root, id.as_deref()) {
            Some(s) => Some(s),
            None => {
                eprintln!("qed: session not found");
                return 1;
            }
        },
        None => None,
    };
    if let Some(p) = cli.prompt.clone() {
        // On a terminal, `qed "task"` opens a session that starts with the task and stays
        // open for follow-ups. Piped or -p: one task, then exit.
        if cli.out == Out::Human && term::is_tty() {
            return repl(cli, &root, sess, Some(p));
        }
        let ok = one(&cli, &root, &mut sess, &p);
        return if ok { 0 } else { 1 };
    }
    repl(cli, &root, sess, None)
}

/// Run one prompt as a task on (possibly new) session `sess`. Returns whether it ended cleanly.
fn one(cli: &Cli, root: &Path, sess: &mut Option<Session>, prompt: &str) -> bool {
    let created = sess.is_none();
    if created {
        match Session::create_as(root, prompt, cli.next_id.as_deref()) {
            Ok(s) => *sess = Some(s),
            Err(e) => {
                eprintln!("qed: could not create session: {}", e);
                return false;
            }
        }
    }
    let Some(s) = sess.as_mut() else { return false };
    // The user's words go into the trace (the recap on resume shows them); not into -p output.
    s.event(
        &json!({"ev": "prompt", "text": store::clip(prompt, 4000), "t": store::now_ms() as u64}),
    );
    s.mark_last();
    let mut prompt = prompt.to_string();
    // A note for the model about what happened between turns (e.g. /undo).
    if let Some(n) = s.meta["note"].as_str().filter(|n| !n.is_empty()) {
        prompt = format!("{}\n\n{}", n, prompt);
        s.set("note", json!(null));
    }
    // Checkpoint before every turn: /undo goes back to it. A session created just now
    // already hashed this exact tree as its base.
    let tree = match s.meta["base_tree"].as_str() {
        Some(b) if created => b.to_string(),
        _ => store::tree_hash(root),
    };
    if tree != "nogit" {
        let mut trees = s.meta["turn_trees"].as_array().cloned().unwrap_or_default();
        trees.push(json!(tree));
        s.set("turn_trees", json!(trees));
    }
    let mut resume = s.meta["claude_session"].as_str().map(String::from);
    if s.meta["turns"].as_u64().unwrap_or(0) > 0 {
        let g = format!(
            "{}\nFollow-up: {}",
            s.meta["goal"].as_str().unwrap_or(""),
            prompt
        );
        s.set("goal", json!(store::clip(&g, 4000)));
    }
    if cli.fresh && resume.is_some() {
        prompt = run::packet(s, root, &format!("\nNew instruction: {}", prompt));
        resume = None;
    }
    // Esc / Ctrl-C cancel and type-ahead only make sense on an interactive terminal.
    engine::CANCEL.store(false, std::sync::atomic::Ordering::SeqCst);
    let _keys = (cli.out == Out::Human && term::is_tty()).then(term::KeyWatch::start);
    let mut em = Emitter::new(cli.out, Some(s));
    let o = run::task(&cli.cfg, cli.mode, &prompt, resume, &mut em);
    !o.cancelled && o.passed != Some(false)
}

/// Most turns a goal may take before qed stops and hands back to the person.
const GOAL_TURNS: usize = 5;

/// `/goal`: set an objective and keep taking turns until the model reports it met
/// (its last line `GOAL: met`), the person cancels, or GOAL_TURNS run out.
fn goal(cli: &Cli, root: &Path, sess: &mut Option<Session>, objective: &str) {
    if sess.is_none() {
        match Session::create_as(root, objective, cli.next_id.as_deref()) {
            Ok(s) => *sess = Some(s),
            Err(e) => return eprintln!("qed: could not create session: {}", e),
        }
    }
    let mut first = 0;
    if let Some(s) = sess.as_mut() {
        s.set("objective", json!(objective));
        s.set("objective_status", json!("active"));
        first = s.meta["turn_trees"].as_array().map_or(0, |a| a.len());
    }
    goal_turns(cli, root, sess, objective);
    // The whole goal is one step for /undo: keep only the checkpoint from before it.
    if let Some(s) = sess.as_mut() {
        let mut t = s.meta["turn_trees"].as_array().cloned().unwrap_or_default();
        t.truncate(first + 1);
        s.set("turn_trees", json!(t));
    }
}

fn goal_turns(cli: &Cli, root: &Path, sess: &mut Option<Session>, objective: &str) {
    const ASK: &str = "End your final message with exactly one line: `GOAL: met` or `GOAL: not met — <what remains>`.";
    let mut prompt = format!(
        "Goal: {}\n\nWork toward this goal until it is fully achieved, verifying with `qed check -- <cmd>` where possible. {}",
        objective, ASK
    );
    for i in 1..=GOAL_TURNS {
        if i > 1 {
            println!("{}◎{} {}continuing toward the goal · turn {}/{}{}", ui::ACCENT, ui::RST, ui::DIM, i, GOAL_TURNS, ui::RST);
        }
        one(cli, root, sess, &prompt);
        let Some(s) = sess.as_mut() else { return };
        if s.meta["status"] == "interrupted" {
            dim("goal paused · /goal shows it, a message continues");
            return;
        }
        let text = s.meta["last_text"].as_str().unwrap_or("").to_string();
        let verdict = text.lines().rev().find(|l| l.trim_start().starts_with("GOAL:")).unwrap_or("").trim().to_string();
        if verdict.starts_with("GOAL: met") {
            s.set("objective_status", json!("met"));
            println!("{}◎{} goal met {}· {}{}", ui::ACCENT, ui::RST, ui::DIM, ui::fit(objective, 50), ui::RST);
            return;
        }
        let remains = verdict.trim_start_matches("GOAL:").trim().trim_start_matches("not met").trim_start_matches([' ', '—', '-', ':']).to_string();
        prompt = format!(
            "Continue toward the goal: {}\nWhat remains{}. {}",
            objective,
            if remains.is_empty() { String::new() } else { format!(": {}", remains) },
            ASK
        );
    }
    dim(&format!("goal not met after {} turns · still active, a message continues", GOAL_TURNS));
}

/// `/undo`: put the files back as they were before the last turn. The tree being left is
/// kept under refs/qed/ so the undo itself can be undone; the model is told next turn.
fn undo(s: &mut Session, root: &Path) {
    let mut trees = s.meta["turn_trees"].as_array().cloned().unwrap_or_default();
    let Some(t) = trees.pop().and_then(|t| t.as_str().map(String::from)) else {
        return dim("nothing to undo");
    };
    let cur = store::tree_hash(root);
    let n = trees.len();
    let id = s.meta["id"].as_str().unwrap_or("x").to_string();
    let saved = store::checkpoint(root, &cur, &format!("{}/undo-{}", id, n));
    let changed = store::restore_tree(root, &t);
    s.set("turn_trees", json!(trees));
    s.set("note", json!("Note: the person reverted the file changes of your previous turn with /undo; the files are back to how they were before it."));
    dim(&format!("undid last turn · {} file{} restored", changed, if changed == 1 { "" } else { "s" }));
    if let Some(c) = saved {
        dim(&format!("redo: git restore -s {} -W -- .", &c[..c.len().min(10)]));
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::Native => "native",
        Mode::Min => "min",
        Mode::Adaptive => "adaptive",
    }
}

fn dim(s: &str) {
    println!("  \x1b[2m{}\x1b[0m", s);
}

fn on_off(v: &str, cur: bool) -> bool {
    match v {
        "on" | "1" | "true" | "si" => true,
        "off" | "0" | "false" | "no" => false,
        _ => !cur,
    }
}

fn on_off_s(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// Full configuration as an aligned key/value list (/config).
fn print_config(cli: &Cli) {
    let c = &cli.cfg;
    let rows = [
        ("model", c.model.clone()),
        ("effort", c.effort.clone()),
        ("mode", mode_name(cli.mode).to_string()),
        ("check", c.check.clone().unwrap_or("—".into())),
        (
            "budget",
            format!("${:.2} · {}", c.budget_usd, ui::secs(c.budget_s as f64)),
        ),
        (
            "candidates",
            if c.candidates > 1 {
                format!("{} (deep: parallel + cross-check selection)", c.candidates)
            } else {
                "1".into()
            },
        ),
        ("critic", on_off_s(c.critic).into()),
        ("alt", on_off_s(c.alt).into()),
        ("web", on_off_s(c.web).into()),
        ("subagents", on_off_s(c.subagents).into()),
    ];
    for (k, v) in rows {
        println!("  \x1b[2m{:<9}\x1b[0m {}", k, v);
    }
}

fn print_help() {
    for (c, d) in term::COMMANDS {
        println!("  {:<12} \x1b[2m{}\x1b[0m", c, d);
    }
    println!(
        "  {:<10} \x1b[2mrun in the shell, bypassing the model\x1b[0m",
        "!cmd"
    );
    let sk = term::skills();
    if !sk.is_empty() {
        println!("  {:<12} \x1b[2m{} installed · type / and part of a name\x1b[0m", "skills", sk.len());
    }
    dim("esc cancels · ↑↓ history · tab completes · \\+enter new line");
}

/// Pick a recent session with the arrow keys (the last used one first).
fn choose_session(root: &Path) -> Option<Session> {
    let mut list = Session::recent(root, 9);
    if list.is_empty() {
        dim("no sessions in this project");
        return None;
    }
    let items: Vec<(String, String)> = list
        .iter()
        .map(|s| {
            let mut d = format!(
                "{} · ${:.2}",
                ui::ago(s.touched_ms()),
                s.meta["cost_usd"].as_f64().unwrap_or(0.0)
            );
            if s.meta["status"] == "interrupted" {
                d.push_str(" · interrupted");
            }
            (s.title(), d)
        })
        .collect();
    let i = term::pick("resume · ↑↓ enter · esc", &items)?;
    Some(list.swap_remove(i))
}

/// Where the session left off: title, then the last exchange in a few dim lines.
fn show_recap(s: &Session) {
    let w = term::width().saturating_sub(6);
    let mut meta = format!(
        "{} · ${:.2}",
        ui::ago(s.touched_ms()),
        s.meta["cost_usd"].as_f64().unwrap_or(0.0)
    );
    if s.meta["status"] == "interrupted" {
        meta.push_str(" · interrupted");
    }
    println!(
        "{}↺{} {}  {}{}{}",
        ui::ACCENT,
        ui::RST,
        ui::fit(&s.title(), w.saturating_sub(meta.chars().count() + 2)),
        ui::DIM,
        meta,
        ui::RST
    );
    let (prompt, reply) = s.recap();
    let p = prompt
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if !p.is_empty() && p != s.title() {
        dim(&ui::fit(&format!("› {}", p), w));
    }
    let lines: Vec<&str> = reply.lines().filter(|l| !l.trim().is_empty()).collect();
    for l in &lines[lines.len().saturating_sub(4)..] {
        let l = l.trim().replace("**", "").replace('`', "");
        let l = match l.strip_prefix("- ") {
            Some(b) => format!("• {}", b),
            None => l,
        };
        dim(&format!("  {}", ui::fit(&l, w.saturating_sub(2))));
    }
    println!();
}

fn read_input(tty: bool, hist: &mut Vec<String>, prefill: &str) -> Option<String> {
    if tty {
        return term::read_line("\x1b[1m› \x1b[0m", 2, prefill, hist);
    }
    print!("› ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches('\n').to_string()),
    }
}

/// One quiet line: model · effort · project [· boot time].
fn header(cli: &Cli, root: &Path, boot: Option<u64>) {
    println!(
        "{}∎{} qed  \x1b[2m{} · {} · {}{}\x1b[0m\n",
        ui::ACCENT,
        ui::RST,
        cli.cfg.model.trim_start_matches("claude-"),
        cli.cfg.effort,
        root.file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default(),
        boot.map(|b| format!(" · {}", engine::fmt_us(b)))
            .unwrap_or_default()
    );
}

/// Prewarm the process for the next author turn: the current session (resumed with its
/// Claude Code id) or a new one at a reserved id.
fn prewarm(cli: &mut Cli, root: &Path, sess: &Option<Session>) {
    let (dir, resume) = match sess {
        Some(s) => (s.dir.clone(), s.meta["claude_session"].as_str().map(String::from)),
        None => {
            let id = cli.next_id.get_or_insert_with(Session::new_id).clone();
            (Session::sessions_dir(root).join(id), None)
        }
    };
    run::prewarm(&cli.cfg, cli.mode, root, &dir, resume);
}

fn repl(cli: Cli, root: &Path, sess: Option<Session>, first: Option<String>) -> i32 {
    engine::PERSIST.store(true, std::sync::atomic::Ordering::SeqCst);
    if term::is_tty() {
        update::background(cli.cfg.autoupdate);
    }
    let code = repl_loop(cli, root, sess, first);
    engine::shutdown();
    code
}

fn repl_loop(mut cli: Cli, root: &Path, mut sess: Option<Session>, first: Option<String>) -> i32 {
    // One quiet line, measured from process start. The logo animation is opt-in
    // (`qed logo`, /logo, QED_LOGO=1) so opening qed never waits on it.
    let boot = boot_us();
    if logo::enabled() {
        logo::play();
    }
    header(&cli, root, Some(boot));
    term::load_skills(root);
    if let Some(s) = &sess {
        show_recap(s);
    }
    let tty = term::is_tty();
    let mut hist = term::history();
    let mut prefill = String::new();
    let mut pending: Vec<String> = first.into_iter().collect();
    let mut announced = false;
    loop {
        if !announced {
            if let Some(v) = update::READY.lock().ok().and_then(|r| r.clone()) {
                announced = true;
                println!("{}↑{} {}qed {} installed · /restart to use it{}", ui::ACCENT, ui::RST, ui::DIM, v, ui::RST);
            }
        }
        // Start the next turn's Claude Code process while the person types.
        if pending.is_empty() {
            prewarm(&mut cli, root, &sess);
        }
        let input = match pending.is_empty() {
            false => {
                let p = pending.remove(0);
                println!("\x1b[1m› \x1b[0m{}", p);
                p
            }
            true => {
                if let Ok(mut f) = term::FOOTER.lock() {
                    *f = format!(
                        "{}{} · {} · {}{}{}{}{}   / commands · ! shell · esc cancels",
                        sess.as_ref()
                            .and_then(|s| s.meta["objective"].as_str())
                            .filter(|_| sess.as_ref().is_some_and(|s| s.meta["objective_status"] == "active"))
                            .map(|g| format!("◎ {} · ", ui::fit(g, 24)))
                            .unwrap_or_default(),
                        cli.cfg.model.trim_start_matches("claude-"),
                        cli.cfg.effort,
                        mode_name(cli.mode),
                        if cli.cfg.ask { " · ask" } else { "" },
                        sess.as_ref()
                            .and_then(|s| Some((s.meta["ctx"].as_u64()?, s.meta["ctx_window"].as_u64().unwrap_or(0))))
                            .map(|(c, w)| format!(" · {}", ui::ctx(c, w)))
                            .unwrap_or_default(),
                        sess.as_ref()
                            .and_then(|s| s.meta["cost_usd"].as_f64())
                            .filter(|c| *c > 0.0)
                            .map(|c| format!(" · {}", ui::money(c)))
                            .unwrap_or_default(),
                        update::READY
                            .lock()
                            .ok()
                            .and_then(|r| r.clone())
                            .map(|v| format!(" · ↑ {} /restart", v))
                            .unwrap_or_default()
                    );
                }
                match read_input(tty, &mut hist, &std::mem::take(&mut prefill)) {
                    Some(l) => l,
                    None => return 0,
                }
            }
        };
        let input = input.trim();
        if input.is_empty() {
            continue;
        }
        let (cmd, arg) = input
            .split_once(' ')
            .map(|(a, b)| (a, b.trim()))
            .unwrap_or((input, ""));
        match cmd {
            "/q" | "/quit" | "/exit" => return 0,
            "/help" | "/?" => print_help(),
            "/model" if arg.is_empty() => dim(&format!("model {} · options: {}", cli.cfg.model, MODELS.join(", "))),
            "/model" => {
                cli.cfg.model = arg.to_string();
                dim(&format!("model {}", arg));
            }
            "/effort" if EFFORTS.contains(&arg) => {
                cli.cfg.effort = arg.to_string();
                dim(&format!("effort {}", arg));
            }
            "/effort" => dim(&format!("effort {} · options: {}", cli.cfg.effort, EFFORTS.join(", "))),
            "/mode" => match parse_mode(arg) {
                Some(m) => {
                    cli.mode = m;
                    dim(&format!("mode {}", arg));
                }
                None => dim("native: plain Claude Code · min: + evidence · adaptive: + critic and alt branch"),
            },
            "/check" if arg.is_empty() => dim(&format!("check {}", cli.cfg.check.as_deref().unwrap_or("—"))),
            "/check" if arg == "off" => {
                cli.cfg.check = None;
                dim("check off");
            }
            "/check" => {
                cli.cfg.check = Some(arg.to_string());
                let mut e = store::run_check(sess.as_ref(), root, arg, "user", cli.cfg.check_timeout_s);
                e["ev"] = json!("check");
                Emitter::new(Out::Human, None).emit(e);
            }
            "/budget" => {
                let mut it = arg.split_whitespace();
                if let Some(u) = it.next().and_then(|v| v.parse().ok()) {
                    cli.cfg.budget_usd = u;
                }
                if let Some(t) = it.next().and_then(|v| v.parse().ok()) {
                    cli.cfg.budget_s = t;
                }
                dim(&format!("budget ${:.2} · {}", cli.cfg.budget_usd, ui::secs(cli.cfg.budget_s as f64)));
            }
            "/candidates" => {
                match arg.parse::<usize>() {
                    Ok(k) if k >= 1 => cli.cfg.candidates = k,
                    _ => {}
                }
                dim(&format!(
                    "candidates {}{}",
                    cli.cfg.candidates,
                    if cli.cfg.candidates > 1 { " · deep: parallel attempts, selection by cross-executed checks" } else { "" }
                ));
            }
            "/critic" => {
                cli.cfg.critic = on_off(arg, cli.cfg.critic);
                dim(&format!("critic {}", on_off_s(cli.cfg.critic)));
            }
            "/alt" => {
                cli.cfg.alt = on_off(arg, cli.cfg.alt);
                dim(&format!("alt {}", on_off_s(cli.cfg.alt)));
            }
            "/web" => {
                cli.cfg.web = on_off(arg, cli.cfg.web);
                let base = cli.cfg.tools.split(',').filter(|t| !t.starts_with("Web")).collect::<Vec<_>>().join(",");
                cli.cfg.tools = if cli.cfg.web { format!("{},WebSearch,WebFetch", base) } else { base };
                dim(&format!("web {}", on_off_s(cli.cfg.web)));
            }
            "/subagents" => {
                cli.cfg.subagents = on_off(arg, cli.cfg.subagents);
                dim(&format!(
                    "subagents {}",
                    if cli.cfg.subagents { "on · Explore etc. for broad searches; ~2.3k more prompt tokens per request" } else { "off" }
                ));
            }
            "/config" => print_config(&cli),
            "/perm" => {
                match arg {
                    "ask" => cli.cfg.ask = true,
                    "auto" => cli.cfg.ask = false,
                    _ => {}
                }
                dim(&format!(
                    "perm {}",
                    if cli.cfg.ask { "ask · shell and web fetches need your ok; edits and checks run" } else { "auto · tools run without asking" }
                ));
            }
            "/goal" if arg.is_empty() => match sess.as_ref().and_then(|s| s.meta["objective"].as_str().map(|g| (g.to_string(), s.meta["objective_status"].as_str().unwrap_or("").to_string()))) {
                Some((g, st)) => println!("{}◎{} {} {}· {}{}", ui::ACCENT, ui::RST, g, ui::DIM, st, ui::RST),
                None => dim("no goal · /goal <objective> keeps working until it is met"),
            },
            "/goal" if arg == "clear" => {
                if let Some(s) = sess.as_mut() {
                    s.set("objective", json!(null));
                    s.set("objective_status", json!(null));
                }
                dim("goal cleared");
            }
            "/goal" => goal(&cli, root, &mut sess, arg),
            "/undo" => match sess.as_mut() {
                Some(s) => undo(s, root),
                None => dim("nothing to undo"),
            },
            "/thinking" => dim("in Claude Code, thinking is controlled with /effort"),
            c if c.starts_with('!') => {
                let (code, out) = store::sh_timeout(input[1..].trim(), root, cli.cfg.check_timeout_s);
                print!("{}", out);
                if code != 0 {
                    dim(&format!("exit {}", code));
                }
            }
            "/logo" => logo::play(),
            "/restart" => restart(&cli, &sess),
            "/new" => {
                sess = None;
                cli.next_id = None;
                engine::shutdown();
                if tty {
                    // Clear the screen and scrollback so the new session starts clean.
                    print!("\x1b[2J\x1b[3J\x1b[H");
                    header(&cli, root, None);
                } else {
                    dim("new session");
                }
            }
            "/resume" | "/sessions" => {
                let picked = if arg.is_empty() && tty {
                    choose_session(root)
                } else {
                    let s = Session::open(root, (!arg.is_empty()).then_some(arg));
                    if s.is_none() {
                        dim("session not found · /resume to pick");
                    }
                    s
                };
                if let Some(s) = picked {
                    println!();
                    show_recap(&s);
                    sess = Some(s);
                }
            }
            "/status" => match &sess {
                Some(s) => print_status(s),
                None => dim("no session"),
            },
            "/cost" => match &sess {
                Some(s) => dim(&format!("{} · {} calls", ui::money(s.meta["cost_usd"].as_f64().unwrap_or(0.0)), s.meta["calls"])),
                None => dim("no session"),
            },
            "/diff" => {
                if let Some(s) = &sess {
                    let d = store::diff_since(root, s.meta["base_tree"].as_str().unwrap_or("nogit"));
                    println!("{}", if d.is_empty() { "(no changes)".into() } else { d });
                }
            }
            "/fresh" if !arg.is_empty() => {
                cli.fresh = true;
                one(&cli, root, &mut sess, arg);
                cli.fresh = false;
            }
            c if c.starts_with('/') && term::expand_skill(input).is_some() => {
                if let Some(p) = term::expand_skill(input) {
                    one(&cli, root, &mut sess, &p);
                }
            }
            // A path like /Users/x is a prompt, not a command.
            c if c.starts_with('/') && !c[1..].contains('/') => {
                dim(&format!("unknown command {} · /help", c));
            }
            _ => {
                one(&cli, root, &mut sess, input);
            }
        }
        // Type-ahead from the turn: confirmed lines run next, a partial line pre-fills the editor.
        if let Ok(mut q) = term::QUEUE.lock() {
            pending.append(&mut q);
        }
        if let Ok(mut t) = term::TYPEAHEAD.lock() {
            prefill = std::mem::take(&mut *t);
        }
    }
}

/// Replace this process with the installed binary (a fresh build or an update), carrying
/// on the current session with the same model and effort.
fn restart(cli: &Cli, sess: &Option<Session>) {
    use std::os::unix::process::CommandExt;
    engine::shutdown();
    let mut args: Vec<String> = Vec::new();
    if let Some(s) = sess {
        args.extend(["resume".into(), s.id()]);
    }
    args.extend(["--model".into(), cli.cfg.model.clone(), "--effort".into(), cli.cfg.effort.clone()]);
    if !cli.cfg.lean {
        args.push("--full".into());
    }
    if cli.cfg.subagents {
        args.push("--subagents".into());
    }
    if cli.cfg.ask {
        args.push("--ask".into());
    }
    let _ = std::io::stdout().flush();
    let err = std::process::Command::new(update::exe()).args(&args).exec();
    dim(&format!("restart failed: {}", err));
}

fn cmd_check(args: &[String], cwd: &Path) -> i32 {
    let mut timeout = 600;
    let mut i = 0;
    while i < args.len() && args[i] != "--" {
        if args[i] == "-t" {
            timeout = args
                .get(i + 1)
                .and_then(|v| v.parse().ok())
                .unwrap_or(timeout);
            i += 1;
        }
        i += 1;
    }
    let cmd = args.get(i + 1..).map(shell_join).unwrap_or_default();
    if cmd.trim().is_empty() {
        eprintln!("usage: qed check [-t SECS] -- CMD");
        return 2;
    }
    let root = project_root(cwd);
    let sess = std::env::var("QED_SESSION")
        .ok()
        .and_then(|d| Session::open_dir(&PathBuf::from(d), &root))
        .or_else(|| Session::open(&root, None));
    let by = if std::env::var("QED_SESSION").is_ok() {
        "model"
    } else {
        "user"
    };
    let ev = store::run_check(sess.as_ref(), cwd, &cmd, by, timeout);
    let code = ev["exit"].as_i64().unwrap_or(1) as i32;
    println!("{}", store::tail(ev["tail"].as_str().unwrap_or(""), 60));
    println!(
        "[qed check] exit {} · tree {} · log {}",
        code,
        &ev["tree"].as_str().unwrap_or("")[..8.min(ev["tree"].as_str().unwrap_or("").len())],
        ev["log"].as_str().unwrap_or("(no session: not recorded)")
    );
    code
}

/// The command after `--` as one shell string. A single argument is a script as written
/// (`qed check -- 'a && b'`). Several arguments were already split by the caller's shell,
/// so each is quoted only if it needs it: plain words stay as typed (`python3 test.py`),
/// which keeps them comparable with the configured check.
fn shell_join(args: &[String]) -> String {
    if args.len() == 1 {
        return args[0].clone();
    }
    let plain = |a: &str| {
        !a.is_empty()
            && a.chars()
                .all(|c| c.is_ascii_alphanumeric() || "_-./=:,@%+".contains(c))
    };
    args.iter()
        .map(|a| {
            if plain(a) {
                a.clone()
            } else {
                format!("'{}'", a.replace('\'', r"'\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cmd_status(cwd: &Path, id: Option<&str>) {
    let root = project_root(cwd);
    match Session::open(&root, id) {
        Some(s) => print_status(&s),
        None => println!("no sessions in {}", root.display()),
    }
}

fn print_status(s: &Session) {
    let cur = tree_hash(&s.root);
    println!(
        "session {} · {} · {} calls · ${:.3}\ngoal: {}",
        s.id(),
        s.meta["status"].as_str().unwrap_or(""),
        s.meta["calls"],
        s.meta["cost_usd"].as_f64().unwrap_or(0.0),
        store::clip(s.meta["goal"].as_str().unwrap_or(""), 300)
    );
    for (e, valid) in store::check_status(&s.ledger(), &cur) {
        println!(
            "  {} {:<7} exit {:<3} {} `{}`",
            if valid {
                "\x1b[32mvalid\x1b[0m"
            } else {
                "\x1b[2mstale\x1b[0m"
            },
            e["by"].as_str().unwrap_or(""),
            e["exit"],
            &e["tree"].as_str().unwrap_or("")[..8.min(e["tree"].as_str().unwrap_or("").len())],
            e["cmd"].as_str().unwrap_or("")
        );
    }
    for e in s.ledger().iter().filter(|e| e["kind"] == "critic") {
        let tag = if e["observed"] == true {
            "observed"
        } else {
            "inferred"
        };
        println!(
            "  critic [{}] {}: {}",
            tag,
            e["verdict"].as_str().unwrap_or(""),
            e["finding"].as_str().unwrap_or("")
        );
    }
}
