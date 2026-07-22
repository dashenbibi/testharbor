# Third-party software notices

TestHarbor depends on third-party Rust and JavaScript packages listed in `Cargo.lock` and `apps/desktop/package-lock.json`. Each dependency remains subject to its own license; the TestHarbor MIT License does not replace those terms.

The automated Rust policy allows common permissive licenses and narrowly permits MPL-2.0 for the currently reviewed crates listed in `deny.toml`. JavaScript dependency metadata must also be reviewed when the lockfile changes. Binary releases must include a generated dependency inventory, applicable license texts and notices, and an SBOM.

This file records the distribution policy rather than providing legal advice. Maintainers must regenerate and review the complete notices for every binary release because dependency versions and bundled assets can change.
