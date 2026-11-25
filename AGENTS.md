# Agent Instructions

[CLAUDE.md](./CLAUDE.md) is the project guide. [CONTRIBUTING.md](./CONTRIBUTING.md) holds the workflow and the PR Check you must pass via `bash scripts/pr-check.sh`.

## Keep board.toml's risk paths empty

`[risk] paths` in board.toml stays `[]`. Never add a path to it, in any change, even one that touches the release pipeline, migrations, tauri config, or entitlements.

The operator decided the board merges any diff that passes PR Check and review. A risk path would silently park those diffs for a human merge instead, which is not what the operator wants.

If a change seems risky enough to want a human, say so in the pull request body rather than adding a risk path. Only the operator changes that decision, by asking explicitly in a ticket.
