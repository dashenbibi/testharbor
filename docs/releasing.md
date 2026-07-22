# Release packaging

TestHarbor uses `.github/workflows/macos-release.yml` for native macOS packaging.

## Preview build

Run **macOS package** manually from the GitHub Actions page, or with:

```bash
gh workflow run macos-release.yml --ref main
```

The workflow builds Apple Silicon and Intel packages on matching native runners and uploads them as workflow artifacts. Manual builds pass `--no-sign`; they are for packaging verification only and will trigger macOS trust warnings when downloaded.

## Draft release

Before creating a version tag, set these GitHub Actions secrets:

- `APPLE_CERTIFICATE`: base64-encoded Developer ID Application certificate in PKCS #12 format;
- `APPLE_CERTIFICATE_PASSWORD`: password for that certificate;
- `APPLE_SIGNING_IDENTITY`: certificate signing identity;
- `APPLE_ID`: Apple account used for notarization;
- `APPLE_PASSWORD`: app-specific password for that account;
- `APPLE_TEAM_ID`: Apple Developer team identifier.

Keep `apps/desktop/package.json` and `apps/desktop/src-tauri/tauri.conf.json` on the same version, then push the matching tag:

```bash
git tag -s v0.1.0 -m "TestHarbor v0.1.0"
git push origin v0.1.0
```

The workflow rejects a mismatched version or missing release secret. A successful tag build creates a draft GitHub Release and uploads the signed, notarized Apple Silicon and Intel bundles. Review the generated notes and assets before publishing the draft.

Do not publish manual preview artifacts as a release. Checksums, an SBOM, complete third-party notices, clean-machine installation tests, and final release approval remain separate release gates.
