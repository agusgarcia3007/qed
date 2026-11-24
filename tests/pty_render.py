#!/usr/bin/env python3
"""What a person sees, asserted on an emulated 60-column screen (tests/vt.py).
Usage: pty_render.py QED_BIN FAKE_CLAUDE WORKDIR"""
import fcntl, os, pty, select, struct, sys, termios, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vt import Screen

qed, fake, wd = (os.path.abspath(a) for a in sys.argv[1:4])
COLS = 60
fails = []


def spawn(args, env_extra):
    env = dict(os.environ, QED_CLAUDE=fake, HOME=wd, PYTHONDONTWRITEBYTECODE="1", **env_extra)
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(wd)
        os.execvpe(qed, [qed] + args, env)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, COLS, 0, 0))
    return pid, fd


def read_for(fd, secs):
    out = b""
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                b = os.read(fd, 65536)
            except OSError:
                break
            if not b:
                break
            out += b
    return out


def check(cond, what, screen):
    if cond:
        print("ok:", what)
    else:
        fails.append(what)
        print("FAIL:", what, "\n--- screen ---\n" + screen + "\n---")


# 1. One task rendered for a person.
pid, fd = spawn(["explicá"], {"FAKE_MODE": "ui"})
raw = read_for(fd, 8).decode(errors="replace")
# On a terminal, `qed "task"` is a session: after the task it waits for a follow-up.
alive = os.waitpid(pid, os.WNOHANG) == (0, 0)
os.write(fd, b"\x04")
raw += read_for(fd, 1.0).decode(errors="replace")
os.waitpid(pid, 0)
s = Screen(COLS)
s.feed(raw)
txt = s.text()
lines = s.lines()
check("**" not in txt and "`" not in txt, "markdown of a long, wrapped paragraph is rendered", txt)
check("negrita en el medio" in txt.replace("\n", " "), "the bold words are there", txt)
check(all(len(l) <= COLS for l in lines), "no row exceeds the width", txt)
para = [l for l in lines if "párrafo" in l]
check(bool(para) and not para[0].rstrip().endswith(("má", "oc", "ang")), "wraps at word boundaries", txt)
fail_row = [l for l in lines if "false-cmd" in l]
read_row = [l for l in lines if "calc.py" in l and "read" in l]
check(bool(fail_row) and "✗" in fail_row[0], "the failed tool (first of two parallel) is marked", txt)
check(bool(read_row) and "✗" not in read_row[0], "the tool that succeeded is not marked", txt)
check(any("Ahora voy a seguir." in l for l in lines), "a pause mid-sentence does not split the line", txt)
check(not any("esc to cancel" in l for l in lines), "no spinner left on screen", txt)
check(any("return a + b" in l for l in lines) and any("return a - b" in l for l in lines),
      "an Edit shows its diff", txt)
check(any("∎" in l for l in lines), "closing line", txt)
check(alive and "› explicá" in txt, "qed \"task\" opens a session that starts with the task and stays open", txt)

# 2. Completion with many skills (like a real ~/.claude/skills).
for n in ["agent-browser", "copywriting", "frontend-design", "improve", "shadcn", "tanstack-start"] + \
         [f"skill-{i:02d}" for i in range(30)]:
    os.makedirs(f"{wd}/.claude/skills/{n}", exist_ok=True)
    with open(f"{wd}/.claude/skills/{n}/SKILL.md", "w") as f:
        f.write(f"---\nname: {n}\ndescription: Does {n} things\n---\nbody {n}\n")
plug = f"{wd}/.claude/plugins/cache/mk/figma/1.0.0"
os.makedirs(f"{plug}/skills/figma-use", exist_ok=True)
with open(f"{plug}/skills/figma-use/SKILL.md", "w") as f:
    f.write("---\nname: figma-use\ndescription: Use Figma\n---\nbody\n")
os.makedirs(f"{wd}/.claude/plugins", exist_ok=True)
with open(f"{wd}/.claude/plugins/installed_plugins.json", "w") as f:
    f.write('{"version":2,"plugins":{"figma@mk":[{"scope":"user","installPath":"%s"}]}}' % plug)

pid, fd = spawn([], {"FAKE_MODE": "short"})
s = Screen(COLS)
s.feed(read_for(fd, 1.0).decode(errors="replace"))


def typed(keys, wait=0.4):
    os.write(fd, keys.encode())
    s.feed(read_for(fd, wait).decode(errors="replace"))
    return s.text()


t = typed("/")
check("more" in t, "a bare / says there are more entries than shown", t)
t = typed("imprv")
check("❯ /improve" in t, "fuzzy match: /imprv finds /improve", t)
t = typed("\x15/figma")
check("/figma:figma-use" in t or "/figma-use" in t, "plugin skills are offered", t)
t = typed("\x15/shad")
check("❯ /shadcn" in t, "prefix match ranks first", t)
long_in = " ".join(f"palabra{i:02d}" for i in range(16))  # 159 chars on a 60-column screen
t = typed("\x15" + long_in)
rows = [l for l in s.lines() if "palabra" in l]
check(len(rows) >= 3 and all(len(l) <= COLS for l in s.lines())
      and "".join(r[2:] for r in rows) == long_in,
      "a long input grows downwards in rows under the prompt, nothing cut", t)
t = typed("\x15corto")
check(not any("palabra" in l for l in s.lines()) and "› corto" in t, "clearing it leaves no rows behind", t)
os.write(fd, b"\x15\x04")
read_for(fd, 0.5)

# 3. Follow-ups in the REPL reuse one live claude process (no re-init per turn).
log = f"{wd}.fake3.log"
pid, fd = spawn([], {"FAKE_MODE": "short", "FAKE_LOG": log})
read_for(fd, 1.0)
for msg in ["uno", "dos", "tres"]:
    os.write(fd, (msg + "\r").encode())
    out = b""
    end = time.time() + 6
    while time.time() < end and "∎".encode() not in out:
        out += read_for(fd, 0.2)
import json
pids = [json.loads(l)["pid"] for l in open(log) if l.strip()] if os.path.exists(log) else []
check(len(pids) == 3 and len(set(pids)) == 1, f"three turns, one claude process (pids {pids})", "")
ages = [json.loads(l)["age"] for l in open(log) if l.strip()] if os.path.exists(log) else []
# The REPL waited ~1s before the first message: a prewarmed process is already that old.
check(bool(ages) and ages[0] > 0.5, f"claude is prewarmed before the first prompt (age {ages[:1]})", "")
# /restart replaces the process (same pid: exec) and carries on the same session.
os.write(fd, b"/restart\r")
scr3 = Screen(COLS)
scr3.feed(read_for(fd, 1.5).decode(errors="replace"))
t3 = scr3.text()
check("↺" in t3 and "uno" in t3 and os.waitpid(pid, os.WNOHANG) == (0, 0),
      "/restart reopens qed on the same session", t3)
os.write(fd, b"cuatro\r")
scr3.feed(read_for(fd, 2.0).decode(errors="replace"))
sessions = [json.loads(l).get("argv", []) for l in open(log) if l.strip()]
check(len(sessions) == 4 and "--resume" in sessions[-1], "the turn after /restart resumes the Claude Code session", scr3.text())
os.write(fd, b"\x04")
read_for(fd, 0.5)

# 4. Harness features in the REPL: permission card, goals, undo, context meter.
def repl(mode, log):
    pid, fd = spawn([], {"FAKE_MODE": mode, "FAKE_LOG": log})
    scr = Screen(COLS)
    scr.feed(read_for(fd, 1.0).decode(errors="replace"))
    return pid, fd, scr

def until(fd, scr, pat, secs=6):
    end = time.time() + secs
    while time.time() < end:
        scr.feed(read_for(fd, 0.2).decode(errors="replace"))
        if pat in scr.text():
            return True
    return False

target = f"{wd}/made-by-agent"
pid, fd, scr = repl("perm", f"{wd}.fake4.log")
os.write(fd, b"/perm ask\r")
until(fd, scr, "perm ask")
os.write(fd, b"crea el archivo\r")
check(until(fd, scr, "[n]o"), "ask mode shows a permission card", scr.text())
os.write(fd, b"n")
check(until(fd, scr, "no me dejaron") and not os.path.exists(target), "n denies: the command did not run", scr.text())
check("[n]o" not in scr.text(), "the card is erased after answering", scr.text())
os.write(fd, b"otra vez\r")
until(fd, scr, "[n]o")
os.write(fd, b"y")
check(until(fd, scr, "hecho") and os.path.exists(target), "y allows: the command ran", scr.text())
os.write(fd, b"\x04"); read_for(fd, 0.5)

calc = open(f"{wd}/calc.py").read()
glog = f"{wd}.fake5.log"
pid, fd, scr = repl("goal", glog)
os.write(fd, b"/goal que add sume bien\r")
check(until(fd, scr, "goal met", 10), "/goal keeps working until the model reports GOAL: met", scr.text())
turns = len(open(glog).read().splitlines()) if os.path.exists(glog) else 0
check(turns == 2, f"two turns for the goal (got {turns})", scr.text())
os.write(fd, b"/goal\r")
check(until(fd, scr, "◎ que add sume bien"), "/goal shows the active goal", scr.text())
check("ctx 11k/200k (6%)" in scr.text(), "footer shows context use against the window", scr.text())
os.write(fd, b"/undo\r")
check(until(fd, scr, "undid") and open(f"{wd}/calc.py").read() == calc, "/undo restores files from before the turn", scr.text())
os.write(fd, b"\x04"); read_for(fd, 0.5)

sys.exit(1 if fails else 0)
