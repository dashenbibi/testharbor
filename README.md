# TestHarbor

[简体中文](./README.zh-CN.md)

TestHarbor is an experimental, local-first test orchestration tool for repository-based software projects. It coordinates AI-assisted test generation, deterministic validation, human review, controlled execution, and explicit delivery from a desktop app or CLI.

> TestHarbor is an early preview. Use it on repositories you can restore, review every command and patch, and do not treat its process controls as an operating-system security sandbox.

## What it does

- Captures an immutable run snapshot containing the base commit, test intent, profiles, command identities, and trust scope.
- Generates candidate tests in an isolated Git worktree.
- Validates paths and candidate content before review and execution.
- Keeps agent review separate from deterministic checks and human approval.
- Runs structured test and service profiles through a supervised process launcher.
- Stores revisions, executions, decisions, logs, and artifacts as auditable history.
- Supports multiple runs from both the desktop app and CLI. Process slots are coordinated through the shared local database; the default global OS-process limit is two.
- Uses English by default and supports Simplified Chinese in both interfaces.

## Current capability boundary

| Area | Current support |
| --- | --- |
| Desktop | macOS-first Tauri 2 application |
| CLI | Local `testharbor` command sharing the desktop data directory |
| Providers | Codex CLI, Claude Code CLI, and Grok CLI after capability probing |
| Strict pass decision | Go Test JSON with stable inventory and intended-test observation |
| Other results | JUnit and generic exit-code evidence can be reviewed, but do not receive the same strict pass guarantee |
| Process isolation | Unix process groups, timeouts, cancellation, concurrency limits; no VM/container sandbox |
| Delivery | Explicit export or guarded apply after human acceptance |

Providers are never silently replaced with Mock in normal operation. Mock is reserved for tests and explicit local dogfooding.

## Security and privacy

TestHarbor runs user-approved local executables such as Git, provider CLIs, test tools, and service commands. Those programs execute with the current user's operating-system permissions and may access the network. Provider requests can contain repository-derived intent, attachments, diffs, validation results, and test evidence.

Read [Security model](./docs/security-model.md) and [Provider data flow](./docs/provider-data-flow.md) before using TestHarbor with confidential code. Vulnerabilities should be reported according to [SECURITY.md](./SECURITY.md).

## Prerequisites

- macOS for the supported desktop release path
- Rust 1.88 or newer
- Node.js 22.12 or newer and npm
- Git
- A supported provider CLI, authenticated separately, when using a real provider
- The project-specific test tools selected in the Test Profile

## Build and test

```bash
cargo test --workspace
cargo check -p testharbor-desktop

cd apps/desktop
npm ci
npm run build
npm run tauri dev
```

The desktop bundle embeds a target-specific process launcher built by `apps/desktop/scripts/prepare-sidecar.sh`:

```bash
cd apps/desktop
npm run tauri:bundle
```

See [Release packaging](./docs/releasing.md) for GitHub-hosted macOS preview builds and signed draft releases.

## CLI quick start

```bash
cargo build -p testharbor-process-launcher -p testharbor-cli
./target/debug/testharbor --help

testharbor projects open /path/to/repository
testharbor runs create --project-id <project-id>
testharbor runs list
```

Use `--json` for automation. See [CLI documentation](./apps/cli/README.md) for the complete flow.

## Language

```bash
testharbor config language en
testharbor config language zh-CN
testharbor --language en info
```

The preference is persisted in the shared local application settings and can also be changed from the desktop Settings dialog.

## Architecture

```text
React UI -> Tauri commands -> application -> domain / adapters
```

Domain states and guards are shared by the desktop app and CLI. See [Architecture](./docs/architecture.md), [Testing](./docs/testing.md), [Roadmap](./docs/roadmap.md), and the [Public release checklist](./docs/release-checklist.md).

## Contributing

Contributions are welcome. Start with [CONTRIBUTING.md](./CONTRIBUTING.md) and follow the [Code of Conduct](./CODE_OF_CONDUCT.md).

TestHarbor is available under the [MIT License](./LICENSE). Provider and product names belong to their respective owners; this project is not affiliated with or sponsored by OpenAI, Anthropic, xAI, or other provider vendors.
