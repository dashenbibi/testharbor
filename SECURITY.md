# Security policy

## Supported versions

TestHarbor is an early preview. Security fixes are applied to the latest released `0.1.x` version and the default development branch. Older preview builds are not supported.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability or include repository content, credentials, logs, or exploit details in a public discussion.

Use GitHub's **Security -> Report a vulnerability** flow when private vulnerability reporting is enabled. If it is unavailable, contact a maintainer privately through the contact method listed on the project profile and include:

- the affected version and platform;
- the security boundary or data at risk;
- minimal reproduction steps;
- whether credentials or confidential repository content may have been exposed;
- any suggested mitigation.

Maintainers will acknowledge a complete report within five business days, coordinate reproduction and remediation privately, and credit the reporter unless anonymity is requested. Please allow a reasonable remediation window before public disclosure.

The limits documented in [Security model](./docs/security-model.md)—including the absence of an OS sandbox—are not vulnerabilities by themselves. Unexpected bypasses of documented profile, trust, path, process, evidence, or apply controls should be reported.
