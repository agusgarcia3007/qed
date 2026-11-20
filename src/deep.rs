//! Deep mode: k independent candidates of the same model in parallel worktrees, then
//! selection by cross-execution. Each candidate's source fix is tested against every
//! candidate's reproduction checks (their test files transplanted onto it); the fix that
//! satisfies the most checks wins. Behavior decides, not the most convincing explanation.
//! Evidence: Agentless (repro-test filtering), CodeMonkeys, OpenHands best-of-k.

use crate::engine::{Emitter, Out};
use crate::run::{self, Cfg, Mode, Outcome};
use crate::store::{self, git, git_raw, run_check, tree_hash, Session};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const MAX_CHECKS_PER_CANDIDATE: usize = 3;

/// Heuristic split of a change into test files and source files.
pub fn is_test_path(p: &str) -> bool {
    let p = p.to_ascii_lowercase();
    let name = p.rsplit('/').next().unwrap_or(&p);
    p.starts_with("test/")
        || p.starts_with("tests/")
        || p.starts_with("testing/")
        || p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/testing/")
        || p.contains("/__tests__/")
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.contains(".test.")
        || name.contains(".spec.")
}

struct Cand {
    out: Outcome,
    tree: String,
    src: Vec<String>,
    tests: Vec<String>,
    checks: Vec<String>,
}

fn worktree(
    root: &Path,
    base_ref: &str,
    name: &str,
    setup: Option<&str>,
    timeout: u64,
) -> Option<PathBuf> {
    let wt = root.join(".qed").join("wt").join(name);
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            &wt.to_string_lossy(),
            base_ref,
        ],
    )?;
    if let Some(s) = setup {
        let _ = store::sh_timeout(s, &wt, timeout);
    }
    Some(wt)
}

fn files(dir: &Path, base: &str, tree: &str) -> Vec<String> {
    git(dir, &["diff", "--name-only", base, tree])
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
}

fn patch(dir: &Path, base: &str, tree: &str, paths: &[String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut args = vec!["diff", "--binary", base, tree, "--"];
    args.extend(paths.iter().map(String::as_str));
    git_raw(dir, &args).unwrap_or_default()
}

fn apply(dir: &Path, p: &str) -> bool {
    p.is_empty() || run::apply_patch(dir, p).0 == 0
}

pub fn deep(cfg: &Cfg, k: usize, prompt: &str, em: &mut Emitter) -> Outcome {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(cfg.budget_s);
    let mut out = Outcome {
        final_branch: "main".into(),
        ..Default::default()
    };
    let Some((root, base_ref, base_tree, sid)) = em.sess.as_deref().and_then(|s| {
        Some((
            s.root.clone(),
            s.meta["base_ref"].as_str()?.to_string(),
            s.meta["base_tree"].as_str()?.to_string(),
            s.id(),
        ))
    }) else {
        em.emit(json!({"ev": "info", "msg": "deep mode needs git: running a single candidate"}));
        return run::task(cfg, Mode::Min, prompt, None, em);
    };

    // 1. k candidates in parallel, each a normal min-mode task in its own worktree.
    let mut single = cfg.clone();
    single.candidates = 1;
    single.live = false;
    single.budget_usd = cfg.budget_usd / k as f64;
    let mut handles = Vec::new();
    for i in 0..k {
        let Some(wt) = worktree(
            &root,
            &base_ref,
            &format!("{}-c{}", sid, i),
            cfg.setup.as_deref(),
            cfg.check_timeout_s,
        ) else {
            em.emit(json!({"ev": "error", "msg": format!("could not create worktree for candidate {}", i)}));
            continue;
        };
        // Selection needs each candidate's reproduction to survive in the final tree.
        let (c, p) = (
            single.clone(),
            format!("{}\n\nKeep your reproduction as a regular test file in the project's test suite (do not delete it), and run it with `qed check`.", prompt),
        );
        handles.push(std::thread::spawn(move || {
            let o = match Session::create(&wt, &p) {
                Ok(mut s) => {
                    let mut e = Emitter::new(Out::Quiet, Some(&mut s));
                    let o = run::task(&c, Mode::Min, &p, None, &mut e);
                    (o, Some(s.dir.clone()))
                }
                Err(_) => (Outcome::default(), None),
            };
            (wt, o)
        }));
    }
    em.emit(json!({"ev": "info", "msg": format!("deep: {} candidates in parallel (effort {})", handles.len(), cfg.effort)}));
    em.status(&format!("{} candidates working", handles.len()));
    while handles.iter().any(|h| !h.is_finished()) {
        em.tick();
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut cands = Vec::new();
    for h in handles {
        let Ok((wt, (o, sdir))) = h.join() else {
            continue;
        };
        let tree = tree_hash(&wt);
        let changed = files(&root, &base_tree, &tree);
        let (tests, src): (Vec<String>, Vec<String>) =
            changed.into_iter().partition(|p| is_test_path(p));
        let ledger = sdir
            .map(|d| store::read_jsonl(&d.join("evidence.jsonl")))
            .unwrap_or_default();
        let mut checks: Vec<String> = Vec::new();
        for e in ledger
            .iter()
            .rev()
            .filter(|e| e["kind"] == "check" && e["by"] == "model")
        {
            if let Some(c) = e["cmd"].as_str() {
                if !checks.iter().any(|x| x == c) && checks.len() < MAX_CHECKS_PER_CANDIDATE {
                    checks.push(c.to_string());
                }
            }
        }
        out.cost += o.cost;
        out.calls += o.calls;
        out.ttft_ms = out.ttft_ms.or(o.ttft_ms);
        out.cancelled |= o.cancelled;
        em.emit(json!({"ev": "candidate", "i": cands.len(), "src_files": src.len(), "test_files": tests.len(),
            "checks": checks, "cost": o.cost, "passed_own": o.passed}));
        let _ = wt;
        cands.push(Cand {
            out: o,
            tree,
            src,
            tests,
            checks,
        });
    }
    if out.cancelled || cands.is_empty() {
        out.wall_ms = start.elapsed().as_millis() as u64;
        return out;
    }

    // 2. Cross-execution. A check only counts if it discriminates (Agentless rule): it must
    // FAIL on the original code (with its test files transplanted) and pass on some candidate.
    // Broad suites with pre-existing/env failures and deleted repro files drop out on their own.
    em.status("cross-checking candidates");
    let n = cands.len();
    let mut score = vec![0usize; n];
    let mut informative = 0usize;
    let scratch = worktree(
        &root,
        &base_ref,
        &format!("{}-select", sid),
        cfg.setup.as_deref(),
        cfg.check_timeout_s,
    );
    let srcs: Vec<String> = cands
        .iter()
        .map(|c| patch(&root, &base_tree, &c.tree, &c.src))
        .collect();
    let mut seen: Vec<String> = Vec::new();
    'checks: for (j, cj) in cands.iter().enumerate() {
        let test_j = patch(&root, &base_tree, &cj.tree, &cj.tests);
        for c in &cj.checks {
            if seen.contains(c) {
                continue;
            }
            seen.push(c.clone());
            let Some(sc) = scratch.as_deref() else {
                break 'checks;
            };
            // Run `c` on base + test_j + (optional) a candidate's source fix.
            let run = |fix: Option<usize>, em: &mut Emitter| -> Option<bool> {
                if Instant::now() > deadline {
                    return None;
                }
                // reset --hard also drops `git add -N` entries left by .qenv, then clean untracked.
                let _ = store::sh_timeout("git reset -q --hard HEAD && git clean -qfd", sc, 60);
                if !apply(sc, &test_j) || !fix.is_none_or(|i| apply(sc, &srcs[i])) {
                    return Some(false);
                }
                let left = deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs()
                    .max(1);
                let r = run_check(None, sc, c, "select", cfg.check_timeout_s.min(left));
                em.emit(
                    json!({"ev": "select", "owner": j, "fix": fix, "cmd": c, "exit": r["exit"],
                    "tail": store::tail(r["tail"].as_str().unwrap_or(""), 12)}),
                );
                Some(r["exit"] == 0)
            };
            match run(None, em) {
                None => break 'checks,
                Some(true) => continue, // passes without any fix: doesn't reproduce the bug
                Some(false) => {}
            }
            let passes: Vec<bool> = (0..n)
                .map(|i| !cands[i].src.is_empty() && run(Some(i), em) == Some(true))
                .collect();
            if passes.iter().any(|&p| p) {
                informative += 1;
                for (i, p) in passes.iter().enumerate() {
                    score[i] += *p as usize;
                }
            }
        }
    }
    if let Some(sc) = &scratch {
        let _ = git(
            &root,
            &["worktree", "remove", "--force", &sc.to_string_lossy()],
        );
    }

    // 3. Pick: most discriminating checks passed; ties -> smaller source change. Empty fixes lose.
    let win = (0..n)
        .min_by_key(|&i| {
            (
                cands[i].src.is_empty(),
                std::cmp::Reverse(score[i]),
                cands[i].src.len(),
            )
        })
        .unwrap_or(0);
    let table: Vec<String> = (0..n)
        .map(|i| format!("c{} {}/{}", i, score[i], informative))
        .collect();
    em.emit(json!({"ev": "decision", "msg": format!("discriminating checks passed: {} → c{}", table.join(" · "), win)}));

    let full = git_raw(
        &root,
        &["diff", "--binary", &tree_hash(&root), &cands[win].tree],
    )
    .unwrap_or_default();
    if !full.is_empty() && !apply(&root, &full) {
        em.emit(json!({"ev": "error", "msg": "could not apply the winning candidate"}));
    } else {
        out.final_branch = format!("c{}", win);
    }
    out.passed = cands[win]
        .out
        .passed
        .or(Some(informative > 0 && score[win] == informative));
    out.wall_ms = start.elapsed().as_millis() as u64;
    em.emit(json!({
        "ev": "done", "passed": out.passed, "branch": out.final_branch, "cost": out.cost, "calls": out.calls,
        "wall_ms": out.wall_ms, "escalations": 0, "cancelled": out.cancelled, "candidates": n
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_tests_from_source() {
        for p in [
            "tests/test_x.py",
            "pkg/tests/a.py",
            "testing/x.py",
            "src/foo_test.go",
            "web/a.test.ts",
            "lib/test_util.py",
        ] {
            assert!(is_test_path(p), "{}", p);
        }
        for p in [
            "src/main.rs",
            "django/db/models/deletion.py",
            "contest/x.py",
            "attest.py",
        ] {
            assert!(!is_test_path(p), "{}", p);
        }
    }
}
