# Verifier rollout reference

## Wave 1 pilot: choco

- `/verify` is the task-proof layer; existing Cargo and no-mistakes gates stay authoritative.
- Use `scripts/dev-local.sh up` for one idempotent tmux TUI and a scratch Markdown fixture.
- PR #7 proof passed independently: slash-search narrowed to two candidates, labeled them
  `a/s`, selected `Search beta` with `s`, and showed `/search [s] selected`.
- Evidence: `evidence/pr7-slash-search-{live,labeled,selected}.txt` (ignored locally).

## Wave 2 draft: portal

- Wrap the existing `app.sh` and Docker Compose stack; verify only against the dev stack.
- Inventory-adapt `PortalAccess` and `PortalVisualQA`; retain existing pytest/Playwright regression.
- Every brief names synthetic test-patient data, no production PHI, and no deployment verification.
- Browser proof embeds a screenshot and links video; deployment proof is a separate post-merge step.

## AGT Linux verification lab (draft, 2026-08-30)

Read-only SSH probe from the GPU host: AGT-8 reachable; AGT-3..7 and AGT-9..15 did not answer.
Fleet OS/tooling baseline comes from the canonical provisioning guide: Ubuntu 24.04 hosts use
`apt` for git, tmux, curl, jq, build-essential, and stow, then Bun, uv, gh, and Rust/cargo;
AGT-8 is macOS and uses Homebrew (tmux was present; cargo was absent in this probe).

| Hosts | Distro | Registration gate |
|---|---|---|
| AGT-3..7, AGT-9..15 | Ubuntu 24.04 | SSH + `uname` + `git` + `tmux` + `cargo --version` |
| AGT-8 | macOS 26.3.2 | SSH + `uname` + `git` + `tmux`; install Rust before Choco |

Re-probe each host immediately before fleet registration; do not infer reachability from stale
inventory status. The external rollout checklist remains the fleet authority.
