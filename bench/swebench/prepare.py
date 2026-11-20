"""Generate qed eval tasks from hard SWE-bench Verified instances.
Usage: prepare.py N_PER_REPO_SPEC   e.g. 'django/django=4,sympy/sympy=2,...'
Problem statement only (no hints, no gold patch, no tests) reaches the agent."""
import json, subprocess, sys
from pathlib import Path

here = Path(__file__).resolve().parent
spec = dict(kv.split("=") for kv in sys.argv[1].split(","))
hard = sorted(json.load(open(here / "hard.json")), key=lambda r: r["instance_id"])
picked = []
for repo, n in spec.items():
    picked += [r for r in hard if r["repo"] == repo][: int(n)]
py = here / ".venv" / "bin" / "python"
for r in picked:
    iid, repo = r["instance_id"], r["repo"]
    cache = here / "cache" / repo.replace("/", "__")
    if not cache.exists():
        subprocess.run(["git", "clone", "-q", f"https://github.com/{repo}.git", str(cache)], check=True)
    img = f"swebench/sweb.eval.x86_64.{iid.replace('__', '_1776_')}:latest".lower()
    td = here / "tasks" / iid
    td.mkdir(parents=True, exist_ok=True)
    (td / "qenv.sh").write_text((here / "qenv.sh").read_text().replace("__IMG__", img))
    prompt = (r["problem_statement"].strip() + "\n\n---\n"
              "The repository is checked out at the base commit of this issue. Fix the issue in the source code.\n"
              "The project's test environment (right Python version and dependencies) is a Docker image; the host "
              "Python does not have the dependencies. Run any command inside it, with your current changes applied, "
              "via `./.qenv <command>` (for example `./.qenv python -m pytest path/to/test.py -x -q`, or the project's "
              "own test runner).")
    task = {"id": iid, "category": "swe-hard", "repo_git": str(cache), "commit": r["base_commit"],
            "image": img, "web": False, "budget_s": 2400, "budget_usd": 10, "hidden_timeout_s": 2400,
            "setup": f"cp '{td / 'qenv.sh'}' .qenv && chmod +x .qenv && echo .qenv >> .git/info/exclude",
            "prompt": prompt, "hidden_cmd": f"'{py}' '{here / 'score.py'}' {iid}",
            "forbidden_urls": [f"github.com/{repo}/pull", f"github.com/{repo}/commit"]}
    (td / "task.json").write_text(json.dumps(task, indent=2))
    print(iid, r["difficulty"], img)
