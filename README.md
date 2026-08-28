# Choco

Fast Rust TUI for agent tasks: channels on the left, tasks in the middle, and
task details on the right.

## Run

```sh
cargo run -- --file /path/to/todo.md
```

The Markdown file is the canonical store. A legacy JSON board can still be
opened explicitly with `--file choco.json`.

Keys follow familiar vim muscle memory: `h`/`j`/`k`/`l` move and switch panes,
`gg`/`G` jump to the first/last item in the focused pane, and `/` opens a live,
Flash-style search. Type to filter candidates, press `Enter`, then press a
candidate's letter to jump to it. `n`/`N` cycle matches afterward.
`Ctrl-u`/`Ctrl-d` scroll task details, `Enter` edits the selected task, `Esc`
cancels search, and `q` quits. Outside an active search, `n` still launches the
editor for a new task, `r` launches it for a reply, and `a` archives the
selected task; `R` reloads.
These controls work directly against the selected store. Markdown edits are
targeted, atomic updates to the source file.
If the source changes after loading, Choco refuses the write rather than
overwriting newer content.
The configured `$EDITOR` is used when set; otherwise nvim is launched. Writing
the editor buffer submits the text and returns to the board; quitting without
writing discards the draft.

Editing a task opens the editor with its title and body. On JSON stores, the
`--- Replies (preserved on save) ---` marker keeps replies in the JSON thread.
On Markdown stores, saving changes only the selected card and preserves the
rest of the document.

Choco watches its board file. External changes reload when the TUI is idle.
Writes use a temporary file and atomic rename, so a watcher never sees a
partial board.

## Harness interface

The board can be plain JSON, so any harness can read or write it without a
Choco integration. A `.md` `--file` is also supported: each top-level `# `
heading is a task, its following content is the body, and cards are shown in
file order. Markdown create, edit, and reply operations write back to that
same file without a Choco-specific sync layer. The `a` archive shortcut moves a
Markdown card to the sibling `todo-done.md` file. The CLI is intentionally
small:

```sh
choco --file /path/to/todo.md post --channel general "Investigate the flaky test"
choco --file /path/to/todo.md reply markdown-1 "I found the cause"
choco --file choco.json render --markdown /path/to/todo.md
```

On JSON stores, `post` creates a missing channel automatically. Markdown stores
support only the `general` channel. `CHOCO_AUTHOR` sets the author for replies
and defaults to `$USER`. Choco serializes writers with a small lock file and
rejects unsupported versions or unknown fields instead of silently dropping
data.

`render` writes a markdown view of the JSON board atomically. Tasks remain
newest-first, and existing Firstmate stamp text is preserved on its task card.
The output path is explicit. Markdown files supplied as `--file` are read
directly rather than rendered through a JSON side store.

The file format is versioned and contains `channels`, `tasks`, each task's
`title`, `body`, and `replies`, plus the `task_order` value
`"newest_first"` written by current Choco versions. Boards without
`task_order` are treated as newest-first when loaded. The `body` field
may be omitted when loading older boards and defaults to empty. A minimal
board is:

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

Greptile Apps reviews pull requests using the standard `.greptile/config.json`
status-check setting.
