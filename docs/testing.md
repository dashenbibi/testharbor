# Testing

Run the checks relevant to a change before opening a pull request.

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo check -p testharbor-desktop

cd apps/desktop
npm ci
npm run build
```

## Test areas

- Domain tests cover transitions, guards, snapshot hashes, decisions, flaky evidence, and stop/end behavior.
- Adapter tests use temporary repositories and data directories for path, Git, storage, evidence, trust, and process behavior.
- Provider contract tests use explicit mocks; normal runtime must not silently fall back to Mock.
- Go Test JSON integration tests cover inventory, intended tests, complete formal execution, and AcceptPass requirements.
- Frontend tests should cover localized labels, intent/profile forms, action descriptors, and concurrent run activity.

Real-provider dogfooding is a release activity because it depends on locally installed CLIs and authenticated accounts. Do not put credentials, account output, repository content, or unredacted evidence into issues or CI logs.

## Public source check

Create and validate a clean source export before a release:

```bash
./scripts/export-open-source.sh /tmp/testharbor-public
```

The destination must not already exist. The export excludes internal planning documents, nested Git metadata, local state, generated sidecars, and build output.
