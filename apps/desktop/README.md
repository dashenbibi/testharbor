# TestHarbor desktop

The desktop application is a React 19 frontend embedded in a Tauri 2 shell. Business transitions and guards stay in the Rust domain and application layers; this package renders backend-provided state and action descriptors.

## Development

```bash
npm ci
npm run build
npm run tauri dev
```

`tauri dev` and `tauri build` compile a target-specific `testharbor-process-launcher` sidecar. Generated sidecar binaries and `dist/` are not source artifacts and must not be committed.

The supported release path is currently macOS. See the repository [security model](../../docs/security-model.md) before packaging or distributing the application.
