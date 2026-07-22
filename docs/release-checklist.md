# Public release checklist

Complete every applicable item before changing the repository visibility or publishing a binary.

## Ownership and identity

- [ ] Complete project-name and trademark clearance; rename the product before publication if the current name is not sufficiently distinctive.
- [ ] Record the actual copyright owner approved to publish the code and assets.
- [x] Create the public repository under the intended owner (`dashenbibi/testharbor`).
- [x] Add the owner-controlled repository and homepage URLs to Cargo and npm metadata.
- [x] Use the GitHub-owner-based application identifier `io.github.dashenbibi.testharbor`.
- [x] Add `.github/CODEOWNERS` using the active GitHub account (`@dashenbibi`).

## Source and legal

- [ ] Confirm every contribution and asset can be released under MIT.
- [x] Run `scripts/export-open-source.sh` to create the exact public source tree.
- [x] Run a secret scan against the public tree and any Git history that will be published.
- [x] Confirm internal planning documents, nested Git repositories, local state, build output, and generated sidecars are absent.
- [ ] Run `cargo deny check licenses` and review JavaScript dependency licenses.
- [ ] Generate complete third-party notices and an SBOM for binary artifacts.

## Claims and verification

- [ ] Verify every feature named in the README against the release commit.
- [ ] Complete a controlled run for each advertised real provider without publishing private prompts, logs, or credentials.
- [ ] Confirm Go Test JSON is the only driver described as supporting strict AcceptPass unless another driver has reached the same capability.
- [ ] Confirm English is the default and English/Chinese switching works in the desktop app and CLI.
- [ ] Verify concurrent desktop/CLI runs respect the shared process limit and do not interfere with one another.
- [ ] Run all commands in `docs/testing.md` on the release commit.

## Desktop distribution

- [x] Replace template icons and branding with original, rights-cleared assets.
- [x] Validate the Content Security Policy and Tauri capability scopes in a packaged build.
- [x] Restrict release targets to platforms with supported process supervision.
- [ ] Sign and notarize macOS artifacts.
- [ ] Publish checksums, SBOM, third-party notices, and release notes alongside each artifact.
- [ ] Test installation, first launch, upgrade, cancellation, crash recovery, and uninstall/cleanup on a clean macOS account.

## GitHub settings

- [x] Enable private vulnerability reporting, secret scanning, push protection, Dependabot, and branch protection.
- [ ] Require CI and signed-off commits before merging.
- [ ] Set the security contact method referenced by `SECURITY.md` and `CODE_OF_CONDUCT.md`.
- [ ] Review issue and pull-request templates using a non-maintainer account.
