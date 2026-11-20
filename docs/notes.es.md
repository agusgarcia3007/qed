# qed

Harness mínimo en Rust sobre **Claude Code oficial** (`claude -p`, stream-json, login de tu suscripción).
Binario de ~1.1 MB, dos dependencias (`serde_json`, `libc`) y ningún runtime async. No lanza el modelo hasta el primer prompt.

```bash
cargo install --path . --locked     # instala en ~/.cargo/bin (repetir tras cambios)
qed                                   # interactivo
qed "arreglá X" --check "pytest -q"   # una tarea, salida legible
qed -p "arreglá X" --check "..."      # headless: eventos JSON por línea (mismo motor)
qed resume                            # elegís la sesión con ↑↓ (qed -c: la última; acepta un pedazo del id)
qed status                            # evidencia: qué checks siguen vigentes en el árbol actual
qed eval bench/tasks --variants A,B,C,D --repeat 3
qed logo                              # ∎ quod erat demonstrandum (QED_LOGO=1 lo anima también al abrir)
```

En el modo interactivo:
- **Esc** o **Ctrl-C** cancelan el turno y la sesión sigue reanudable.
- Lo que escribís mientras el agente trabaja queda en cola: Enter lo confirma y corre al terminar el turno.
- La interfaz está en inglés. El spinner muestra la fase actual (thinking / running / reading / lo que hace un subagente), el tiempo y los tokens que van saliendo. Si el modelo pausa a mitad de frase, el spinner se dibuja en la fila de abajo y la frase no se corta.
- Salida: el texto del modelo es lo único a color normal, con markdown en streaming (títulos, listas, numeradas, citas, **negrita**, `code`, bloques de código y tablas), y qed lo ajusta por palabra al ancho de la terminal (mide emoji y CJK como dos columnas). Cada herramienta es una línea tenue (`$ cmd`, `read x.rs`, `~ x.rs +3 −1` con el diff en rojo/verde). Cuando llega el resultado, esa misma fila se actualiza en el lugar, aunque haya varias herramientas en paralelo: ✗ si falló, la duración si tardó más de 1s. El turno cierra con `∎ 4.2s · $0.012`. El detalle por llamada queda en el trace y en `-p`.
- Editor: ↑/↓ historial (`~/.qed_history`), ←/→, Ctrl-A/E/U/W/K, pegado multilínea, `\`+Enter para una línea nueva, Tab completa comandos. Bajo el prompt vacío hay una línea con modelo · esfuerzo · modo · costo y los atajos.
- Autocompletado: comandos, skills y commands de `.claude/` (proyecto y usuario) y de los plugins instalados (`/plugin:skill`). Busca por prefijo, por palabra (`/design` → `/frontend-design`), por subcadena, por letras en orden (`/imprv` → `/improve`) y por descripción. Si hay más entradas que las visibles, el menú lo avisa (`↓ 40 more`).
- Comandos: `/model` `/effort` `/mode` `/check` `/budget` `/critic` `/alt` `/web` `/goal` `/undo` `/perm` `/config` `/new` `/resume` `/status` `/diff` `/cost` `/fresh` `/logo` `/help` `/quit`.
- **Goals** — `/goal <objetivo>`: qed toma turnos hasta que el modelo cierra con `GOAL: met`, lo cancelás, o se cumplen 5 turnos. El objetivo queda en la sesión y se ve en la línea de ayuda (`◎ …`). `/goal` lo muestra con su estado y `/goal clear` lo borra.
- **Permisos** — `/perm ask` (o `--ask`, o `"ask": true` en la config): las ediciones y los `qed check` corren solos. Bash y WebFetch muestran una tarjeta: `[y]es`, `[a]lways` (regla para el resto de la sesión) o `[n]o`/Esc. Al denegar, el modelo recibe la instrucción de no reintentar lo mismo. Usa el protocolo de host de Claude Code (`--permission-prompt-tool stdio`, `can_use_tool`) con `--permission-mode default`. `/perm auto` es el default.
- **`/undo`** — antes de cada turno qed guarda un checkpoint del árbol. `/undo` deja los archivos como estaban antes del último turno (un goal cuenta como uno solo), guarda el estado que se deshace en `refs/qed/` para poder rehacer, y le avisa al modelo en el turno siguiente. No toca el índice ni HEAD.
- La línea de ayuda muestra el contexto en uso (`ctx 23k`, tamaño del último request) y el costo de la sesión.
- `/resume` abre la lista de sesiones recientes (↑↓, 1-9, Enter); al retomar ves tu último mensaje y el final de la respuesta.
- `!cmd` corre un comando de shell sin pasar por el modelo.

Config opcional en `.qed/config.json`: `check`, `setup`, `model`, `effort`, `budget_s`, `budget_usd`, `max_rounds`, `critic`, `alt`, `web`, `lean`.

## Cómo funciona

- **Motor** (`src/engine.rs`): cada llamada al modelo es un `claude -p` en modo stream-json. En el REPL, la conversación principal vive en **un solo proceso** con `--input-format stream-json`. Los follow-ups no pagan arranque ni init (medido con haiku: 1.0s contra 1.8s), y Esc manda un `interrupt` de control en vez de matar el proceso. El proceso se reinicia (con `--resume`) si cambia el modelo, el esfuerzo o la sesión. El crítico, la rama alternativa, `eval` y `-p` siguen usando un proceso por llamada. Los eventos se normalizan a un solo stream que alimenta el render humano (texto en streaming, diffs inline de cada Edit, comandos) y el JSON headless, y siempre queda en `.qed/sessions/<id>/events.jsonl`. Con Ctrl-C se le manda SIGINT a Claude Code, que guarda su sesión, así que `qed resume` la retoma con `--resume`.
- **Contexto acotado de entrada**: por defecto usa `--strict-mcp-config --disable-slash-commands --setting-sources project,local`. Medido en esta máquina: el prompt baja de **268k a 9.5k tokens** y un "ok" pasa de **$2.15 y 13.5s a $0.03 y 3.3s**. Eso abarata, pero no es una mejora de capacidad. `--full` vuelve a cargar tus MCP y skills.
- **Herramientas**: Read, Edit, Write, Bash, Grep, Glob, WebSearch y WebFetch son las nativas de Claude Code, no reescritas. Browser, LSP y MCP no se cargan al inicio.
- **Evidencia** (`src/store.rs`): `qed check -- CMD` (lo usa el modelo vía Bash) corre el check, lo ata al hash exacto del árbol de trabajo (incluye los archivos sin trackear), guarda el log completo en disco y devuelve solo la cola. Un cambio posterior lo marca **vencido**. Lo observado y lo inferido se registran por separado.
- **Camino corto** (`src/run.rs`): un solo escritor. Si hay `check`, el harness lo corre al final y, si falla, le devuelve la salida al mismo autor (`--resume`), hasta `max_rounds` veces.
- **Escalamiento por señales observables** (modo `adaptive`, el default):
  - el mismo comando falla dos veces seguidas con la misma huella (cola normalizada), o no corrió ningún check sobre el estado final → **crítico** con contexto fresco y solo lectura (Read/Grep/Glob, más web si está permitida). Recibe la tarea, el diff y la evidencia, pero no los mensajes del autor. Tiene que devolver por JSON schema un contraejemplo reproducible, una referencia o "insuficiente". El harness corre el `repro`, y solo si falla cuenta como observado.
  - si hay un contraejemplo reproducido que invalida la hipótesis → **rama alternativa** en un `git worktree` aparte, desde el checkpoint inicial, con un paquete breve de continuación (hechos vigentes, hipótesis rechazada). Las dos ramas se comparan con los mismos checks y gana la que pasa más. Si empatan, se queda la original. El intento original queda guardado en `refs/qed/<id>/main-before-alt`.
  - el crítico y la rama alternativa gastan del mismo presupuesto global de tiempo y USD.

## Qué está probado, qué está medido, qué es hipótesis

**Probado** (17 tests de integración, más unitarios, uno de ellos maneja el REPL en una pseudo-terminal real, `cargo test`, con un `claude` falso determinista en `tests/fake_claude.py`; prueban el harness, no el modelo):
una tarea corta no dispara subagentes (1 llamada) · un bug con repro que falla en el árbol base y pasa después · un fallo repetido activa al crítico (solo lectura, sin `--resume`, sin la defensa del autor) · la rama alternativa vive en un worktree aislado, no pisa el intento original y gana por checks · un cambio posterior vence un check · Ctrl-C cancela, deja la sesión `interrupted` y `resume` retoma el mismo id nativo · arranque, primer evento, TTFT y total se miden por separado (un evento inmediato no cuenta como TTFT) · web activa por defecto y desactivable · los MCP del usuario no se cargan · tarjeta de permiso (n no ejecuta, y sí), goal en dos turnos hasta `GOAL: met`, `/undo` que restaura los archivos, medidor de contexto, y lo que ve una persona en una pantalla de 60 columnas, verificado sobre un emulador VT (`tests/vt.py`, `tests/pty_render.py`): markdown de párrafos largos ajustado por palabra, ✗ en la herramienta correcta entre varias en paralelo, frase sin cortar tras una pausa, diff de Edit, autocompletado fuzzy con skills de plugins y tres turnos en un solo proceso `claude`.

**Medido con Opus 5.5 real** (una corrida, tarea `bug-duration`, modo adaptive):

| métrica | valor |
|---|---|
| arranque local (`qed --version` / REPL hasta el prompt) | ~11 ms / ~15 ms (30 corridas, incluye el overhead de lanzar el proceso) |
| spawn de `claude` | 6 ms |
| primer evento de Claude Code (init) | 604 ms |
| primer token del modelo (TTFT) | 3.2 s |
| total | 31.8 s, 1 llamada, $0.09 |
| resultado | repro registrada fallando en el original → fix → pasa; verificación del harness OK; **tests ocultos OK** |

**Pilotos locales medidos** (Opus 5.5, esfuerzo alto, 1 corrida por celda; `bench/runs/pilot1`, `pilot2`; `python3 bench/analyze.py DIR`):

| variante | resueltas (9 tareas) | costo total | mediana |
|---|---|---|---|
| A — Claude Code nativo | 9/9 | $1.65 | 36s |
| P — nativo + prompt de protocolo | 9/9 | $2.22 | 38s |
| C — min | 9/9 | $1.68 | 32s |
| D — adaptive | 9/9 | $1.70 | 34s |

- **No hay mejora de capacidad medible en tareas locales:** Opus 5.5 las resuelve todas en cualquier variante, incluso semver contra 896 casos de node-semver. C y D no degradan las tareas cortas (D nunca escaló) y cuestan lo mismo que el nativo.
- El prompt de protocolo solo (P), sin el loop, cuesta un 35% más y no compra aciertos.
- Claude Code nativo ya corre los tests en 5/5 tareas. Lo que cambia con el protocolo es reproducir antes de arreglar: 1/5 en A contra 2-3/5 en P/C/D (heurística).

**SWE-bench Verified, subset difícil** (10 instancias "1-4 hours" de 6 repos, Opus 5.5, sin web, 2400s/$10 por corrida, puntaje con el harness oficial; `bench/runs/swe*`):

```bash
python3 bench/analyze.py bench/runs/swe1-fix:C=P1,D=P1b bench/runs/swe1r-fix bench/runs/swe2-fix:C=C2,D=D2 \
  bench/runs/swe2r-fix:C=C2,D=D2 bench/runs/swe3-fix:C=C3,D=D3 bench/runs/swe4 --budget-s 2400
```

| variante | resueltas | costo total | mediana |
|---|---|---|---|
| A — Claude Code nativo, esfuerzo high | 7/10 | $3.75 | 206s |
| **P@medium — protocolo + esfuerzo medium (default actual)** | **8/10** | **$2.24** | **88s** |
| P — protocolo, high (2 brazos idénticos) | 8/10 · 9/10 | $3.41 · $3.43 | 131s |
| C2 / C3 — + verificación del check del modelo | 7/10 · 6/10 | $5.23 · $4.09 | 120s |
| D2 / D3 — + crítico y rama alternativa | 8/10 · 8/10 | $6.59 · $3.74 | 115s |
| DEEP3@medium — 3 candidatos + selección por ejecución cruzada | 8/10 | $7.23 | 243s |

- **Mejora medida de costo y velocidad.** P@medium fue más rápido que Claude Code nativo en **10/10** instancias (test de signo p=0.002) y más barato en **9/10** (p=0.02), resolviendo 8/10 contra 7/10. Por eso el default es `min` con esfuerzo `medium`.
- **Mejora de capacidad: no demostrada.** Las diferencias de aciertos están dentro del ruido: n=10, y dos brazos idénticos difieren en una tarea.
- **Modo profundo (`--candidates 3`):**
  - Resolvió sphinx-11510, que ninguna otra variante resolvió, porque la selección eligió el único fix completo (4/4 checks discriminantes contra 2/4).
  - Perdió dos instancias por empates: todos los candidatos satisfacían todos los tests y la selección no tenía cómo distinguirlos.
  - Gana solo gastando más (~3× P@medium), así que según la regla es un **modo profundo**, opcional.
  - Margen pendiente: juntando variantes se resuelven 10/10. La selección es el cuello de botella, como dice la literatura.
- **Lo que no sirve:** el loop de verificación del check del modelo y el crítico no suman aciertos y encarecen.
- **Corrección:** una primera versión de esta tabla estaba mal.
  - Un bug del evaluador (diffs recortados) dejó 30/73 corridas sin evaluar.
  - Se corrigió con `store::git_raw` y un test de regresión. Los diffs guardados se repararon de forma exacta, se validaron con `git apply --check` y se re-puntuaron (`bench/swebench/rescore.py`).
- **Incidentes controlados:** corridas que atravesaron un sueño de la Mac (re-corridas, y ahora se detectan solas), C/D v1 que no verificaban, y un rebuild a mitad de corrida (binarios congelados desde entonces).

## Evaluador

`qed eval TAREAS` copia cada repo a un directorio temporal, hace un commit base determinista, corre `setup`, ejecuta la variante con el presupuesto de la tarea, guarda **un solo diff final** y recién después copia `hidden/` y corre `hidden_cmd`. Las corridas se intercalan rotando el orden de las variantes por tarea y por repetición. Para cada corrida guarda trazas y evidencia en `bench/runs/<ts>/`, y deja `results.jsonl`, `summary.md` (resueltas por categoría, mediana y p95, $ por resuelta incluyendo los fallos, llamadas, escalamientos) y `env.json` (modelo, esfuerzo, versión de claude, commit de qed). Una corrida cuya traza toca los tests ocultos o una URL listada en `forbidden_urls` queda marcada como contaminada y no cuenta como resuelta.

Variantes: **A** Claude Code nativo (mismas herramientas, modelo, esfuerzo y config lean) · **B** A más un reintento desde cero si el check visible falla, dentro del mismo presupuesto · **C** `min` (protocolo + evidencia + verificación) · **D** `adaptive` · ablaciones **D-nocritic** y **D-noalt**. Todas las variantes reciben el mismo texto de tarea, más el check visible.

Tareas piloto en `bench/tasks/`, validadas: los tests ocultos fallan sobre el original y pasan con una solución de referencia que no está en el repo.

| tarea | categoría | qué discrimina |
|---|---|---|
| short-slug | short | feature chica |
| bug-duration | bug | el fix ingenuo deja pasar entradas inválidas |
| bug-paginate | bug | reporte de usuario, off-by-one |
| long-ttlcache | long | spec larga, casos borde de LRU/TTL |
| docs-zod3 | docs (web) | zod **3.22.4** en el lockfile: el código estilo v4 (`z.email()`, `{error}`) falla |

**SWE-bench Verified** (`bench/swebench/`):
- `prepare.py` elige instancias por repo, clona un caché y genera las tareas. El agente solo recibe el enunciado del issue: sin hints, sin el patch gold, sin los tests.
- `./.qenv <cmd>` corre cualquier comando dentro de la imagen oficial x86 de la instancia (Rosetta vía OrbStack), con los cambios actuales aplicados.
- `score.py` puntúa el diff final con el harness oficial (`swebench` 4.1, Python x86_64). Validado: el patch gold da resuelto y uno vacío no.
- `run.sh` baja la imagen, corre las variantes alternando el orden y borra la imagen al terminar.
- La web está desactivada para que el agente no recupere el fix upstream. Las corridas con error de API o cuota se excluyen, y si la cuota llega al 98% la evaluación se detiene.

```bash
bench/swebench/.venv/bin/python bench/swebench/prepare.py 'django/django=4,sympy/sympy=2'
bench/swebench/run.sh bench/runs/swe1 A,C,D <instance_id>...
python3 bench/analyze.py bench/runs/swe1     # comparación pareada + test de McNemar
```

Corrida local sugerida (~5 tareas × 6 variantes × 3 repeticiones = 90 corridas):
```bash
qed eval bench/tasks --variants A,B,C,D,D-nocritic,D-noalt --repeat 3
```
**Regla de decisión**: si D resuelve más que A y B con el mismo presupuesto sin degradar las tareas cortas, es una mejora del harness. Si solo gana gastando más, es un modo profundo. Si gana C y D no suma, quedate con C (`--mode min`).

## Límites conocidos

- El deny-list de credenciales (`.env`, `~/.ssh`, `~/.aws`, `printenv`) es por patrones de Claude Code, no es un sandbox. Que "una página no autoriza comandos" queda como instrucción del protocolo y no se hace cumplir.
- La señal "dos hipótesis predicen comportamientos distintos" no se detecta automáticamente.
- Si las dos ramas pasan todos los checks, no se busca un test que las distinga: gana la original.
- Los tests ocultos están en disco en este repo. La detección de contaminación es heurística (rutas en los inputs de herramientas).
- La rama alternativa no copia los directorios ignorados (`node_modules`, `.venv`): para eso está `setup`.
