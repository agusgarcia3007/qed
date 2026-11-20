#!/bin/sh
# Usage: run.sh OUT_DIR VARIANTS INSTANCE_ID...
# Per instance: pull image -> qed eval (variant order alternates across instances) -> remove image.
here=$(cd "$(dirname "$0")" && pwd)
qed="${QED_BIN:-$here/../../target/release/qed}"
out=$1; vars=$2; shift 2
i=0
for id in "$@"; do
  img=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["image"])' "$here/tasks/$id/task.json")
  docker pull --platform linux/amd64 -q "$img" >/dev/null || { echo "pull falló: $id"; continue; }
  order=$vars
  [ $((i % 2)) -eq 1 ] && order=$(echo "$vars" | tr ',' '\n' | tail -r | paste -sd, -)
  "$qed" eval "$here/tasks" --only "$id" --variants "$order" --out "$out"
  rc=$?
  docker rmi -f "$img" >/dev/null 2>&1
  [ $rc -eq 3 ] && { echo "cuota agotada, corto"; exit 3; }
  i=$((i + 1))
done
