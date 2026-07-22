# Roadmap

TestHarbor is an early preview. Roadmap items describe direction rather than a compatibility or delivery promise.

## Before the first public binary release

- Complete project-name and trademark review.
- Adopt an owner-controlled application identifier and repository URL.
- Replace template application icons with original, rights-cleared branding.
- Validate the explicit desktop Content Security Policy and minimized Tauri capabilities.
- Publish signed and notarized macOS artifacts with checksums, an SBOM, and third-party notices.
- Complete controlled dogfood runs for every provider capability advertised in the release notes.

## Near term

- Expand UI and CLI contract coverage for concurrent runs and bilingual output.
- Automate dependency license reports and release provenance.
- Improve cleanup, storage retention controls, and operator-facing diagnostics.
- Document schema compatibility and backup expectations before stable migrations.

## Later

- Evaluate Windows process supervision with native job-object semantics.
- Promote additional test drivers to strict pass capability only when they provide stable inventory and intended-test observation.
- Consider stronger OS-level isolation as a separate security feature rather than implying it through application policy.
