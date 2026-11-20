"""Score one qed run with the official SWE-bench harness. Exit 0 iff resolved.
Reads the final diff from $QED_DIFF; runs outside the agent's workspace and session."""
import json, os, subprocess, sys, time
from pathlib import Path

iid = sys.argv[1]
diff = Path(os.environ["QED_DIFF"]).read_text()
work = Path(__file__).resolve().parent / "scoring"
work.mkdir(exist_ok=True)
run_id = f"qed-{int(time.time() * 1000)}"
preds = work / f"{run_id}.json"
preds.write_text(json.dumps([{"instance_id": iid, "model_name_or_path": "qed", "model_patch": diff}]))
subprocess.run([sys.executable, "-m", "swebench.harness.run_evaluation",
                "--dataset_name", "princeton-nlp/SWE-bench_Verified", "--predictions_path", str(preds),
                "--instance_ids", iid, "--max_workers", "1", "--run_id", run_id,
                "--namespace", "swebench", "--cache_level", "instance", "--report_dir", str(work)], cwd=work, check=False)
rep = None
for p in work.glob(f"qed.{run_id}.json"):
    rep = json.loads(p.read_text())
resolved = bool(rep) and iid in rep.get("resolved_ids", [])
print(f"[score] {iid} resolved={resolved} empty_patch={not diff.strip()}")
sys.exit(0 if resolved else 1)
