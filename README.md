# qed ∎

A small, fast harness around the official Claude Code CLI. It makes the agent show its work.

qed runs `claude -p` with your normal Claude Code login and adds three things. It loads a much smaller context. It records evidence: every check is tied to the exact code it ran on. And it puts a quiet terminal UI on top. It's a 1.1 MB Rust binary with two dependencies, and the prompt is ready in about 10 ms.

## Quick start

```bash
curl -fsSL https://qed.pages.dev/install | sh
# prebuilt binary for macOS/Linux (x86_64, arm64) into ~/.local/bin; falls back to cargo.
# needs the `claude` CLI, logged in. Options: QED_VERSION=v0.2.0, QED_INSTALL_DIR=/usr/local/bin
# or: cargo install --git https://github.com/agusgarcia3007/qed --locked

qed                                   # interactive
qed "fix the pagination bug" --check "pytest -q"   # a session that starts with this task
qed -p "..."                          # headless: one JSON event per line
qed resume                            # pick up a past session
qed status                            # which checks still hold on the current code
qed update                            # install the latest release (alias: qed upgrade)
```

The REPL also checks for a new release in the background (at most hourly), installs it, and shows `↑ <version> /restart`. `/restart` reopens qed on the same session. Turn it off with `QED_NO_UPDATE=1` or `"autoupdate": false` in `.qed/config.json`. Updates are checked against the release's `SHA256SUMS`, and binaries built from a checkout are never replaced.

## Why use it

**Less context, lower cost.** By default qed skips your user-level MCP servers, skills and settings, so Claude Code starts with only what the project needs. On my machine the prompt went from 268k to 9.5k tokens. A trivial "ok" went from $2.15 and 13.5 s to $0.03 and 3.3 s. `--full` brings everything back.

**Evidence instead of claims.** The model runs its tests through `qed check -- <cmd>`. The result is stored against a hash of the exact working tree, untracked files included. If anything changes afterwards, the check shows as stale. For bugs, the protocol asks for a reproduction that fails first and passes after the fix.

**Fast.** One Claude Code process stays alive across REPL turns. It is started while you type, so the first reply skips startup too.

## Benchmarks

**SWE-bench Verified, hard subset:** 10 instances rated "1–4 hours", from 6 repos. Opus 5.5, no web access, official scoring harness.

| Setup | Solved | Total cost | Median time |
|---|---|---|---|
| Claude Code, native, effort high | 7/10 | $3.75 | 206 s |
| **qed default** (protocol, effort medium) | **8/10** | **$2.24** | **88 s** |
| qed protocol, effort high (two identical runs) | 8/10 · 9/10 | $3.41 · $3.43 | 131 s |
| qed + critic and alternative branch | 8/10 | $3.74–6.59 | 115 s |
| qed deep mode (3 candidates, cross-checked) | 8/10 | $7.23 | 243 s |

- **Speed and cost improved, and that holds up statistically.** The default was faster than native Claude Code on 10 of 10 instances (sign test p = 0.002) and cheaper on 9 of 10 (p = 0.02).
- **Better solve rate: not shown.** 8/10 vs 7/10 is within noise. Two identical runs differed by one task.
- **Heavier machinery didn't pay off.** The critic and the alternative branch cost more and solved no more, so they are opt-in (`--mode adaptive`). Deep mode solved one instance nothing else did, but lost two to ties, at about 3× the cost.

**Local tasks:** 9 small bug, feature and docs tasks with hidden tests. Every setup solved all 9, native Claude Code included, at about the same cost. qed didn't make short tasks worse.

These are small samples. Treat them as a pilot, not a verdict. The tasks and scripts are in [`bench/`](bench). `qed eval bench/tasks --variants A,C` runs the local set.

## In the REPL

- **Esc** cancels the turn, and the session stays resumable. Anything you type while the agent works is queued.
- Long input wraps into rows under the prompt as you type. `\` + Enter adds a new line.
- When a background task Claude Code started finishes between turns, its reply is shown before your next message is answered, so answers never shift by one turn.
- The status line shows model, effort, cost, and **context in use**, for example `ctx 23k/200k (12%)`. The spinner updates it live during a turn.
- `/goal <objective>` keeps taking turns until the model reports the goal met (up to 5).
- `/undo` puts the files back as they were before the last turn.
- `/subagents on` (or `--subagents`, or `"subagents": true` in config) offers Claude Code's subagents: the built-in Explore, general-purpose and Plan agents, plus the project's `.claude/agents`. Off by default, because the tool adds about 2.3k prompt tokens to every request. When it's on, qed runs subagents inline rather than in the background, so their report comes back within the same turn. The protocol limits them to broad exploration: no delegated edits, and a subagent's report doesn't count as evidence. Checks they run are still recorded. Measure it on your tasks with `qed eval … --variants C,C+sub`.
- `/perm ask` asks before shell commands and web fetches. Edits and `qed check` still run on their own.
- Also available: `/check`, `/model`, `/effort`, `/diff`, `/cost`, `/resume`, `!cmd` for the shell, and your skills via `/name`. `/help` lists them all.

Optional project config lives in `.qed/config.json`. It takes the same keys as the flags: `check`, `setup`, `model`, `effort`, `budget_s`, `budget_usd`, …

## Limits

- The credential deny-list (`.env`, `~/.ssh`, `~/.aws`) uses Claude Code's permission patterns. It is not a sandbox.
- Checks are only as good as the commands behind them. qed records what ran; it doesn't judge whether the test was meaningful.
- Results above come from one model on a few dozen runs.

## Development

```bash
cargo test     # unit + integration tests against a deterministic fake `claude`, incl. a real PTY
```

Detailed design notes and the full benchmark log (in Spanish) are in [`docs/notes.es.md`](docs/notes.es.md).

MIT licensed.
