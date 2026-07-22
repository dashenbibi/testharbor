# TestHarbor CLI

[简体中文](./README.zh-CN.md)

`testharbor` exposes the same application workflow and local data directory as the desktop app. It uses the same state transitions, guards, worktrees, trust decisions, execution evidence, and delivery checks.

## Build

```bash
cargo build -p testharbor-process-launcher -p testharbor-cli
./target/debug/testharbor --help
```

For a local release build, keep `testharbor` and `testharbor-process-launcher` in the same directory:

```bash
cargo build --release -p testharbor-process-launcher -p testharbor-cli
./target/release/testharbor --help
```

The CLI uses the desktop data directory by default. Use an isolated directory for CI or disposable testing:

```bash
testharbor --data-dir /tmp/testharbor-data info
```

Add `--json` when consuming the stable machine-readable output available for a command.

## Start a run

Interactive initialization:

```bash
testharbor init /path/to/repository
testharbor init /path/to/repository --generate
```

Explicit workflow:

```bash
testharbor projects open /path/to/repository
testharbor runs create --project-id <project-id>

testharbor generate <run-id> \
  --prompt 'Verify the service release flow' \
  --criteria 'The released version can be queried' \
  --environment 'The local service is already running; test account: ...; MySQL and Redis use the repository config.' \
  --follow

testharbor review show <run-id>
testharbor review approve <run-id>
testharbor execute start <run-id> --wait
testharbor result accept-pass <run-id>
testharbor apply guard <run-id>
testharbor apply export <run-id> --complete
```

Opening a project no longer creates a placeholder run. Use `runs create` explicitly.

## Providers and runtime inputs

Select a provider with `--agent codex|claude|grok` and an optional provider model with `--model <cli-model-id>`. Provider capabilities and models are discovered through the installed CLI:

```bash
testharbor models codex
testharbor models claude
testharbor models grok
testharbor config show
testharbor config agents --enable codex --enable claude --enable grok
```

Runtime secrets use `--var KEY` to inherit the value from the current process environment. Do not put secret values in arguments, intent, attachments, or profile files. Free-form service startup and shutdown shell strings are not accepted; services use a confirmed structured Service Profile.

The normal generation input is intentionally limited to the test prompt, acceptance criteria, and optional environment details. TestHarbor/Agent discovers code anchors, affected files, service ports, authentication flow, and test boundaries from the repository and its configuration. Legacy anchor, scope, attachment, and profile commands remain available only for compatibility and are hidden from the normal help output.

## Multiple runs

Separate CLI processes can operate on different runs concurrently. The CLI and desktop app coordinate process leases through the shared SQLite database, so they respect the same global limit. Use `runs list`, `execute poll`, and `execute cancel` to inspect or stop work without relying on one long-lived terminal.

## Language

English is the default. The persisted preference is shared with the desktop app:

```bash
testharbor config language en
testharbor config language zh-CN
testharbor --language en info
```

See the repository [security model](../../docs/security-model.md) before running TestHarbor on confidential code.
