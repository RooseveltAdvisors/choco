# Choco

Choco is a fast public Rust TUI task board that any agent, on any harness, can use.

## Who it is for

Humans and agents. Claude, Cursor, Codex, Pi, a person in a terminal, or anything that can run a small CLI or edit the canonical Markdown file.

A local `--file` path is an install choice, not product ownership.

## What it is

One Markdown file is the store: each top-level `# ` heading is a task and its
following content is the body. Choco also supports its original versioned JSON
format for legacy boards, but it is not involved when Choco opens the canonical
Markdown file.

Agents participate by reading and editing that file, or by calling:

```
choco --file todo.md post --channel general "Investigate the flaky test"
choco --file todo.md reply markdown-1 "I found the cause"
```

The file is the contract. There is no plugin, no database, and no hosted backend.

## What it is not

Not a second store or sync layer. Not a database. Not a cloud service. Not tied
to one agent harness.

## How it feels

Nvim keys move around the board. The user's real editor (nvim when `$EDITOR` is
unset) writes targeted changes directly to the selected file.

Compose a new task or a reply in the editor: visible cursor, vim motions, and
return to the board with the text saved. Quit without write to discard.

Press Enter on a task to edit it in the editor. A newly written task is
selected and shown when the editor closes. External edits are re-read and
detected before Choco saves.

Do not fake a partial vim layer inside the TUI when the person asked for their editor.

## Success

Two agents on different harnesses can share one board file with no Choco plugin.

A person can press reply, land in their editor, edit with a cursor and motions, and return to the board with the text saved.

A person can press Enter on a task, update it in their editor, and return to the board with the task selected.
