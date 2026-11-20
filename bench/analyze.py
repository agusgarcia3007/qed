#!/usr/bin/env python3
"""Paired comparison of eval variants on the same tasks.

Usage: analyze.py SOURCE [SOURCE...] [--base A] [--budget-s N]
SOURCE is a run dir, optionally with renames: bench/runs/swe1:C=P1,D=P1b
Later sources override earlier rows for the same (task, variant, rep) — use it for reruns.
--budget-s N: a run over N seconds of wall time counts as unsolved (same rule for all).
Rows with infra_error are dropped. For each variant vs the baseline: tasks only it solves,
tasks only the baseline solves, and an exact two-sided sign test (McNemar) on those."""
import json, sys
from collections import defaultdict
from math import comb

args = sys.argv[1:]
base, budget, sources = "A", None, []
i = 0
while i < len(args):
    if args[i] == "--base":
        base = args[i + 1]; i += 2
    elif args[i] == "--budget-s":
        budget = float(args[i + 1]); i += 2
    else:
        sources.append(args[i]); i += 1

rows = {}
for src in sources:
    path, _, ren = src.partition(":")
    names = dict(kv.split("=") for kv in ren.split(",")) if ren else {}
    for line in open(f"{path}/results.jsonl"):
        r = json.loads(line)
        if r.get("infra_error"):
            continue
        r["variant"] = names.get(r["variant"], r["variant"])
        if budget is not None and r["wall_ms"] / 1000 > budget:
            r["solved"] = False
        rows[(r["task"], r["variant"], r.get("rep", 0))] = r

by = defaultdict(lambda: defaultdict(list))
cost, wall = defaultdict(float), defaultdict(list)
for r in rows.values():
    by[r["task"]][r["variant"]].append(bool(r["solved"]))
    cost[r["variant"]] += r["cost"]
    wall[r["variant"]].append(r["wall_ms"] / 1000)


def solved(task, v):
    s = by[task][v]
    return sum(s) / len(s) >= 0.5 if s else None


variants = sorted({v for t in by for v in by[t]}, key=lambda v: (v != base, v))
print(f"{len(by)} tareas · baseline {base}" + (f" · presupuesto {budget:.0f}s" if budget else "") + "\n")
print("variante  resueltas  $total   mediana_s  | vs base: sólo_ella  sólo_base  p(McNemar)")
for v in variants:
    tasks = [t for t in by if solved(t, v) is not None]
    n_ok = sum(solved(t, v) for t in tasks)
    w = sorted(wall[v])
    med = w[len(w) // 2] if w else 0
    line = f"{v:<9} {n_ok:>3}/{len(tasks):<5} {cost[v]:>7.2f}  {med:>8.0f}  "
    if v != base:
        pair = [t for t in tasks if solved(t, base) is not None]
        b = sum(1 for t in pair if solved(t, v) and not solved(t, base))
        c = sum(1 for t in pair if solved(t, base) and not solved(t, v))
        n = b + c
        p = min(1.0, 2 * sum(comb(n, k) for k in range(0, min(b, c) + 1)) / 2**n) if n else 1.0
        line += f"| {b:>9}  {c:>9}  {p:>10.3f}"
    print(line)
print("\npor tarea:")
for t in sorted(by):
    print(f"  {t:<34} " + "  ".join(f"{v}:{'✓' if solved(t, v) else '✗'}" for v in variants if solved(t, v) is not None))
