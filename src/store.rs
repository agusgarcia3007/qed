//! Session store: meta, event trace, evidence ledger, full logs, git state.
//! Everything is plain files under `.qed/` so traces are greppable and diffable.

use serde_json::{json, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Run a git command in `dir`; returns trimmed stdout on success.
/// Raw stdout (no trimming): patches must keep trailing blank context lines and the final
/// newline, or `git apply`/`patch` reject them as malformed.
pub fn git_raw(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).to_string())
}

pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

pub fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<String> {
    let mut c = Command::new("git");
    c.args(args).current_dir(dir).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).trim_end().to_string())
}

/// Content hash of the exact working tree (tracked + untracked, minus ignored), the same
/// from any subdirectory. A check is valid only for the tree it ran on.
/// Uses a scratch index kept in the git dir (`qed-index`, one per worktree) so files whose
/// stat didn't change, untracked ones included, are not re-read; seeded from the real
/// index the first time. Each call works on a private copy and renames it into place.
pub fn tree_hash(dir: &Path) -> String {
    let Some(paths) = git(
        dir,
        &[
            "rev-parse",
            "--is-inside-work-tree",
            "--path-format=absolute",
            "--git-path",
            "index",
            "--git-path",
            "qed-index",
        ],
    ) else {
        return "nogit".into();
    };
    let mut it = paths.lines();
    let (Some("true"), Some(real), Some(cache)) = (it.next(), it.next(), it.next()) else {
        return "nogit".into();
    };
    let tmp = format!("{}.{}-{}", cache, std::process::id(), now_ms());
    let seed = if Path::new(cache).exists() { cache } else { real };
    let _ = fs::copy(seed, &tmp);
    let env = [("GIT_INDEX_FILE", tmp.as_str())];
    // `:/` and `top` make the pathspecs relative to the worktree root, not to `dir`.
    let _ = git_env(dir, &["add", "-A", "--", ":/", ":(top,exclude).qed"], &env);
    match git_env(dir, &["write-tree"], &env) {
        Some(t) => {
            let _ = fs::rename(&tmp, cache);
            t
        }
        None => {
            let _ = fs::remove_file(&tmp);
            "unknown".into()
        }
    }
}

/// Persist a tree as a commit under refs/qed/ so it survives gc; returns commit sha.
pub fn checkpoint(dir: &Path, tree: &str, name: &str) -> Option<String> {
    let parent = git(dir, &["rev-parse", "HEAD"]);
    let mut args = vec!["commit-tree", tree, "-m", name];
    if let Some(p) = parent.as_deref() {
        args.push("-p");
        args.push(p);
    }
    let c = git_env(
        dir,
        &args,
        &[
            ("GIT_AUTHOR_NAME", "qed"),
            ("GIT_AUTHOR_EMAIL", "qed@localhost"),
            ("GIT_COMMITTER_NAME", "qed"),
            ("GIT_COMMITTER_EMAIL", "qed@localhost"),
        ],
    )?;
    git(dir, &["update-ref", &format!("refs/qed/{}", name), &c]);
    Some(c)
}

/// Make the working tree match `tree` (files only; the index and HEAD are untouched):
/// files added since are removed, changed or deleted ones come back. Returns how many
/// paths changed.
pub fn restore_tree(dir: &Path, tree: &str) -> usize {
    let cur = tree_hash(dir);
    if cur == tree {
        return 0;
    }
    let out = git_raw(dir, &["diff", "--name-status", "--no-renames", "-z", tree, &cur]).unwrap_or_default();
    let mut f = out.split('\0').filter(|x| !x.is_empty());
    let mut back: Vec<String> = Vec::new();
    let mut n = 0;
    while let (Some(st), Some(path)) = (f.next(), f.next()) {
        n += 1;
        if st == "A" {
            let _ = fs::remove_file(dir.join(path));
        } else {
            back.push(path.to_string());
        }
    }
    if !back.is_empty() {
        let mut args = vec!["restore", "--source", tree, "--worktree", "--"];
        args.extend(back.iter().map(String::as_str));
        git(dir, &args);
    }
    n
}

/// Diff between a checkpoint tree and the current working tree.
pub fn diff_since(dir: &Path, base_tree: &str) -> String {
    if base_tree == "nogit" {
        return String::new();
    }
    let cur = tree_hash(dir);
    git_raw(dir, &["diff", "--no-color", base_tree, &cur]).unwrap_or_default()
}

/// Worktree root: nearest ancestor holding a `.git` dir or file (worktrees/submodules).
/// Walking the filesystem avoids spawning git on every startup; git is still asked
/// when GIT_DIR/GIT_WORK_TREE override discovery.
pub fn project_root(cwd: &Path) -> PathBuf {
    if std::env::var_os("GIT_DIR").is_some() || std::env::var_os("GIT_WORK_TREE").is_some() {
        return git(cwd, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
    }
    cwd.ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(cwd)
        .to_path_buf()
}

fn ensure_excluded(root: &Path) {
    if let Some(p) = git(
        root,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "info/exclude",
        ],
    ) {
        let p = PathBuf::from(p);
        let cur = fs::read_to_string(&p).unwrap_or_default();
        if !cur.lines().any(|l| l.trim() == ".qed/") {
            if let Some(parent) = p.parent() {
                let _ = fs::create_dir_all(parent);
            }
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&p)
                .and_then(|mut f| writeln!(f, ".qed/"));
        }
    }
}

pub struct Session {
    pub dir: PathBuf,
    pub root: PathBuf,
    pub meta: Value,
    events: Option<File>,
}

impl Session {
    pub fn sessions_dir(root: &Path) -> PathBuf {
        root.join(".qed").join("sessions")
    }

    /// A fresh session id: seconds since the epoch plus 16 bits of time and pid.
    pub fn new_id() -> String {
        let t = now_ms();
        format!(
            "{}-{:04x}",
            t / 1000,
            (t as u32 ^ std::process::id()) & 0xffff
        )
    }

    pub fn create(root: &Path, goal: &str) -> std::io::Result<Session> {
        Self::create_as(root, goal, None)
    }

    /// Create a session, with an id reserved earlier (the REPL prewarms Claude Code with
    /// that session's directory before the first prompt) or a new one.
    pub fn create_as(root: &Path, goal: &str, id: Option<&str>) -> std::io::Result<Session> {
        ensure_excluded(root);
        let t = now_ms();
        let id = match id {
            Some(i) if !Self::sessions_dir(root).join(i).exists() => i.to_string(),
            _ => Self::new_id(),
        };
        let dir = Self::sessions_dir(root).join(&id);
        fs::create_dir_all(dir.join("logs"))?;
        let base_tree = tree_hash(root);
        let base_ref = if base_tree != "nogit" {
            checkpoint(root, &base_tree, &format!("{}/base", id))
        } else {
            None
        };
        let meta = json!({
            "id": id, "goal": goal, "root": root, "created_ms": t as u64,
            "base_tree": base_tree, "base_ref": base_ref, "claude_session": null,
            "status": "new", "turns": 0, "cost_usd": 0.0, "calls": 0
        });
        let mut s = Session {
            dir,
            root: root.to_path_buf(),
            meta,
            events: None,
        };
        s.save();
        let _ = fs::write(Self::sessions_dir(root).join("LAST"), &id);
        Ok(s)
    }

    /// Open by id (a unique fragment of it is enough) or the last used session.
    pub fn open(root: &Path, id: Option<&str>) -> Option<Session> {
        let sd = Self::sessions_dir(root);
        let id = match id {
            Some(i) if sd.join(i).is_dir() => i.to_string(),
            Some(i) if !i.is_empty() && !i.contains('/') => {
                let hits: Vec<String> = Self::ids(root)
                    .into_iter()
                    .filter(|d| d.contains(i))
                    .collect();
                match hits.as_slice() {
                    [one] => one.clone(),
                    _ => return None,
                }
            }
            Some(_) => return None,
            None => fs::read_to_string(sd.join("LAST")).ok()?.trim().to_string(),
        };
        Self::open_dir(&sd.join(&id), root)
    }

    fn ids(root: &Path) -> Vec<String> {
        fs::read_dir(Self::sessions_dir(root))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect()
    }

    /// Up to `n` sessions, most recently used first (meta.json is rewritten on every turn).
    pub fn recent(root: &Path, n: usize) -> Vec<Session> {
        let sd = Self::sessions_dir(root);
        let mut v: Vec<(std::time::SystemTime, Session)> = Self::ids(root)
            .into_iter()
            .filter_map(|id| {
                let m = fs::metadata(sd.join(&id).join("meta.json"))
                    .ok()?
                    .modified()
                    .ok()?;
                Some((m, Self::open_dir(&sd.join(&id), root)?))
            })
            .collect();
        v.sort_by(|a, b| b.0.cmp(&a.0));
        v.into_iter().take(n).map(|(_, s)| s).collect()
    }

    /// Last activity in ms since the epoch.
    pub fn touched_ms(&self) -> u128 {
        fs::metadata(self.dir.join("meta.json"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// Make this the session `qed -c` / `qed resume` continue.
    pub fn mark_last(&self) {
        let _ = fs::write(Self::sessions_dir(&self.root).join("LAST"), self.id());
    }

    /// The original task: first line of the goal.
    pub fn title(&self) -> String {
        let g = self.meta["goal"].as_str().unwrap_or("");
        g.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim()
            .to_string()
    }

    /// Last exchange from the trace: (user prompt, model reply). Traces from before
    /// prompts were recorded fall back to the last instruction kept in the goal.
    pub fn recap(&self) -> (String, String) {
        let (mut prompt, mut reply, mut cur) = (None::<String>, String::new(), String::new());
        let f = fs::read_to_string(self.dir.join("events.jsonl")).unwrap_or_default();
        for l in f.lines() {
            let Ok(e) = serde_json::from_str::<Value>(l) else {
                continue;
            };
            match e["ev"].as_str().unwrap_or("") {
                "prompt" => {
                    prompt = e["text"].as_str().map(String::from);
                    cur.clear();
                    reply.clear();
                }
                "text" => cur.push_str(e["text"].as_str().unwrap_or("")),
                // On screen a tool call breaks the prose; keep that break in the recap.
                "tool" if !cur.is_empty() && !cur.ends_with('\n') => cur.push('\n'),
                "done" | "cancelled" if !cur.trim().is_empty() => reply = std::mem::take(&mut cur),
                _ => {}
            }
        }
        if !cur.trim().is_empty() {
            reply = cur;
        }
        let prompt = prompt.unwrap_or_else(|| {
            let g = self.meta["goal"].as_str().unwrap_or("");
            g.rsplit("\nFollow-up: ").next().unwrap_or(g).to_string()
        });
        (prompt, reply)
    }

    pub fn open_dir(dir: &Path, root: &Path) -> Option<Session> {
        let meta: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
        Some(Session {
            dir: dir.to_path_buf(),
            root: root.to_path_buf(),
            meta,
            events: None,
        })
    }

    pub fn id(&self) -> String {
        self.meta["id"].as_str().unwrap_or("").to_string()
    }

    pub fn save(&mut self) {
        let _ = fs::write(
            self.dir.join("meta.json"),
            serde_json::to_vec_pretty(&self.meta).unwrap_or_default(),
        );
    }

    pub fn set(&mut self, k: &str, v: Value) {
        self.meta[k] = v;
        self.save();
    }

    pub fn event(&mut self, ev: &Value) {
        if self.events.is_none() {
            self.events = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.dir.join("events.jsonl"))
                .ok();
        }
        if let Some(f) = self.events.as_mut() {
            let _ = writeln!(f, "{}", ev);
        }
    }

    pub fn evidence(&self, ev: Value) {
        append_jsonl(&self.dir.join("evidence.jsonl"), &ev);
    }

    pub fn ledger(&self) -> Vec<Value> {
        read_jsonl(&self.dir.join("evidence.jsonl"))
    }

    /// Store a large output out of the prompt; return its path.
    pub fn blob(&self, tag: &str, data: &str) -> PathBuf {
        let p = self
            .dir
            .join("logs")
            .join(format!("{}-{}.log", now_ms(), tag));
        let _ = fs::write(&p, data);
        p
    }

    pub fn add_cost(&mut self, usd: f64, calls: u64) {
        let c = self.meta["cost_usd"].as_f64().unwrap_or(0.0) + usd;
        let n = self.meta["calls"].as_u64().unwrap_or(0) + calls;
        self.meta["cost_usd"] = json!(c);
        self.meta["calls"] = json!(n);
        self.save();
    }
}

pub fn append_jsonl(p: &Path, v: &Value) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(p) {
        let _ = writeln!(f, "{}", v);
    }
}

pub fn read_jsonl(p: &Path) -> Vec<Value> {
    fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

pub fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    let start = lines.len().saturating_sub(n);
    let mut out = String::new();
    if start > 0 {
        out.push_str(&format!("[... {} lines omitted]\n", start));
    }
    out.push_str(&lines[start..].join("\n"));
    out
}

pub fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[... clipped {} bytes]", &s[..cut], s.len() - cut)
}

/// Failure fingerprint: last lines with digits/hex/paths-of-tmp normalized, so
/// "same failure, different timing" collapses to one signature.
pub fn fingerprint(exit: i32, out: &str) -> String {
    let t = tail(out, 15);
    let mut norm = String::with_capacity(t.len());
    let mut prev_digit = false;
    for ch in t.chars() {
        if ch.is_ascii_digit() {
            if !prev_digit {
                norm.push('#');
            }
            prev_digit = true;
        } else {
            prev_digit = false;
            norm.push(ch);
        }
    }
    // FNV-1a
    let mut h: u64 = 0xcbf29ce484222325;
    for b in format!("{}:{}", exit, norm).bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Run a shell check, bound its result to the exact tree, keep the full log on disk.
pub fn run_check(sess: Option<&Session>, dir: &Path, cmd: &str, by: &str, timeout_s: u64) -> Value {
    let t0 = now_ms();
    let tree_before = tree_hash(dir);
    let (code, out) = sh_timeout(cmd, dir, timeout_s);
    let tree_after = tree_hash(dir);
    let fp = fingerprint(code, &out);
    let log = sess.map(|s| s.blob("check", &out).to_string_lossy().to_string());
    let ev = json!({
        "kind": "check", "observed": true, "by": by, "cmd": cmd, "exit": code,
        "tree": tree_before, "tree_changed_by_check": tree_after != tree_before,
        "fp": fp, "log": log, "ms": (now_ms() - t0) as u64, "t": t0 as u64,
        "tail": tail(&out, 40)
    });
    if let Some(s) = sess {
        s.evidence(ev.clone());
    }
    ev
}

/// Every live descendant of `pid` (children first found, deepest last), via `pgrep -P`.
/// Used instead of a separate process group so terminal Ctrl-C and Claude Code's own
/// kill of its shell still reach the command.
fn descendants(pid: u32) -> Vec<i32> {
    let mut all: Vec<i32> = Vec::new();
    let mut frontier = vec![pid as i32];
    while let Some(p) = frontier.pop() {
        let Ok(o) = Command::new("pgrep")
            .args(["-P", &p.to_string()])
            .stderr(Stdio::null())
            .output()
        else {
            break;
        };
        for c in String::from_utf8_lossy(&o.stdout)
            .split_whitespace()
            .filter_map(|s| s.parse::<i32>().ok())
        {
            if !all.contains(&c) {
                all.push(c);
                frontier.push(c);
            }
        }
    }
    all
}

/// Run `cmd` with `sh -c`, stdout and stderr merged, bounded by `timeout_s`.
/// On timeout the whole process tree is killed (exit 124). Output is returned even if a
/// background process keeps the pipe open past the command's exit (bounded by the timeout).
pub fn sh_timeout(cmd: &str, dir: &Path, timeout_s: u64) -> (i32, String) {
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::{Duration, Instant};
    // `exec 2>&1` first so stderr of EVERY command (and shell syntax errors) is captured;
    // a trailing `2>&1` would bind only to the last command of `a && b` or `a; b`.
    let child = Command::new("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{}", cmd))
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => return (127, e.to_string()),
    };
    let Some(mut so) = child.stdout.take() else {
        return (127, "no stdout".into());
    };
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    {
        let buf = buf.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match std::io::Read::read(&mut so, &mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
            let _ = done_tx.send(());
        });
    }
    let deadline = Instant::now() + Duration::from_secs(timeout_s.max(1));
    let code = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(-1),
            Ok(None) if Instant::now() > deadline => {
                // Killing only `sh` would leave its children holding the pipe (and running).
                let tree = descendants(child.id());
                let _ = child.kill();
                for p in tree {
                    // SAFETY: kill(2) on pids that were descendants of our child.
                    unsafe {
                        libc::kill(p, libc::SIGKILL);
                    }
                }
                let _ = child.wait();
                break 124;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => break -1,
        }
    };
    // Wait for EOF: until the deadline normally, a short grace after a kill.
    let wait = if code == 124 {
        Duration::from_secs(1)
    } else {
        deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(200))
    };
    let eof = done_rx.recv_timeout(wait).is_ok();
    let mut out = buf
        .lock()
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    if code == 124 {
        out.push_str(&format!("\n[qed: timed out after {}s]", timeout_s));
    } else if !eof {
        out.push_str(&format!(
            "\n[qed: a background process kept the output open; stopped reading after {}s]",
            timeout_s
        ));
    }
    (code, out)
}

/// Checks in the ledger, each marked valid only if its tree equals `cur_tree`.
pub fn check_status(ledger: &[Value], cur_tree: &str) -> Vec<(Value, bool)> {
    ledger
        .iter()
        .filter(|e| e["kind"] == "check")
        .map(|e| (e.clone(), e["tree"].as_str() == Some(cur_tree)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_root_matches_git_toplevel() {
        let base = std::env::temp_dir().join(format!("qed-root-{}", std::process::id()));
        let repo = base.join("repo");
        let sub = repo.join("a").join("b");
        let outside = base.join("plain");
        fs::create_dir_all(&sub).unwrap();
        fs::create_dir_all(&outside).unwrap();
        // Canonicalize like getcwd does on macOS (/var -> /private/var).
        let repo = repo.canonicalize().unwrap();
        let sub = sub.canonicalize().unwrap();
        let outside = outside.canonicalize().unwrap();
        git(&repo, &["init", "-q"]).unwrap();
        for d in [&repo, &sub] {
            let want = git(d, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
            assert_eq!(Some(project_root(d)), want, "from {}", d.display());
        }
        // Outside any repo (temp dirs are not inside a worktree): falls back to cwd.
        if git(&outside, &["rev-parse", "--show-toplevel"]).is_none() {
            assert_eq!(project_root(&outside), outside);
        }
        let _ = fs::remove_dir_all(&base);
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("qed-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    #[test]
    fn check_output_keeps_stderr_of_every_command() {
        let d = tmp("stderr");
        // The error of the FIRST command in a chain is what explains the failure.
        let (code, out) = sh_timeout("ls /nonexistent-qed-zz && true", &d, 10);
        assert_ne!(code, 0);
        assert!(out.contains("nonexistent-qed-zz"), "{:?}", out);
        let (_, out) = sh_timeout("ls /nonexistent-qed-yy; true", &d, 10);
        assert!(out.contains("nonexistent-qed-yy"), "{:?}", out);
        // A shell syntax error must be visible, not an empty failure.
        let (code, out) = sh_timeout("echo (", &d, 10);
        assert_ne!(code, 0);
        assert!(!out.trim().is_empty(), "syntax error is shown");
        let _ = fs::remove_dir_all(&d);
    }

    fn running(pattern: &str) -> bool {
        Command::new("pgrep")
            .args(["-f", pattern])
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    #[test]
    fn timeout_bounds_time_and_kills_the_whole_command() {
        let d = tmp("timeout");
        let t = std::time::Instant::now();
        // `cat` and `sleep` are children of sh: killing only sh leaves them holding the pipe.
        let (code, out) = sh_timeout("sleep 13.37 | cat", &d, 1);
        let took = t.elapsed().as_secs_f64();
        assert_eq!(code, 124, "{}", out);
        assert!(took < 5.0, "timeout of 1s took {:.1}s", took);
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!running("sleep 13.37"), "the whole command is killed");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn background_writer_does_not_outlive_the_timeout() {
        let d = tmp("bgwriter");
        let t = std::time::Instant::now();
        // sh exits at once, but a background process keeps the output pipe open.
        let (code, out) = sh_timeout("(sleep 9.87 &); echo hi", &d, 2);
        let took = t.elapsed().as_secs_f64();
        let _ = Command::new("pkill").args(["-f", "sleep 9.87"]).status();
        assert_eq!(code, 0);
        assert!(out.contains("hi"), "{:?}", out);
        assert!(took < 5.0, "bounded by the 2s timeout, took {:.1}s", took);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tree_hash_is_the_same_from_a_subdirectory() {
        let d = tmp("subtree");
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "a\n").unwrap();
        fs::write(d.join("sub/b.txt"), "b\n").unwrap();
        sh_timeout(
            "git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm b",
            &d,
            30,
        );
        // An edit OUTSIDE the subdirectory must change the hash seen from inside it.
        fs::write(d.join("a.txt"), "changed\n").unwrap();
        fs::write(d.join("new.txt"), "untracked\n").unwrap();
        assert_eq!(tree_hash(&d.join("sub")), tree_hash(&d));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tree_hash_keeps_a_stat_cache_for_untracked_files() {
        let d = tmp("statcache");
        fs::write(d.join("a.txt"), "a\n").unwrap();
        sh_timeout(
            "git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm b",
            &d,
            30,
        );
        fs::write(d.join("untracked.bin"), "data\n").unwrap();
        let h = tree_hash(&d);
        // The scratch index survives, so the next hash re-reads only files whose stat changed.
        let idx = git(&d, &["rev-parse", "--path-format=absolute", "--git-path", "qed-index"]).unwrap();
        let listed = git_env(&d, &["ls-files"], &[("GIT_INDEX_FILE", idx.as_str())]).unwrap_or_default();
        assert!(listed.lines().any(|l| l == "untracked.bin"), "{:?}", listed);
        assert_eq!(tree_hash(&d), h, "same tree, same hash");
        fs::write(d.join("untracked.bin"), "other\n").unwrap();
        assert_ne!(tree_hash(&d), h, "a changed untracked file changes the hash");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sessions_open_by_fragment_and_recap_last_exchange() {
        let root = std::env::temp_dir().join(format!("qed-sess-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let mut a = Session::create(&root, "primera tarea\nFollow-up: segunda").unwrap();
        // Old-style trace (no prompt events): reply is the text of the last finished turn.
        for ev in [
            json!({"ev": "text", "text": "uno"}),
            json!({"ev": "done"}),
            json!({"ev": "text", "text": "hecho"}),
            json!({"ev": "tool"}),
            json!({"ev": "text", "text": "listo"}),
            json!({"ev": "done"}),
        ] {
            a.event(&ev);
        }
        assert_eq!(a.recap(), ("segunda".into(), "hecho\nlisto".into()));
        a.event(&json!({"ev": "prompt", "text": "tercera"}));
        a.event(&json!({"ev": "text", "text": "a medias"}));
        assert_eq!(a.recap(), ("tercera".into(), "a medias".into()));
        assert_eq!(a.title(), "primera tarea");
        let id = a.id();
        let frag = &id[id.len() - 4..];
        assert_eq!(
            Session::open(&root, Some(frag)).map(|s| s.id()),
            Some(id.clone())
        );
        assert!(Session::open(&root, Some("zzzz")).is_none());
        assert_eq!(Session::recent(&root, 5).len(), 1);
        let _ = fs::remove_dir_all(&root);
    }
}
