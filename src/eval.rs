//! Reproducible A/B/C/D evaluator. Each run: fresh copy of the task repo in a temp dir,
//! one variant under a fixed budget, one final diff, hidden tests applied only afterwards.

use crate::engine::{Emitter, Out};
use crate::run::{self, Cfg, Mode};
use crate::store::{self, git, now_ms, sh_timeout, Session};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

const VARIANTS: &str = "A,B,C,D";

struct Task {
    dir: PathBuf,
    v: Value,
}

impl Task {
    fn s(&self, k: &str) -> Option<String> {
        self.v[k].as_str().map(String::from)
    }
    fn id(&self) -> String {
        self.s("id").unwrap_or_else(|| {
            self.dir
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default()
        })
    }
}

fn load_tasks(dir: &Path) -> Vec<Task> {
    let mut ts: Vec<Task> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let v: Value =
                serde_json::from_str(&fs::read_to_string(p.join("task.json")).ok()?).ok()?;
            Some(Task { dir: p, v })
        })
        .collect();
    ts.sort_by_key(|t| t.id());
    ts
}

fn cp(src: &Path, dst: &Path) -> bool {
    sh_timeout(
        &format!("cp -R '{}/.' '{}'", src.display(), dst.display()),
        dst,
        120,
    )
    .0 == 0
}

/// Fresh git workspace with a deterministic base commit.
fn workspace(t: &Task, root: &Path, tag: &str) -> Option<(PathBuf, String)> {
    let ws = root.join(tag);
    fs::create_dir_all(&ws).ok()?;
    // Real-repo tasks (e.g. SWE-bench): shared clone from a local cache at the base commit.
    if let (Some(src), Some(commit)) = (t.s("repo_git"), t.s("commit")) {
        let cmd = format!(
            "git clone -q --shared '{}' . && git checkout -q --detach {}",
            src, commit
        );
        if sh_timeout(&cmd, &ws, 600).0 != 0 {
            return None;
        }
        if let Some(s) = t.s("setup") {
            sh_timeout(&s, &ws, 900);
        }
        return Some((ws, commit));
    }
    if !cp(&t.dir.join("repo"), &ws) {
        return None;
    }
    let env = "GIT_AUTHOR_NAME=eval GIT_AUTHOR_EMAIL=eval@localhost GIT_COMMITTER_NAME=eval GIT_COMMITTER_EMAIL=eval@localhost GIT_AUTHOR_DATE=2026-01-01T00:00:00Z GIT_COMMITTER_DATE=2026-01-01T00:00:00Z";
    let (c, _) = sh_timeout(
        &format!("git init -q && git add -A && {} git commit -qm base", env),
        &ws,
        60,
    );
    if c != 0 {
        return None;
    }
    if let Some(s) = t.s("setup") {
        sh_timeout(&s, &ws, 900);
    }
    let base = git(&ws, &["rev-parse", "HEAD"])?;
    Some((ws, base))
}

fn reset(ws: &Path) {
    sh_timeout("git checkout -q -- . && git clean -qfd", ws, 60);
}

fn cfg_for(t: &Task, base: &Cfg, variant: &str) -> (Cfg, Mode) {
    let mut c = base.clone();
    // "NAME@effort" overrides the effort; "DEEPk" = min mode with k parallel candidates.
    let (variant, effort) = variant
        .split_once('@')
        .map(|(v, e)| (v, Some(e)))
        .unwrap_or((variant, None));
    if let Some(e) = effort {
        c.effort = e.to_string();
    }
    if let Some(k) = variant
        .strip_prefix("DEEP")
        .and_then(|k| k.parse::<usize>().ok())
    {
        c.candidates = k;
    }
    if let Some(x) = t.v["budget_s"].as_u64() {
        c.budget_s = x;
    }
    if let Some(x) = t.v["budget_usd"].as_f64() {
        c.budget_usd = x;
    }
    c.check = t.s("check");
    c.setup = t.s("setup");
    if t.v["web"] == false {
        c.web = false;
        c.tools = c
            .tools
            .split(',')
            .filter(|x| !x.starts_with("Web"))
            .collect::<Vec<_>>()
            .join(",");
    }
    let mode = match variant {
        "A" | "B" | "P" => Mode::Native,
        "C" => Mode::Min,
        v if v.starts_with("DEEP") => Mode::Min,
        _ => Mode::Adaptive,
    };
    if variant == "P" {
        c.protocol_only = true;
    }
    if variant == "D-nocritic" {
        c.critic = false;
    }
    if variant == "D-noalt" {
        c.alt = false;
    }
    (c, mode)
}

/// Same information for every variant: the task text plus the visible check, if any.
fn prompt_for(t: &Task) -> String {
    let mut p = t.s("prompt").unwrap_or_default();
    if let Some(c) = t.s("check") {
        p.push_str(&format!("\n\nVisible check you can run: `{}`", c));
    }
    p
}

fn run_variant(t: &Task, base_cfg: &Cfg, variant: &str, ws: &Path) -> (run::Outcome, Vec<PathBuf>) {
    let (cfg, mode) = cfg_for(t, base_cfg, variant);
    let prompt = prompt_for(t);
    let mut traces = Vec::new();
    let Ok(mut sess) = Session::create(ws, &prompt) else {
        return (run::Outcome::default(), traces);
    };
    traces.push(sess.dir.clone());
    let mut o = {
        let mut em = Emitter::new(Out::Quiet, Some(&mut sess));
        run::task(&cfg, mode, &prompt, None, &mut em)
    };
    if variant == "B" {
        // One blind retry from scratch inside the same total budget, only if the visible check fails.
        let left_s = cfg.budget_s.saturating_sub(o.wall_ms / 1000);
        let left_usd = cfg.budget_usd - o.cost;
        let failing = cfg
            .check
            .as_deref()
            .is_some_and(|c| sh_timeout(c, ws, cfg.check_timeout_s).0 != 0);
        if failing && left_s > 30 && left_usd > 0.05 && !o.cancelled {
            reset(ws);
            let mut c2 = cfg.clone();
            c2.budget_s = left_s;
            c2.budget_usd = left_usd;
            if let Ok(mut s2) = Session::create(ws, &prompt) {
                traces.push(s2.dir.clone());
                let mut em = Emitter::new(Out::Quiet, Some(&mut s2));
                let o2 = run::task(&c2, Mode::Native, &prompt, None, &mut em);
                o.cost += o2.cost;
                o.calls += o2.calls;
                o.wall_ms += o2.wall_ms;
                o.escalations += 1;
            }
        }
    }
    (o, traces)
}

/// Flag runs whose trace touched the hidden tests or a forbidden URL (reference patch).
fn contaminated(t: &Task, traces: &[PathBuf]) -> Vec<String> {
    // Any reference to the task definition (hidden tests, scorer, gold data) contaminates the run.
    let mut needles = vec![
        t.dir.to_string_lossy().to_string(),
        "bench/tasks".into(),
        "bench/swebench".into(),
    ];
    for u in t.v["forbidden_urls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| u.as_str())
    {
        needles.push(u.into());
    }
    let mut hits = Vec::new();
    for tr in traces {
        for e in store::read_jsonl(&tr.join("events.jsonl"))
            .iter()
            .filter(|e| e["ev"] == "tool")
        {
            let inp = e["input"].to_string();
            for n in &needles {
                if inp.contains(n.as_str()) {
                    hits.push(n.clone());
                }
            }
        }
    }
    hits.sort();
    hits.dedup();
    hits
}

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[((v.len() as f64 - 1.0) * p).round() as usize]
}

pub fn main(args: &[String], cwd: &Path) -> i32 {
    let Some(tasks_dir) = args.first().map(|a| cwd.join(a)) else {
        eprintln!(
            "usage: qed eval TASKS [--variants A,B,C,D] [--repeat N] [--out DIR] [--only id,...]"
        );
        return 2;
    };
    let mut variants = VARIANTS.to_string();
    let mut repeat = 1usize;
    let mut out = cwd
        .join("bench")
        .join("runs")
        .join(format!("{}", now_ms() / 1000));
    let mut only: Option<String> = None;
    let mut i = 1;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--variants" => variants = v,
            "--repeat" => repeat = v.parse().unwrap_or(1),
            "--out" => out = cwd.join(v),
            "--only" => only = Some(v),
            _ => {
                eprintln!("unknown flag: {}", args[i]);
                return 2;
            }
        }
        i += 2;
    }
    let mut tasks = load_tasks(&tasks_dir);
    if let Some(o) = &only {
        tasks.retain(|t| o.split(',').any(|x| x == t.id()));
    }
    if tasks.is_empty() {
        eprintln!("no tasks in {}", tasks_dir.display());
        return 1;
    }
    let vs: Vec<&str> = variants.split(',').collect();
    let base_cfg = crate::load_cfg(cwd);
    let _ = fs::create_dir_all(&out);
    let claude_v = sh_timeout(
        &format!("{} --version", crate::engine::claude_bin()),
        cwd,
        30,
    )
    .1
    .trim()
    .to_string();
    let env = json!({
        "model": base_cfg.model, "effort": base_cfg.effort, "lean": base_cfg.lean, "claude": claude_v,
        "qed": git(cwd, &["rev-parse", "--short", "HEAD"]), "variants": vs, "repeat": repeat,
        "tasks": tasks.iter().map(|t| t.id()).collect::<Vec<_>>(), "started_ms": now_ms() as u64
    });
    let _ = fs::write(
        out.join("env.json"),
        serde_json::to_vec_pretty(&env).unwrap_or_default(),
    );
    eprintln!(
        "eval → {} ({} tasks × {} variants × {})",
        out.display(),
        tasks.len(),
        vs.len(),
        repeat
    );
    let scratch = std::env::temp_dir().join(format!("qed-eval-{}", now_ms()));
    let results = out.join("results.jsonl");

    for rep in 0..repeat {
        for (ti, t) in tasks.iter().enumerate() {
            // Interleave: rotate variant order per task and repetition to spread drift (rate limits, caching).
            let k = (rep + ti) % vs.len();
            let order: Vec<&str> = vs[k..].iter().chain(vs[..k].iter()).copied().collect();
            for variant in order {
                let tag = format!("{}-{}-r{}", t.id(), variant, rep);
                let Some((ws, base)) = workspace(t, &scratch, &tag) else {
                    eprintln!("  {} setup failed", tag);
                    continue;
                };
                let (o, traces) = run_variant(t, &base_cfg, variant, &ws);
                let rd = out.join(&tag);
                let _ = fs::create_dir_all(&rd);
                for (n, tr) in traces.iter().enumerate() {
                    let _ = fs::copy(
                        tr.join("events.jsonl"),
                        rd.join(format!("events{}.jsonl", n)),
                    );
                    let _ = fs::copy(
                        tr.join("evidence.jsonl"),
                        rd.join(format!("evidence{}.jsonl", n)),
                    );
                }
                // One final diff per run, then hidden tests outside the agent's reach.
                sh_timeout("git add -A -- . ':(exclude).qed'", &ws, 60);
                let diff = store::git_raw(&ws, &["diff", "--cached", "--binary", &base])
                    .unwrap_or_default();
                let _ = fs::write(rd.join("final.diff"), &diff);
                let visible = t.s("check").map(|c| sh_timeout(&c, &ws, 600).0 == 0);
                let hidden_dir = t.dir.join("hidden");
                let copied = !hidden_dir.exists() || cp(&hidden_dir, &ws);
                let hidden = if let (true, Some(cmd)) = (copied, t.s("hidden_cmd")) {
                    // External scorers (SWE-bench harness) get the final diff path.
                    let cmd = format!(
                        "QED_DIFF='{}' QED_TASK='{}' {}",
                        rd.join("final.diff").display(),
                        t.id(),
                        cmd
                    );
                    let (c, log) =
                        sh_timeout(&cmd, &ws, t.v["hidden_timeout_s"].as_u64().unwrap_or(900));
                    let _ = fs::write(rd.join("hidden.log"), &log);
                    c == 0
                } else {
                    false
                };
                let hits = contaminated(t, &traces);
                // Infra failures (API errors, rate limits) are not model failures: excluded from scoring.
                let mut infra = false;
                let mut quota = 0.0f64;
                let mut rejected = false;
                // Machine sleep pauses Docker and the monotonic clock but not the wall clock:
                // a large gap means the run's timing and checks are unreliable.
                let mut span_ms = 0u64;
                for tr in &traces {
                    let ts: Vec<u64> = store::read_jsonl(&tr.join("events.jsonl"))
                        .iter()
                        .filter_map(|e| e["t"].as_u64())
                        .collect();
                    if let (Some(a), Some(b)) = (ts.iter().min(), ts.iter().max()) {
                        span_ms += b - a;
                    }
                }
                let slept = span_ms > o.wall_ms + 120_000;
                infra |= slept;
                for tr in &traces {
                    for e in store::read_jsonl(&tr.join("events.jsonl")) {
                        match e["ev"].as_str().unwrap_or("") {
                            // Structured flags only (message text is localized): a timeout or a
                            // cancel is the variant's outcome; any other errored result is infra.
                            "result"
                                if e["is_error"] == true
                                    && e["timed_out"] != true
                                    && e["cancelled"] != true =>
                            {
                                infra = true
                            }
                            "quota" => {
                                quota = quota.max(e["utilization"].as_f64().unwrap_or(0.0));
                                rejected |= e["status"] == "rejected";
                            }
                            _ => {}
                        }
                    }
                }
                let r = json!({
                    "task": t.id(), "category": t.s("category"), "variant": variant, "rep": rep,
                    "solved": hidden && hits.is_empty(), "hidden_pass": hidden, "visible_pass": visible,
                    "cost": o.cost, "calls": o.calls, "wall_ms": o.wall_ms, "ttft_ms": o.ttft_ms,
                    "escalations": o.escalations, "branch": o.final_branch, "cancelled": o.cancelled,
                    "contaminated": hits, "diff_bytes": diff.len(), "infra_error": infra, "slept": slept, "quota": quota
                });
                store::append_jsonl(&results, &r);
                eprintln!(
                    "  {:<28} {} ${:.2} {:>5.0}s calls {} esc {}",
                    tag,
                    if r["solved"] == true { "✓" } else { "✗" },
                    o.cost,
                    o.wall_ms as f64 / 1000.0,
                    o.calls,
                    o.escalations
                );
                let _ = fs::remove_dir_all(&ws);
                if quota > 0.0 {
                    eprintln!(
                        "    quota used: {:.0}%{}",
                        quota * 100.0,
                        if infra {
                            " · INFRA ERROR (excluded)"
                        } else {
                            ""
                        }
                    );
                }
                if rejected || quota >= 0.98 {
                    eprintln!(
                        "quota exhausted: stopping the eval to avoid recording false failures"
                    );
                    let summary = summarize(&store::read_jsonl(&results), &vs);
                    let _ = fs::write(out.join("summary.md"), &summary);
                    println!("{}", summary);
                    return 3;
                }
            }
        }
    }
    let summary = summarize(&store::read_jsonl(&results), &vs);
    let _ = fs::write(out.join("summary.md"), &summary);
    println!("{}", summary);
    0
}

pub fn summarize(rs: &[Value], vs: &[&str]) -> String {
    let mut s = String::from("| variant | solved | short | bug | long | docs | swe | median s | p95 s | $ total | $/solved | calls | escal. | contam. |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for v in vs {
        let r: Vec<&Value> = rs
            .iter()
            .filter(|x| x["variant"] == *v && x["infra_error"] != true)
            .collect();
        let solved = r.iter().filter(|x| x["solved"] == true).count();
        let cat = |c: &str| {
            let rc: Vec<&&Value> = r.iter().filter(|x| x["category"] == c).collect();
            if rc.is_empty() {
                "-".to_string()
            } else {
                format!(
                    "{}/{}",
                    rc.iter().filter(|x| x["solved"] == true).count(),
                    rc.len()
                )
            }
        };
        let mut wall: Vec<u64> = r.iter().filter_map(|x| x["wall_ms"].as_u64()).collect();
        let cost: f64 = r.iter().filter_map(|x| x["cost"].as_f64()).sum();
        let calls: u64 = r.iter().filter_map(|x| x["calls"].as_u64()).sum();
        let esc: u64 = r.iter().filter_map(|x| x["escalations"].as_u64()).sum();
        let cont = r
            .iter()
            .filter(|x| x["contaminated"].as_array().is_some_and(|a| !a.is_empty()))
            .count();
        s.push_str(&format!(
            "| {} | {}/{} | {} | {} | {} | {} | {} | {:.0} | {:.0} | {:.2} | {} | {} | {} | {} |\n",
            v,
            solved,
            r.len(),
            cat("short"),
            cat("bug"),
            cat("long"),
            cat("docs"),
            cat("swe-hard"),
            pct(&mut wall.clone(), 0.5) as f64 / 1000.0,
            pct(&mut wall, 0.95) as f64 / 1000.0,
            cost,
            if solved > 0 {
                format!("{:.2}", cost / solved as f64)
            } else {
                "-".into()
            },
            calls,
            esc,
            cont
        ));
    }
    let infra = rs.iter().filter(|x| x["infra_error"] == true).count();
    if infra > 0 {
        s.push_str(&format!(
            "\n{} runs excluded due to infrastructure errors (API/quota).\n",
            infra
        ));
    }
    s.push_str("\n$/solved includes spend on failed runs. A small pilot is not enough to claim improvements.\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_diff_keeps_trailing_blank_context() {
        let d = std::env::temp_dir().join(format!("qed-diff-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("f.py"), "a\nb\n\n").unwrap();
        sh_timeout(
            "git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm b",
            &d,
            30,
        );
        fs::write(d.join("f.py"), "a\nB\n\n").unwrap();
        let diff = store::git_raw(&d, &["diff"]).unwrap();
        assert!(
            diff.ends_with("\n \n"),
            "keeps the blank context line: {:?}",
            diff
        );
        fs::write(d.join("p.diff"), &diff).unwrap();
        sh_timeout("git checkout -q -- f.py", &d, 30);
        assert_eq!(
            sh_timeout("git apply --check p.diff", &d, 30).0,
            0,
            "patch applies"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn summary_counts_failures_in_cost_per_solved() {
        let rs = vec![
            json!({"variant": "A", "category": "bug", "solved": true, "cost": 1.0, "wall_ms": 1000, "calls": 1, "escalations": 0, "contaminated": []}),
            json!({"variant": "A", "category": "bug", "solved": false, "cost": 3.0, "wall_ms": 3000, "calls": 1, "escalations": 0, "contaminated": []}),
        ];
        let s = summarize(&rs, &["A"]);
        assert!(s.contains("| A | 1/2 | - | 1/2 |"), "{}", s);
        assert!(s.contains("| 4.00 | 4.00 |"), "{}", s);
    }
}
