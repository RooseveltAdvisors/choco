# Choco

Fast Rust TUI for agent tasks: channels on the left, tasks in the middle, and
Slack-like threads on demand.

## Run

```sh
cargo run -- --file choco.json
```

Keys: `j`/`k` or arrows move, `h`/`l` or `Tab` switch panes, `Enter` opens a
thread, `Esc` closes it, `n` creates a task, `r` replies, `R` reloads, and `q`
quits.

Choco watches its board file. External changes reload when the TUI is idle. If
the file changes while a task or reply draft is open, the draft stays on
screen; submitting it merges into the latest file contents. Writes use a
temporary file and atomic rename, so a watcher never sees a partial board.

## Harness interface

The board is plain JSON, so any harness can read or write it without a Choco
integration. The CLI is also intentionally small:

```sh
choco --file choco.json post --channel general "Investigate the flaky test"
choco --file choco.json reply TASK_ID "I found the cause"
```

`post` creates a missing channel automatically. `CHOCO_AUTHOR` sets the author
for replies and defaults to `$USER`. Choco serializes writers with a small
lock file and rejects unsupported versions or unknown fields instead of
silently dropping data.

The file format is versioned and contains `channels`, `tasks`, and each task's
`replies`. A minimal board is:

```json
{"version":1,"channels":[{"id":"general","name":"general"}],"tasks":[]}
```

## Development

```sh
cargo test
cargo run -- post --channel general "First task"
```

## License

MIT
