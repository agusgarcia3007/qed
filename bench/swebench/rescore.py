"""Repair final diffs damaged by the trailing-whitespace trim bug and re-score them.

The bug stripped trailing whitespace from the whole diff: the final newline and any
trailing blank context lines (" "). Hunk headers say how many lines each hunk has, so
the missing blank context lines are restored exactly. Every repaired patch is checked
with `git apply --check` on the base commit before scoring with the official harness.

Usage: rescore.py RUN_DIR... -> writes RUN_DIR-fix/results.jsonl (only `solved` changes)."""
import json, os, re, subprocess, sys, tempfile
from pathlib import Path

here = Path(__file__).resolve().parent
HUNK = re.compile(r"^@@ -\d+(?:,(\d+))? \+\d+(?:,(\d+))? @@")


def repair(diff: str) -> str:
    if not diff.strip():
        return diff
    lines = diff.rstrip("\n").split("\n")
    # Find the last hunk header and count what it actually has.
    h = max(i for i, l in enumerate(lines) if HUNK.match(l))
    m = HUNK.match(lines[h])
    old_n, new_n = int(m.group(1) or 1), int(m.group(2) or 1)
    old = new = 0
    for l in lines[h + 1:]:
        if l.startswith("\\"):
            continue
        if l.startswith("-"):
            old += 1
        elif l.startswith("+"):
            new += 1
        else:
            old += 1
            new += 1
    missing = old_n - old
    if missing > 0 and missing == new_n - new:
        lines += [" "] * missing
    return "\n".join(lines) + "\n"


def applies(task: dict, diff: str) -> bool:
    with tempfile.TemporaryDirectory() as d:
        subprocess.run(["git", "clone", "-q", "--shared", task["repo_git"], d], check=True)
        subprocess.run(["git", "checkout", "-q", "--detach", task["commit"]], cwd=d, check=True)
        p = Path(d) / "p.diff"
        p.write_text(diff)
        return subprocess.run(["git", "apply", "--check", str(p)], cwd=d, capture_output=True).returncode == 0


def failed_to_apply(run: Path, r: dict) -> bool:
    log = (run / f"{r['task']}-{r['variant']}-r{r['rep']}" / "hidden.log").read_text()
    m = re.search(r"(qed-\d+)", log)
    if not m:
        return False
    ri = here / "scoring" / "logs" / "run_evaluation" / m.group(1) / "qed" / r["task"] / "run_instance.log"
    return ri.exists() and "Patch Apply Failed" in ri.read_text()


py = str(here / ".venv" / "bin" / "python")
todo = []  # (run, row, repaired_diff_path)
for run in map(Path, sys.argv[1:]):
    for line in open(run / "results.jsonl"):
        r = json.loads(line)
        rd = run / f"{r['task']}-{r['variant']}-r{r['rep']}"
        if not failed_to_apply(run, r):
            continue
        task = json.load(open(here / "tasks" / r["task"] / "task.json"))
        fixed = repair((rd / "final.diff").read_text())
        ok = applies(task, fixed)
        (rd / "final.fixed.diff").write_text(fixed)
        print(f"{run.name} {r['task']} {r['variant']}: reparado, git apply --check {'OK' if ok else 'FALLA'}", flush=True)
        todo.append((run, r, rd / "final.fixed.diff", ok))

# Re-score grouped by instance so each image is pulled once.
scores = {}
for task_id in sorted({r["task"] for _, r, _, _ in todo}):
    img = json.load(open(here / "tasks" / task_id / "task.json"))["image"]
    subprocess.run(["docker", "pull", "--platform", "linux/amd64", "-q", img], capture_output=True)
    for run, r, path, ok in [t for t in todo if t[1]["task"] == task_id]:
        res = subprocess.run([py, str(here / "score.py"), task_id], env={**os.environ, "QED_DIFF": str(path)},
                             capture_output=True, text=True)
        scores[(run.name, task_id, r["variant"], r["rep"])] = res.returncode == 0
        print(f"  score {run.name} {task_id} {r['variant']}: {'resuelta' if res.returncode == 0 else 'no resuelta'}", flush=True)
    subprocess.run(["docker", "rmi", "-f", img], capture_output=True)

for run in map(Path, sys.argv[1:]):
    out = Path(str(run) + "-fix")
    out.mkdir(exist_ok=True)
    with open(out / "results.jsonl", "w") as f:
        for line in open(run / "results.jsonl"):
            r = json.loads(line)
            k = (run.name, r["task"], r["variant"], r["rep"])
            if k in scores:
                r["solved"] = scores[k] and not r.get("contaminated")
                r["rescored"] = True
            f.write(json.dumps(r) + "\n")
print("listo")
