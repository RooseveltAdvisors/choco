---
name: verify
description: Prove a Choco change through its real TUI, then prepare proof for the PR.
user_invocable: true
---

# /verify — prove the task before the PR

This is the non-web verifier for Choco. It adds proof before the existing
`no-mistakes` review, test, lint, push, PR, and CI gates; never weaken those gates.

## Preconditions

Use a feature branch with committed changes. Run `scripts/dev-local.sh up` once.
The app is a real TUI in tmux (`choco-dev:tui`), not a mock or browser. The
default fixture is scratch data under `/tmp`; never use Firstmate's canonical board.

## Verify the task with a fresh read-only verifier

Delegate this brief without the implementer's context:

> Drive the running Choco TUI independently. For PR #7's slash-search flow,
> press `/`, type a query, confirm live candidates narrow and receive letter
> labels, press `Enter`, select a labeled candidate, and confirm the selected
> task is visible. Capture the pane with `tmux capture-pane` under `evidence/`.
> Return exactly `TASK: works | broken`, expected, observed, and evidence paths.
> Do not edit code. For non-web Choco, pane stdout is the proof; no browser or
> video is required.

If the verdict is `broken`, fix the implementation and use a fresh verifier;
cap the loop at three rounds, then escalate. Never self-certify.

## Regression and PR proof

After `works`, run `cargo fmt --check && cargo clippy -- -D warnings && cargo test`.
Keep evidence ignored locally, upload the pane capture to the public
`pr-evidence` release, and put the capture inline in the PR body with a stable
link plus reproduce steps. Open the PR only after the verifier verdict and
regression sweep. Then run `no-mistakes axi run` with the complete task intent;
its gates remain authoritative.
