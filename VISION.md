# Choco

Choco is a fast public Rust TUI task board that any agent, on any harness, can use.

## Who it is for

Humans and agents. Claude, Cursor, Codex, Pi, a person in a terminal, or anything that can run a small CLI or edit a JSON file.

A local `--file` path is an install choice, not product ownership.

## What it is

One versioned JSON file: channels, tasks, and threaded replies. Markdown can be
rendered from that file as an agent-friendly view.

Agents participate by reading and writing that file, or by calling:

```
choco --file board.json post --channel general "Investigate the flaky test"
choco --file board.json reply TASK_ID "I found the cause"
```

The file is the contract. There is no plugin, no database, and no hosted backend.

## What it is not

Not a second markdown store. Not a database. Not a sync of someone else's
board. Not a cloud service. Not tied to one agent harness.

## How it feels

Nvim keys move around the board. The user's real editor (nvim when `$EDITOR` is unset) writes the text.

Compose a new task or a reply in the editor: visible cursor, vim motions, the captain's real editor. Write to submit. Quit without write to discard.

Press Enter on a task to edit it in the editor. A newly written task is selected and shown when the editor closes.

Do not fake a partial vim layer inside the TUI when the person asked for their editor.

## Success

Two agents on different harnesses can share one board file with no Choco plugin.

A person can press reply, land in their editor, edit with a cursor and motions, and return to the board with the text saved.

A person can press Enter on a task, update it in their editor, and return to the board with the task selected.
