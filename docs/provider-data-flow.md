# Provider data flow

TestHarbor invokes provider CLIs installed and authenticated by the user. The provider CLI, account, model, and vendor policy determine whether submitted content is transmitted, retained, or used outside the local machine.

| Stage | Possible provider input | Expected capability |
| --- | --- | --- |
| Generator | Test Intent, selected attachments, repository context, generation scope, existing tests | Write only inside the isolated candidate worktree |
| Test reviewer | Full intent and attachments, candidate diff, validation output, inventory, intended manifest, command hashes, evidence references | Read-only bundle; no shell, repository write, web, or subagent capability requested |
| Analyzer | Formal execution output, normalized results, candidate and command context | Read-only bundle |
| Result reviewer | Intent, review context, normalized execution evidence, analyzer output | Read-only bundle |

Provider responses are accepted only through the versioned envelope expected for that stage. Schema, task, intent, scope, hash, enum, or evidence-reference mismatches require human handling.

## Secrets

Runtime secret values are passed from the launching environment to approved child processes. The database stores the key, source, and an HMAC fingerprint rather than the plaintext value. TestHarbor redacts known values before persisting logs, provider output, and reports, but users must still avoid embedding secrets in source, attachments, objectives, acceptance criteria, or command arguments.

## User responsibilities

- Review the provider vendor's current terms and account settings.
- Confirm that repository owners permit sending the selected content to that provider.
- Use a provider account appropriate for the repository's confidentiality level.
- Remove or rotate credentials if a log, artifact, or provider response may have exposed them.
