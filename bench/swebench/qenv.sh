#!/bin/sh
# Run a command inside this task's SWE-bench test environment, with the current
# working-tree changes applied on top of the base commit. Usage: ./.qenv <command...>
IMG="__IMG__"
cd "$(git rev-parse --show-toplevel)" || exit 2
P="$(mktemp -t qenv)"
git add -A -N -- . ':(exclude).qenv' ':(exclude).qed' 2>/dev/null
git diff --binary HEAD -- . ':(exclude).qenv' ':(exclude).qed' > "$P"
docker run --rm --platform linux/amd64 -v "$P":/tmp/qenv.diff:ro -e QCMD="$*" "$IMG" bash -c '
  source /opt/miniconda3/bin/activate testbed >/dev/null 2>&1
  cd /testbed
  if [ -s /tmp/qenv.diff ]; then git apply --whitespace=nowarn /tmp/qenv.diff || { echo "qenv: could not apply your changes"; exit 3; }; fi
  eval "$QCMD"'
rc=$?
rm -f "$P"
exit $rc
