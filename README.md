# Choco

Fast Rust TUI for agent tasks: channels on the left, tasks in the middle, and
task details on the right.

## Run

```sh
cargo run -- --file choco.json
```

Keys follow familiar vim muscle memory: `h`/`j`/`k`/`l` move and switch panes,
`gg`/`G` jump to the first/last item in the focused pane, `/` searches, and
`n`/`N` cycle matches.
`Ctrl-u`/`Ctrl-d` scroll task details, `Enter` edits the selected task, `Esc`
cancels search, and `q` quits. Outside an active search, `n` still launches the
editor for a new task and `r` launches it for a reply; `R` reloads.
The configured `$EDITOR` is used when set; otherwise nvim is launched. Writing
the editor buffer submits the text and returns to the board; quitting without
writing discards the draft.

Editing a task opens the editor with its title, body, and a preserved replies
section. Edit the title and body above the `--- Replies (preserved on save) ---`
marker; replies remain in the JSON thread when the task is saved.

Choco watches its board file. External changes reload when the TUI is idle.
Writes use a temporary file and atomic rename, so a watcher never sees a
partial board.

## Harness interface

The board is plain JSON, so any harness can read or write it without a Choco
integration. The CLI is also intentionally small:

```sh
choco --file choco.json post --channel general "Investigate the flaky test"
choco --file choco.json reply TASK_ID "I found the cause"
choco --file choco.json render --markdown /path/to/todo.md
```

`post` creates a missing channel automatically. `CHOCO_AUTHOR` sets the author
for replies and defaults to `$USER`. Choco serializes writers with a small
lock file and rejects unsupported versions or unknown fields instead of
silently dropping data.

`render` writes a markdown view of the JSON board atomically. Tasks remain
newest-first, and existing Firstmate stamp text is preserved on its task card.
The output path is explicit so Choco remains independent of any harness.

The file format is versioned and contains `channels`, `tasks`, each task's
`title`, `body`, and `replies`. The `body` field may be omitted when loading
older boards and defaults to empty. A minimal board is:

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
