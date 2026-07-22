# Contributing

Thank you for contributing to TestHarbor. The project is in early preview, so small, well-tested changes are preferred over broad compatibility layers or speculative abstractions.

## Before opening a change

- Search existing issues and discussions.
- For a significant workflow, schema, security-boundary, or public API change, open an issue first.
- Never include proprietary repository content, provider transcripts, credentials, local databases, generated evidence, or machine-specific paths.
- Keep domain guards in `crates/domain`; do not reproduce business decisions in React, CLI parsing, or Tauri commands.

## Development

Prerequisites are Rust 1.88 or newer, Node.js 22.12 or newer, npm, Git, and macOS for the supported desktop packaging path.

```bash
cargo test --workspace
cargo check -p testharbor-desktop

cd apps/desktop
npm ci
npm run build
```

Run formatting and lint checks before submitting:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
```

See [Testing](./docs/testing.md) for test scope and public-export checks.

## Pull requests

- Keep each pull request focused on one outcome.
- Add or update tests for behavior changes.
- Describe user-visible behavior, security implications, and verification performed.
- Update English and Simplified Chinese user-facing strings together.
- Preserve immutable snapshots, transaction boundaries, TrustGrant checks, state-machine transitions, and human decision requirements.
- Do not commit generated sidecars, build output, local state, or internal planning documents.

All commits must include a Developer Certificate of Origin sign-off:

```bash
git commit -s -m "Describe the change"
```

The sign-off certifies that you have the right to submit the contribution under this repository's license. See [developercertificate.org](https://developercertificate.org/).
