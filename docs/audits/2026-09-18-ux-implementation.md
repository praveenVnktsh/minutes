# UX consolidation implementation

Audit: [findings and acceptance contracts](2026-09-18-app-ux-audit.md).
Architecture: [component dependencies](../plans/2026-09-18-app-ux-consolidation.md).

## Execution

The user authorized parallel Sol builders, individual PRs, and coordinator-reviewed merges on 2026-09-18. Builders use `foundryazure/gpt-5.6-sol`. The graph's original model-tier labels describe estimated complexity; this batch uses the user-selected Sol model for every builder.

All implementation PRs target `integration/ux-consolidation`. One final reviewed PR targets `main`, whose pushes trigger canary releases. `PR Check` covers both targets and integration pushes. Application builders do not merge PRs, change CI, or write another component's files without coordinator agreement.

| Component | Branch | Dependencies |
|---|---|---|
| c1 visual primitives | `enhance/ux-c1-visuals` | none |
| c2 committed configuration | `enhance/ux-c2-config` | none |
| c5 native activity contract | `enhance/ux-c5-native` | none |
| c4 note persistence | `enhance/ux-c4-notes` | c1 |
| c9 settings | `enhance/ux-c9-settings` | c1, c2 |
| c3 catalog/navigation | `enhance/ux-c3-catalog` | c4 |
| c8 export/clipboard | `enhance/ux-c8-export` | c4 |
| c6 activity owner | `enhance/ux-c6-activity` | c1, c2, c3, c4, c5 |
| c7 recording controller | `enhance/ux-c7-recording` | c1, c2, c3, c4, c5, c6 |
| c10 shell/library | `enhance/ux-c10-shell` | c1, c2, c3, c5, c6, c7, c8, c9 |
| c11 workspace | `enhance/ux-c11-workspace` | c1, c3, c4, c6, c7, c8, c9 |

Each PR must document its public interfaces, addressed audit IDs, tests actually run, and any integration handoff. Independent review checks behavior and ownership against the audit. The coordinator verifies the current combined revision before merging; a green historical head is not sufficient after updates.

At most three builders run concurrently; heavy local native builds are serialized. Each builder owns its worktree's frontend dependencies/build output. GitHub CI runs the complete verification sequence. Follow-up fixes stay with the component owner where possible.

## Completion evidence

Implementation PR URLs, review results, integrated test evidence, remaining desktop checks, and final release outcome will be recorded here as work lands. This file does not claim acceptance checks passed before they were executed.
