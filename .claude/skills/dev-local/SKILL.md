---
name: dev-local
description: Start Choco's isolated local TUI fixture in tmux.
user_invocable: true
---

# /dev-local

Use `scripts/dev-local.sh up` before driving Choco. It starts one `choco-dev`
tmux window from a scratch Markdown board (default: `/tmp/choco-verifier-todo.md`).
Set `CHOCO_TODO_FIXTURE` to use another scratch path; never point it at the
canonical Firstmate todo board.

Use `status`, `logs`, `restart`, `attach`, or `down` for lifecycle control.
The launcher is idempotent and owns only its `choco-dev` session.
