# Choco

Choco is a fast public Rust TUI task board that any agent, on any harness, can use.

## Who it is for

Humans and agents. Claude, Cursor, Codex, Pi, a person in a terminal, or anything that can run a small CLI or edit a Markdown file.

A local `--file` path is an install choice, not product ownership.

## What it is

One Markdown file can be the store: each top-level `#` heading is a task and
its following content is the body. Choco also supports its original versioned
JSON format for existing boards and write-oriented harnesses.

Agents participate by reading that file, or by calling:

```
choco --file board.json post --channel general "Investigate the flaky test"
choco --file board.json reply TASK_ID "I found the cause"
```

The file is the contract. There is no plugin, no database, and no hosted backend.

## What it is not

Not a second store for the active Markdown board. Not a database. Not a cloud
service. Not tied to one agent harness.

## How it feels

Nvim keys move around the board. On JSON stores, the user's real editor (nvim
when `$EDITOR` is unset) writes the text. Markdown stores are currently
read-only.

On JSON stores, compose a new task or a reply in the editor: visible cursor,
vim motions, the captain's real editor. Write to submit. Quit without write to
discard.

Press Enter on a task in a JSON store to edit it in the editor. A newly written
task is selected and shown when the editor closes.

Do not fake a partial vim layer inside the TUI when the person asked for their editor.

## Success

Two agents on different harnesses can share one board file with no Choco plugin.

A person can press reply, land in their editor, edit with a cursor and motions, and return to the board with the text saved.

A person can press Enter on a task, update it in their editor, and return to the board with the task selected.
