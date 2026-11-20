#!/usr/bin/env python3
"""Deterministic stand-in for `claude -p --output-format stream-json`.
Tests the harness, never the model. Behavior from FAKE_MODE; argv/cwd/prompt logged to FAKE_LOG."""
import json, os, signal, subprocess, sys, time, uuid

import threading, queue
STARTED = time.time()
argv = sys.argv[1:]
STREAM = "--input-format" in argv and argv[argv.index("--input-format") + 1] == "stream-json"
prompt = "" if STREAM else sys.stdin.read()
mode = os.environ.get("FAKE_MODE", "short")
cwd = os.getcwd()
sid = argv[argv.index("--resume") + 1] if "--resume" in argv else str(uuid.uuid4())
critic = "--json-schema" in argv

def log(prompt):
    if os.environ.get("FAKE_LOG"):
        with open(os.environ["FAKE_LOG"], "a") as f:
            f.write(json.dumps({"argv": argv, "cwd": cwd, "prompt": prompt, "critic": critic,
                                "pid": os.getpid(), "age": round(time.time() - STARTED, 3)}) + "\n")

INTERRUPT = threading.Event()
ANSWERS = queue.Queue()

def ask(tool, inp):
    """Like Claude Code with --permission-prompt-tool stdio: ask the host, wait for it."""
    out({"type": "control_request", "request_id": "perm-1", "request": {"subtype": "can_use_tool",
         "tool_name": tool, "input": inp, "description": inp.get("description", ""),
         "permission_suggestions": [{"type": "addRules", "rules": [{"toolName": tool, "ruleContent": inp.get("command", "")}],
                                     "behavior": "allow", "destination": "localSettings"}]}})
    return ANSWERS.get(timeout=30)
TOTAL = [0.0]

def out(o):
    o.setdefault("parent_tool_use_id", None)
    print(json.dumps(o), flush=True)

def text(t):
    out({"type": "stream_event", "event": {"type": "content_block_delta", "delta": {"type": "text_delta", "text": t}}})

def bash(cmd):
    out({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": cmd}}]}})
    r = subprocess.run(cmd, shell=True, capture_output=True, text=True)
    out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t", "content": r.stdout[-500:], "is_error": r.returncode != 0}]}})

def result(t, structured=None):
    # Like Claude Code: with streamed input the cost is cumulative over the process.
    TOTAL[0] += 0.01
    o = {"type": "result", "session_id": sid, "result": t, "total_cost_usd": round(TOTAL[0], 4), "is_error": False, "num_turns": 1,
         "modelUsage": {"claude-fake": {"inputTokens": 3, "contextWindow": 200000}, "claude-fake-small": {"contextWindow": 100000}},
         "usage": {"input_tokens": 3, "cache_read_input_tokens": 20000, "output_tokens": 50,
                   "iterations": [{"input_tokens": 2, "cache_read_input_tokens": 9000}, {"input_tokens": 1, "cache_read_input_tokens": 11000}]}}
    if structured is not None:
        o["structured_output"] = structured
    out(o)

def write(path, content):
    out({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "w", "name": "Write", "input": {"file_path": path, "content": content}}]}})
    with open(path, "w") as f:
        f.write(content)

def turn(prompt):
    log(prompt)
    out({"type": "system", "subtype": "init", "session_id": sid, "tools": ["Bash"]})
    # Like the API's message_start: the request's prompt size (the context meter's live value).
    out({"type": "stream_event", "event": {"type": "message_start", "message": {"usage": {"input_tokens": 5, "cache_read_input_tokens": 42000}}}})

    GOOD = "def add(a, b):\n    return a + b\n"

    if critic:
        inval = os.environ.get("FAKE_INVALIDATE") == "1"
        result("{}", {"verdict": "counterexample", "hypothesis_invalidated": inval,
                      "finding": "add() subtracts instead of adding", "repro": "python3 test.py", "refs": ["calc.py:2"]})
    elif mode == "short":
        bash("qed check -- true")
        text("listo")
        result("listo")
    elif mode == "md":
        # Markdown streamed in small chunks that split marks across deltas, plus a failing tool.
        bash("false")
        doc = "## Plan\n- usa **negrita** y `code`\n```\nx = 1\n```\nfin"
        for i in range(0, len(doc), 3):
            text(doc[i:i + 3])
            time.sleep(0.01)
        result("fin")
    elif mode == "bug":
        bash("qed check -- python3 test.py")  # must fail on the original code
        write("calc.py", GOOD)
        bash("qed check -- python3 test.py")
        text("arreglado")
        result("arreglado")
    elif mode == "stuck":
        if cwd.endswith("-alt"):
            write("calc.py", GOOD)
            bash("qed check -- python3 test.py")
            result("hipótesis alternativa aplicada")
        else:
            # Wrong hypothesis: same failure every round.
            write("calc.py", "def add(a, b):\n    return a - b  # author\n")
            result("intenté algo")
    elif mode == "seen_fail":
        # Ends with a failing check it has seen (e.g. pre-existing suite failures).
        bash("qed check -- false")
        result("fallas preexistentes, no relacionadas")
    elif mode == "stale":
        # Checks, then edits afterwards: its evidence is stale and the check now fails.
        bash("qed check -- test ! -f marker")
        write("marker", "x")
        result("listo")
    elif mode == "deep":
        # Three candidates differ by worktree name: c0 wrong fix, c1 right fix, c2 test only.
        tag = os.path.basename(cwd)[-2:]
        os.makedirs("tests", exist_ok=True)
        write(f"tests/test_add_{tag}.py", "import sys; sys.path.insert(0, '.')\nfrom calc import add\nassert add(2, 3) == 5\nassert add(-1, 1) == 0\nprint('ok')\n")
        if tag == "c1":
            write("calc.py", GOOD)
        elif tag == "c0":
            write("calc.py", "def add(a, b):\n    return a * b\n")
        # Like .qenv, the check marks new files intent-to-add: selection's reset must cope.
        bash(f"qed check -- 'git add -A -N . && python3 tests/test_add_{tag}.py'")
        result("listo")
    elif mode == "ui":
        # What a person sees: a long paragraph with marks split across deltas (wraps at 60
        # columns), parallel tools where the FIRST fails, a mid-line pause, an Edit.
        para = ("Este es un párrafo largo que ocupa más de una fila en una terminal angosta y "
                "tiene **negrita en el medio** y `código` también, para ver que se renderiza.\n")
        for i in range(0, len(para), 4):
            text(para[i:i + 4])
        out({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": "a", "name": "Bash", "input": {"command": "false-cmd"}},
            {"type": "tool_use", "id": "b", "name": "Read", "input": {"file_path": cwd + "/calc.py"}}]}})
        out({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "a", "content": "boom", "is_error": True},
            {"type": "tool_result", "tool_use_id": "b", "content": "1\tdef add", "is_error": False}]}})
        text("Ahora voy")
        time.sleep(float(os.environ.get("FAKE_PAUSE", "2.2")))
        text(" a seguir.\n")
        out({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "e", "name": "Edit",
            "input": {"file_path": cwd + "/calc.py", "old_string": "    return a - b", "new_string": "    return a + b"}}]}})
        out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "e", "content": "ok", "is_error": False}]}})
        text("listo")
        result("listo")
    elif mode == "perm":
        cmd = "touch made-by-agent"
        out({"type": "assistant", "message": {"content": [{"type": "tool_use", "id": "p", "name": "Bash", "input": {"command": cmd}}]}})
        a = ask("Bash", {"command": cmd, "description": "Create a file"}) if "stdio" in argv else {"behavior": "allow"}
        if a.get("behavior") == "allow":
            subprocess.run(cmd, shell=True)
            out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "p", "content": "", "is_error": False}]}})
            text("hecho")
        else:
            out({"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "p", "content": a.get("message", "denied"), "is_error": True}]}})
            text("no me dejaron")
        result("fin")
    elif mode == "goal":
        # Not met on the first turn, met on the second.
        n = len(open(os.environ["FAKE_LOG"]).read().splitlines()) if os.environ.get("FAKE_LOG") else 1
        if "Goal" in prompt and n <= 1:
            write("calc.py", "def add(a, b):\n    return a + b  # goal\n")
            text("avancé\nGOAL: not met — falta probar")
            result("avancé\nGOAL: not met — falta probar")
        else:
            text("probado\nGOAL: met")
            result("probado\nGOAL: met")
    elif mode == "slow":
        if not STREAM:
            signal.signal(signal.SIGINT, lambda *_: sys.exit(130))
        text("pensando")
        end = time.time() + 30
        while time.time() < end:
            if INTERRUPT.is_set():
                INTERRUPT.clear()
                TOTAL[0] += 0.001
                out({"type": "result", "subtype": "error_during_execution", "session_id": sid, "result": "",
                     "total_cost_usd": round(TOTAL[0], 4), "is_error": True, "num_turns": 1})
                return
            time.sleep(0.05)
        result("tarde")
    elif mode == "ttft":
        time.sleep(0.3)
        text("hola")
        result("hola")


if not STREAM:
    turn(prompt)
else:
    msgs = queue.Queue()
    def reader():
        for line in sys.stdin:
            m = json.loads(line)
            if m.get("type") == "control_request" and m["request"]["subtype"] == "interrupt":
                INTERRUPT.set()
                out({"type": "control_response", "response": {"subtype": "success", "request_id": m["request_id"]}})
            elif m.get("type") == "control_response":
                ANSWERS.put(m["response"].get("response", {}))
            elif m.get("type") == "user":
                msgs.put(m["message"]["content"])
        msgs.put(None)
    threading.Thread(target=reader, daemon=True).start()
    while True:
        p = msgs.get()
        if p is None:
            break
        INTERRUPT.clear()
        turn(p)
