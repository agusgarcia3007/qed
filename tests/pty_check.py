#!/usr/bin/env python3
"""Drive the interactive REPL in a real pseudo-terminal: Esc cancels a turn, text typed
during a turn is queued and runs next, slash commands and !cmd work, Ctrl-D exits.
Usage: pty_check.py QED_BIN FAKE_CLAUDE WORKDIR"""
import os, pty, select, sys, time

qed, fake, wd = (os.path.abspath(a) for a in sys.argv[1:4])
log = os.path.join(wd, "fake.log")
env = dict(os.environ, QED_CLAUDE=fake, FAKE_MODE="slow", HOME=wd, FAKE_LOG=log)
# A user skill (HOME=wd, so ~/.claude/skills lives inside the workdir).
os.makedirs(os.path.join(wd, ".claude/skills/improve"), exist_ok=True)
with open(os.path.join(wd, ".claude/skills/improve/SKILL.md"), "w") as f:
    f.write("---\nname: improve\ndescription: Survey the codebase as an advisor\n---\n\n# Improve\nCUERPO-DE-LA-SKILL\n")
pid, fd = pty.fork()
if pid == 0:
    os.chdir(wd)
    os.execvpe(qed, [qed], env)

buf = b""
pos = 0  # each expect only looks at output after the previous match
def read_until(pat, timeout=8.0):
    global buf, pos
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                buf += os.read(fd, 4096)
            except OSError:
                break
        i = buf.find(pat.encode(), pos)
        if i >= 0:
            pos = i + len(pat)
            return True
    return False

def send(s):
    os.write(fd, s.encode())

def expect(pat, what, timeout=8.0):
    if not read_until(pat, timeout):
        print(f"FAIL: {what} (no '{pat}')\n--- output ---\n{buf.decode(errors='replace')[-2000:]}")
        sys.exit(1)
    print(f"ok: {what}")

expect("∎\x1b[0m qed", "cabecera de una línea")
expect("task, /command or !shell", "placeholder en el prompt vacío")
send("tarea lenta\r")
expect("esc to cancel", "spinner visible durante el turno")
expect("ctx 42k", "el spinner muestra el contexto en uso durante el turno")
send("siguiente cosa\r")          # typed while the agent works -> queued
expect("1 queued", "lo escrito durante el turno queda en cola")
time.sleep(0.3)
send("\x1b")                     # Esc
expect("cancelled", "Esc cancela el turno")
expect("› \x1b[0msiguiente cosa", "el mensaje en cola corre después")
send("\x1b")                     # cancel the queued one too
expect("cancelled", "Esc cancela también el encolado", 6)
time.sleep(0.3)
send("/model sonnet\r")
expect("model sonnet", "/model cambia el modelo")
send("/effort max\r")
expect("effort max", "/effort cambia el esfuerzo")
send("/mo")
expect("❯ /model", "menú de autocompletado al escribir /mo")
send("\t")
expect("❯ opus", "Tab completa /model y ofrece modelos")
send("ha")
expect("iku", "texto fantasma con el resto de la sugerencia")
send("\r")
expect("model haiku", "Enter acepta y ejecuta /model haiku")
send("/eff\r")
expect("❯ low", "Enter en /eff completa y espera el argumento")
send("xh\r")
expect("effort xhigh", "argumento completado y ejecutado")
send("!echo hola-desde-shell\r")
expect("hola-desde-shell", "!cmd corre sin el modelo")
expect("\x1b[1m› ", "prompt de vuelta")
send("/new\r")
expect("\x1b[2J", "/new limpia la pantalla")
expect("∎\x1b[0m qed", "/new vuelve a mostrar la cabecera")
send("otra tarea\r")
expect("esc to cancel", "segunda sesión arranca")
time.sleep(0.3)
send("\x1b")
expect("cancelled", "segunda sesión cancelada")
time.sleep(0.3)
send("/resume\r")
expect("resume · ↑↓", "/resume abre el selector")
expect("❯ otra tarea", "la sesión más reciente va primero")
send("\x1b[B")                  # ↓
expect("❯ tarea lenta", "↓ mueve la selección")
send("\r")
expect("↺\x1b[0m tarea lenta", "Enter retoma la sesión elegida")
expect("› siguiente cosa", "el resumen muestra el último mensaje")
send("/resume zzzz\r")
expect("session not found", "id inexistente")
send("/imp")
expect("❯ /improve", "las skills del usuario aparecen en el autocompletado")
expect("Survey the codebase", "con su descripción")
send("\t")
send("el parser\r")
expect("esc to cancel", "/improve corre como turno")
time.sleep(0.3)
send("\x1b")
expect("cancelled", "turno de la skill cancelado")
time.sleep(0.3)
last = [l for l in open(log).read().splitlines() if l.strip()][-1]
if "CUERPO-DE-LA-SKILL" not in last or "el parser" not in last:
    print("FAIL: el prompt no incluye la skill y sus argumentos\n" + last[-800:]); sys.exit(1)
print("ok: el prompt lleva el cuerpo de la skill y los argumentos")
send("\x04")                     # Ctrl-D on empty line
for _ in range(50):
    done, status = os.waitpid(pid, os.WNOHANG)
    if done:
        break
    read_until("\0", 0.1)
else:
    print("FAIL: Ctrl-D no salió\n" + buf.decode(errors="replace")[-800:])
    os.kill(pid, 9)
    sys.exit(1)
print("ok: Ctrl-D sale" if os.WEXITSTATUS(status) == 0 else f"FAIL: exit {status}")
sys.exit(0 if os.WEXITSTATUS(status) == 0 else 1)
