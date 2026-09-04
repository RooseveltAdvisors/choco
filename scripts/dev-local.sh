#!/usr/bin/env bash
set -euo pipefail

SESSION=choco-dev
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FIXTURE="${CHOCO_TODO_FIXTURE:-${TMPDIR:-/tmp}/choco-verifier-todo.md}"
TARGET="$SESSION:tui"

die() { printf '✗ %s\n' "$*" >&2; exit 1; }
preflight() {
  command -v tmux >/dev/null || die 'tmux is required'
  command -v cargo >/dev/null || die 'cargo is required'
  [ "$FIXTURE" != /opt/ra/firstmate/data/todo.md ] || die 'refusing the canonical todo.md'
}
ensure_fixture() {
  [ -e "$FIXTURE" ] && return
  mkdir -p "$(dirname "$FIXTURE")"
  printf '%s\n' '# Search alpha' '' 'Matches the slash search flow.' '' '# Search beta' '' 'Second labeled candidate.' '' '# Unrelated task' '' 'Should disappear when filtering.' >"$FIXTURE"
}
cmd_up() {
  preflight; ensure_fixture
  tmux has-session -t "$SESSION" 2>/dev/null || tmux new-session -d -s "$SESSION" -n tui -c "$ROOT"
  if ! tmux list-windows -t "$SESSION" -F '#{window_name}' | grep -qx tui; then
    tmux new-window -t "$SESSION" -n tui -c "$ROOT"
  fi
  if ! tmux list-panes -t "$TARGET" -F '#{pane_current_command}' | grep -qxE 'cargo|choco|rustc'; then
    printf -v command 'cargo run --quiet -- --file %q' "$FIXTURE"
    tmux send-keys -t "$TARGET" "$command" C-m
  fi
  printf '✓ tmux session %s is up (fixture %s)\n' "$SESSION" "$FIXTURE"
}
cmd_status() {
  tmux has-session -t "$SESSION" 2>/dev/null || { printf 'session %s is down\n' "$SESSION"; return; }
  tmux list-windows -t "$SESSION" -F 'window #{window_index}: #{window_name} (#{window_panes} pane)'
}
cmd_logs() { tmux capture-pane -p -S -200 -t "$TARGET"; }
cmd_restart() { "$0" down; "$0" up; }
cmd_down() { tmux kill-session -t "$SESSION" 2>/dev/null && printf '✓ stopped %s\n' "$SESSION" || printf 'session %s is down\n' "$SESSION"; }
cmd_attach() { tmux attach -t "$SESSION"; }

case "${1:-up}" in
  up) up="$(cmd_up)"; printf '%s\n' "$up" ;;
  status) cmd_status ;;
  logs) cmd_logs ;;
  restart) cmd_restart ;;
  down) cmd_down ;;
  attach) cmd_attach ;;
  -h|--help|help) sed -n '2,10p' "$0" ;;
  *) die "unknown command: $1 (use up|down|status|logs|restart|attach)" ;;
esac
