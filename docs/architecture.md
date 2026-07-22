# Architecture

TestHarbor is a local application with two interfaces over the same workflow and storage model.

```text
Desktop (React -> Tauri) ----+
                             +-> application -> domain
CLI -------------------------+        |
                                      +-> storage / Git / worktree
                                      +-> provider adapters
                                      +-> process supervisor / launcher
                                      +-> test drivers / review / evidence
```

## Layer responsibilities

- `crates/domain` contains workflow states, transitions, guards, immutable snapshot types, hashes, and decisions. It performs no I/O.
- `crates/application` owns use-case orchestration and transaction boundaries.
- Adapter crates handle SQLite, Git objects and worktrees, path checks, provider CLIs, supervised processes, test result parsing, and evidence storage.
- `apps/desktop/src-tauri` converts Tauri commands and DTOs without duplicating business guards.
- `apps/desktop/src` and `apps/cli` present actions returned by the application layer.

## Core invariants

- A Run Snapshot binds the base commit, Test Intent, profiles, command identities, policy, and trust scope. Configuration changes create a successor rather than mutating the snapshot.
- Candidate generation and formal execution use isolated worktrees.
- Approval and execution evidence remain bound to the revision and relevant hashes that produced them.
- An apply action re-evaluates repository cleanliness, HEAD, scope, and candidate identity immediately before changing the main repository.
- UI availability and command submission use the same domain guard evaluator.
- Agent output is advisory. Deterministic checks and required human decisions cannot be replaced by an agent response.

## Concurrency

The desktop app and CLI can operate on multiple runs. A shared SQLite-backed lease coordinates foreground and service process slots across processes. The default global limit is two OS processes; each run can own at most one service slot and one foreground slot.

The first supported process launcher uses Unix process groups. Windows process semantics are not implemented yet.
