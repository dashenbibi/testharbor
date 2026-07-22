# Security model

TestHarbor reduces accidental repository changes and keeps execution decisions auditable. It is not an operating-system sandbox and is not a security boundary against a malicious executable running as the current user.

## Trust boundary

The following programs are trusted local dependencies once the user approves their exact profile and identity:

- Git
- Codex CLI, Claude Code CLI, or Grok CLI
- project format, collect, compile, and test executables
- optional service executables
- the bundled TestHarbor process launcher

These processes inherit the current user's operating-system permissions. Profile path, environment, command, and network policy are application-level controls; they do not replace a VM, container, macOS sandbox profile, or host firewall.

## Controls provided by TestHarbor

- Immutable snapshot and command hashes bind approvals and evidence to a specific run context.
- Candidate generation occurs in an isolated Git worktree.
- Path checks reject policy violations, symlink changes, gitlinks, escapes, and unexpected candidate mutations.
- Provider adapters must pass capability probes; missing isolation capabilities make a provider unavailable rather than selecting a weaker fallback.
- Reviewer roles receive a read-only bundle through provider capabilities, but the provider process itself is still a locally trusted executable.
- Process groups, timeouts, cancellation, recovery identity, and shared concurrency leases limit accidental runaway execution.
- Runtime secret values are supplied through child environments and are not intentionally stored in argv or the database. Logs and reports are redacted before persistence.
- Evidence uses content-addressed finalization and reconciliation to detect partial, missing, or corrupt artifacts.
- Apply is a separate user action and rechecks the main repository immediately before delivery.

## Limitations

- No VM or container isolation is provided.
- Network access cannot be guaranteed to be blocked by TestHarbor alone.
- Redaction is best effort and must not be treated as a secret vault. Avoid placing long-lived credentials in test intent, attachments, source files, or provider prompts.
- A trusted tool can read data available to the current operating-system user.
- The initial launcher supports macOS/Unix process groups, not Windows job objects.
- Only the Go Test JSON driver currently supports the strict inventory-based AcceptPass path.

## Recommended use

- Start with a clean repository and a recoverable branch.
- Review every executable, argument, working directory, environment key, network policy, and generated diff.
- Use short-lived, least-privilege credentials supplied at runtime.
- Keep provider CLI authentication and account policy outside TestHarbor.
- Do not use TestHarbor on sensitive code until the selected provider's data handling terms are acceptable for that repository.
- Back up important repositories and local TestHarbor data before upgrading preview releases.

See [Provider data flow](./provider-data-flow.md) for the content that can leave the local machine.
