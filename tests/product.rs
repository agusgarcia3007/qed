//! Product behavior tests against a deterministic fake `claude` (tests/fake_claude.py).
//! They validate the harness (events, evidence, escalation, worktrees, cancel/resume), not the model.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const BAD: &str = "def add(a, b):\n    return a - b\n";
const TEST: &str =
    "from calc import add\nassert add(2, 3) == 5, f'add(2,3)={add(2,3)}'\nprint('ok')\n";

fn repo(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("qed-test-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_file(log_path(&d));
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("calc.py"), BAD).unwrap();
    fs::write(d.join("test.py"), TEST).unwrap();
    fs::write(d.join(".gitignore"), "__pycache__/\n").unwrap();
    let ok = Command::new("sh")
        .arg("-c")
        .arg("git init -q && git add -A && git -c user.name=t -c user.email=t@t commit -qm base")
        .current_dir(&d)
        .status()
        .unwrap()
        .success();
    assert!(ok);
    d
}

fn fake() -> String {
    format!("{}/tests/fake_claude.py", env!("CARGO_MANIFEST_DIR"))
}

fn qed(dir: &Path, mode: &str, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_qed"));
    c.args(args)
        .current_dir(dir)
        .env("QED_CLAUDE", fake())
        .env("FAKE_MODE", mode)
        .env("FAKE_LOG", log_path(dir))
        // Running the suite from inside a qed session must not record into that session.
        .env_remove("QED_SESSION")
        // BAD and GOOD calc.py have equal size; a .pyc from the same second would mask the fix.
        .env("PYTHONDONTWRITEBYTECODE", "1");
    for (k, v) in extra {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn events(o: &Output) -> Vec<Value> {
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn count(evs: &[Value], ev: &str) -> usize {
    evs.iter().filter(|e| e["ev"] == ev).count()
}

// Outside the repo so it never changes the tree under test.
fn log_path(dir: &Path) -> PathBuf {
    dir.with_extension("fakelog")
}

fn fake_log(dir: &Path) -> Vec<Value> {
    fs::read_to_string(log_path(dir))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn last_session(dir: &Path) -> PathBuf {
    let id = fs::read_to_string(dir.join(".qed/sessions/LAST")).unwrap();
    dir.join(".qed/sessions").join(id.trim())
}

fn ledger(dir: &Path) -> Vec<Value> {
    fs::read_to_string(last_session(dir).join("evidence.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[test]
fn short_task_takes_the_short_path() {
    let d = repo("short");
    let o = qed(&d, "short", &["-p", "decí listo"], &[]);
    let evs = events(&o);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(count(&evs, "start"), 1, "una sola llamada al modelo");
    assert_eq!(count(&evs, "critic"), 0, "sin subagente");
    let done = evs.iter().find(|e| e["ev"] == "done").unwrap();
    assert_eq!(done["calls"], 1);
    assert_eq!(done["escalations"], 0);
    // Default mode (min) verifies only configured checks: no adoption, no extra turns.
    assert!(!evs
        .iter()
        .any(|e| e["ev"] == "check" && e["by"] == "harness"));
    // The model's own `qed check` was recorded into the session through QED_SESSION.
    assert!(ledger(&d)
        .iter()
        .any(|e| e["by"] == "model" && e["cmd"] == "true"));
}

#[test]
fn bug_check_fails_before_and_passes_after() {
    let d = repo("bug");
    let o = qed(
        &d,
        "bug",
        &["-p", "arreglá add", "--check", "python3 test.py"],
        &[],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
    let l = ledger(&d);
    let model: Vec<&Value> = l.iter().filter(|e| e["by"] == "model").collect();
    assert_eq!(model[0]["exit"], 1, "repro falla en el original");
    assert_eq!(model[1]["exit"], 0, "pasa después del fix");
    assert_ne!(
        model[0]["tree"], model[1]["tree"],
        "cada check atado a su estado"
    );
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(last_session(&d).join("meta.json")).unwrap())
            .unwrap();
    assert_eq!(
        model[0]["tree"], meta["base_tree"],
        "la falla corrió sobre el código original"
    );
    // The model's passing run on the final tree is reused as the harness verification.
    let evs = events(&o);
    assert!(
        evs.iter()
            .any(|e| e["ev"] == "check" && e["reused"] == true && e["exit"] == 0),
        "verificación reusada"
    );
    assert!(
        !l.iter().any(|e| e["by"] == "harness"),
        "no se volvió a correr"
    );
    assert_eq!(count(&evs, "critic"), 0);
}

#[test]
fn repeated_failure_triggers_fresh_readonly_review() {
    let d = repo("stuck");
    let o = qed(
        &d,
        "stuck",
        &[
            "-p",
            "arreglá add",
            "--check",
            "python3 test.py",
            "--mode",
            "adaptive",
        ],
        &[("FAKE_INVALIDATE", "0")],
    );
    let evs = events(&o);
    assert!(count(&evs, "critic") >= 1, "el bloqueo activa la revisión");
    let calls = fake_log(&d);
    let c = calls
        .iter()
        .find(|c| c["critic"] == true)
        .expect("hubo llamada al crítico");
    let argv: Vec<String> = c["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let tools = &argv[argv.iter().position(|a| a == "--tools").unwrap() + 1];
    assert!(
        !tools.contains("Edit") && !tools.contains("Bash") && !tools.contains("Write"),
        "crítico solo lectura: {}",
        tools
    );
    assert!(!argv.contains(&"--resume".to_string()), "contexto fresco");
    let p = c["prompt"].as_str().unwrap();
    assert!(
        p.contains("```diff") && !p.contains("intenté algo"),
        "recibe diff y evidencia, no la defensa del autor"
    );
    // Not invalidated -> feedback goes back to the author, no alternative branch.
    assert_eq!(count(&evs, "alt"), 0);
    assert!(
        ledger(&d)
            .iter()
            .any(|e| e["kind"] == "critic" && e["observed"] == true),
        "contraejemplo reproducido por el harness"
    );
}

#[test]
fn alternative_branch_is_isolated_and_compared() {
    let d = repo("alt");
    let o = qed(
        &d,
        "stuck",
        &[
            "-p",
            "arreglá add",
            "--check",
            "python3 test.py",
            "--mode",
            "adaptive",
        ],
        &[("FAKE_INVALIDATE", "1")],
    );
    let evs = events(&o);
    assert_eq!(
        count(&evs, "alt"),
        1,
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let wt_call = fake_log(&d)
        .into_iter()
        .find(|c| c["cwd"].as_str().unwrap().ends_with("-alt"))
        .expect("corrió en worktree");
    assert!(wt_call["prompt"]
        .as_str()
        .unwrap()
        .contains("DIFFERENT hypothesis"));
    let done = evs.iter().find(|e| e["ev"] == "done").unwrap();
    assert_eq!(
        done["branch"], "alt",
        "gana la rama que pasa los mismos checks"
    );
    assert_eq!(done["passed"], true);
    assert!(fs::read_to_string(d.join("calc.py"))
        .unwrap()
        .contains("a + b"));
    // The original attempt is kept, not overwritten.
    let sid = last_session(&d)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let keep = Command::new("git")
        .args(["show", &format!("refs/qed/{}/main-before-alt:calc.py", sid)])
        .current_dir(&d)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&keep.stdout).contains("# author"),
        "intento original preservado"
    );
    // The worktree is a separate checkout from the original base.
    let wt = d.join(".qed/wt").join(format!("{}-alt", sid));
    assert!(wt.join("calc.py").exists());
}

#[test]
fn later_change_invalidates_earlier_check() {
    let d = repo("stale");
    qed(&d, "short", &["-p", "x"], &[]);
    let st =
        |d: &Path| String::from_utf8_lossy(&qed(d, "short", &["status"], &[]).stdout).to_string();
    assert!(st(&d).contains("\x1b[32mvalid"));
    fs::write(d.join("calc.py"), "# changed\n").unwrap();
    let s = st(&d);
    assert!(s.contains("stale") && !s.contains("\x1b[32mvalid"), "{}", s);
}

#[test]
fn cancel_then_resume_native_session() {
    let d = repo("cancel");
    let c = Command::new(env!("CARGO_BIN_EXE_qed"))
        .args(["-p", "tarea larga"])
        .current_dir(&d)
        .env("QED_CLAUDE", fake())
        .env("FAKE_MODE", "slow")
        .env("FAKE_LOG", log_path(&d))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    // SAFETY: signalling a child we spawned.
    unsafe {
        libc::kill(c.id() as i32, libc::SIGINT);
    }
    let o = c.wait_with_output().unwrap();
    let evs = events(&o);
    assert_eq!(
        count(&evs, "cancelled"),
        1,
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let meta: Value =
        serde_json::from_str(&fs::read_to_string(last_session(&d).join("meta.json")).unwrap())
            .unwrap();
    assert_eq!(meta["status"], "interrupted");
    let sid = meta["claude_session"].as_str().unwrap().to_string();

    let o2 = qed(&d, "short", &["resume", "-p", "seguí"], &[]);
    assert!(o2.status.success());
    let last = fake_log(&d).pop().unwrap();
    let argv: Vec<&str> = last["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let i = argv
        .iter()
        .position(|a| *a == "--resume")
        .expect("reanuda la sesión nativa");
    assert_eq!(argv[i + 1], sid);
}

#[test]
fn timings_are_measured_separately() {
    let d = repo("ttft");
    let evs = events(&qed(&d, "ttft", &["-p", "hola"], &[]));
    let r = evs.iter().find(|e| e["ev"] == "result").unwrap();
    let (sp, fe, tt, tot) = (
        r["spawn_ms"].as_u64().unwrap(),
        r["first_event_ms"].as_u64().unwrap(),
        r["ttft_ms"].as_u64().unwrap(),
        r["total_ms"].as_u64().unwrap(),
    );
    assert!(
        sp <= fe && fe <= tt && tt <= tot,
        "{} {} {} {}",
        sp,
        fe,
        tt,
        tot
    );
    assert!(
        tt >= fe + 250,
        "un evento inmediato no es TTFT: first_event {} ttft {}",
        fe,
        tt
    );
}

#[test]
fn web_tools_default_on_and_removable() {
    let d = repo("web");
    qed(&d, "short", &["-p", "x"], &[]);
    qed(&d, "short", &["-p", "x", "--no-web"], &[]);
    let log = fake_log(&d);
    let tools = |c: &Value| {
        let a: Vec<&str> = c["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a.as_str().unwrap())
            .collect();
        a[a.iter().position(|x| *x == "--tools").unwrap() + 1].to_string()
    };
    assert!(tools(&log[0]).contains("WebSearch") && tools(&log[0]).contains("WebFetch"));
    assert!(!tools(&log[1]).contains("Web"));
    let a0 = log[0]["argv"].to_string();
    assert!(
        a0.contains("--strict-mcp-config"),
        "MCP del usuario no se carga al inicio"
    );
}

fn argv_after(c: &Value, flag: &str) -> Option<String> {
    let a: Vec<&str> = c["argv"].as_array()?.iter().filter_map(|a| a.as_str()).collect();
    a.get(a.iter().position(|x| *x == flag)? + 1).map(|s| s.to_string())
}

#[test]
fn subagents_are_opt_in_and_their_checks_count() {
    let d = repo("subagents");
    qed(&d, "short", &["-p", "x"], &[]);
    let o = qed(&d, "subagent", &["-p", "x", "--subagents"], &[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let log = fake_log(&d);
    // Off by default: the Agent tool costs ~2.3k prompt tokens per request.
    assert!(!argv_after(&log[0], "--tools").unwrap().contains("Agent"));
    assert!(!argv_after(&log[0], "--append-system-prompt").unwrap().contains("Subagents"));
    // On: the tool is offered and allowed, and the protocol says when it pays off.
    assert!(argv_after(&log[1], "--tools").unwrap().split(',').any(|t| t == "Agent"));
    assert!(argv_after(&log[1], "--allowedTools").unwrap().split(',').any(|t| t == "Agent"));
    assert!(argv_after(&log[1], "--append-system-prompt").unwrap().contains("Subagents"));
    // Inline, not backgrounded: the report must arrive inside the same turn.
    assert!(log[0]["bg_off"].is_null());
    assert_eq!(log[1]["bg_off"], "1");
    // The subagent's voice stays out of the main stream; its tool calls too.
    let evs = events(&o);
    assert!(!evs.iter().any(|e| e["ev"] == "text" && e["text"] == "SUBVOICE"));
    assert_eq!(evs.iter().filter(|e| e["ev"] == "tool").count(), 1);
    // A check run by the subagent is recorded into the same session, bound to the tree.
    assert!(ledger(&d)
        .iter()
        .any(|e| e["by"] == "model" && e["cmd"] == "python3 test.py" && e["exit"] != 0));
    // The config key works too.
    fs::create_dir_all(d.join(".qed")).unwrap();
    fs::write(d.join(".qed/config.json"), r#"{"subagents": true}"#).unwrap();
    qed(&d, "short", &["-p", "x"], &[]);
    assert!(argv_after(fake_log(&d).last().unwrap(), "--tools").unwrap().contains("Agent"));
}

#[test]
fn interactive_repl_in_a_real_pty() {
    let d = repo("pty");
    let o = Command::new("python3")
        .arg(format!("{}/tests/pty_check.py", env!("CARGO_MANIFEST_DIR")))
        .args([env!("CARGO_BIN_EXE_qed"), &fake(), d.to_str().unwrap()])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", out);
}

#[test]
fn human_output_renders_markdown_and_stays_quiet() {
    let d = repo("md");
    let o = qed(&d, "md", &["explicá"], &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", out);
    // Marks split across stream deltas are still rendered, never shown raw.
    assert!(out.contains("\x1b[1mPlan"), "{}", out);
    assert!(
        out.contains("•") && !out.contains("**") && !out.contains("- usa"),
        "{}",
        out
    );
    assert!(out.contains("x = 1"), "code block kept: {}", out);
    // Per-call token/ttft line is gone; one closing line instead.
    assert!(!out.contains("ttft") && out.contains("∎"), "{}", out);
}

#[test]
fn adopted_check_seen_failing_is_not_refed() {
    let d = repo("seen");
    let evs = events(&qed(
        &d,
        "seen_fail",
        &["-p", "x", "--mode", "adaptive"],
        &[],
    ));
    assert_eq!(
        count(&evs, "start"),
        1,
        "sin información nueva no hay otra llamada"
    );
    assert!(evs
        .iter()
        .any(|e| e["ev"] == "info" && e["msg"].as_str().unwrap_or("").contains("already saw")));
}

#[test]
fn adopted_check_stale_after_edit_is_rerun_and_fed_back_once() {
    let d = repo("stale-ev");
    let evs = events(&qed(&d, "stale", &["-p", "x", "--mode", "adaptive"], &[]));
    assert!(
        evs.iter()
            .any(|e| e["ev"] == "check" && e["by"] == "harness" && e["exit"] != 0),
        "re-corre la evidencia vencida"
    );
    assert_eq!(
        count(&evs, "start"),
        2,
        "devuelve el fallo nuevo una vez y después para"
    );
}

#[test]
fn adaptive_adopts_and_reuses_the_models_check() {
    let d = repo("adopt");
    let evs = events(&qed(&d, "short", &["-p", "x", "--mode", "adaptive"], &[]));
    assert!(
        evs.iter()
            .any(|e| e["ev"] == "check" && e["reused"] == true && e["cmd"] == "true"),
        "adopta y reusa el check del modelo"
    );
    assert_eq!(count(&evs, "start"), 1);
}

#[test]
fn deep_mode_selects_the_candidate_that_passes_everyones_checks() {
    let d = repo("deep");
    let o = qed(&d, "deep", &["-p", "arreglá add", "--candidates", "3"], &[]);
    let evs = events(&o);
    assert_eq!(
        count(&evs, "candidate"),
        3,
        "{}",
        String::from_utf8_lossy(&o.stdout)
    );
    let dec = evs
        .iter()
        .find(|e| e["ev"] == "decision")
        .expect("decisión");
    assert!(
        dec["msg"].as_str().unwrap().ends_with("→ c1"),
        "{}",
        dec["msg"]
    );
    let done = evs.iter().find(|e| e["ev"] == "done").unwrap();
    assert_eq!(done["branch"], "c1");
    // The winner (fix + its own test) lands in the original repo; losers stay in their worktrees.
    assert!(fs::read_to_string(d.join("calc.py"))
        .unwrap()
        .contains("a + b"));
    assert!(d.join("tests/test_add_c1.py").exists());
    assert!(!d.join("tests/test_add_c0.py").exists());
}

#[test]
fn check_keeps_the_quoting_of_its_arguments() {
    let d = repo("quote");
    // The outer shell already split the words: `print('a b')` is ONE argument.
    let o = qed(&d, "short", &["check", "--", "python3", "-c", "print('a b')"], &[]);
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", out);
    assert!(out.lines().any(|l| l == "a b"), "{}", out);
    // Plain words stay as typed, so the harness can still match the configured check.
    qed(&d, "short", &["-p", "x"], &[]);
    qed(&d, "short", &["check", "--", "python3", "test.py"], &[]);
    assert!(ledger(&d).iter().any(|e| e["cmd"] == "python3 test.py"));
}

#[test]
fn deep_candidates_in_the_repl_use_their_own_processes() {
    let d = repo("deeprepl");
    // No prompt argument: the REPL (which keeps one live process for the main conversation).
    let mut c = Command::new(env!("CARGO_BIN_EXE_qed"));
    c.args(["--json", "--candidates", "3"])
        .current_dir(&d)
        .env("QED_CLAUDE", fake())
        .env("FAKE_MODE", "deep")
        .env("FAKE_LOG", log_path(&d))
        .env_remove("QED_SESSION")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = c.spawn().unwrap();
    {
        use std::io::Write;
        let mut si = child.stdin.take().unwrap();
        writeln!(si, "arreglá add").unwrap();
    }
    let o = child.wait_with_output().unwrap();
    let evs = events(&o);
    assert_eq!(count(&evs, "candidate"), 3, "{}", String::from_utf8_lossy(&o.stdout));
    // Parallel candidates must not share the REPL's single live-process slot.
    let cands: Vec<Value> = fake_log(&d)
        .into_iter()
        .filter(|c| c["cwd"].as_str().unwrap_or("").contains("/.qed/wt/"))
        .collect();
    assert_eq!(cands.len(), 3);
    for c in &cands {
        assert!(
            !c["argv"].as_array().unwrap().iter().any(|a| a == "--input-format"),
            "candidate used the live process: {}",
            c["cwd"]
        );
    }
}

#[test]
fn what_a_person_sees_on_a_narrow_screen() {
    let d = repo("render");
    let o = Command::new("python3")
        .arg(format!("{}/tests/pty_render.py", env!("CARGO_MANIFEST_DIR")))
        .args([env!("CARGO_BIN_EXE_qed"), &fake(), d.to_str().unwrap()])
        .output()
        .unwrap();
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success(), "{}", out);
}
