# Release Process

## Prepare

1. Update the version in `Cargo.toml` and `web/package.json`.
2. Run `cargo update --workspace` only when dependency versions change, then
   review `Cargo.lock`. Rig stays pinned to the reviewed revision in
   `Cargo.toml` and the source patch under `vendor/rig`.
3. Run `npm install --prefix web` when browser dependencies change, then review
   and commit `web/package-lock.json`.
4. Review the npm integrity hashes in `web/package-lock.json`. Nix imports that
   lockfile directly for the fixed browser dependency derivation.
5. Run `just check-ci` and `scripts/package-check`.
6. Commit the version and reviewed lockfile changes.

## Build Artifacts

The release workflow builds Rust 1.94.0 executables for macOS arm64 and Linux
x86-64 musl. Each archive contains:

- `bin/intake`
- `bin/intake-fastmail-source`
- `bin/intake-github-source`
- `share/doc/intake/examples/skills`
- `LICENSE`

Every archive has a SHA-256 checksum file. The Linux musl executables use
bundled SQLite and rustls for a self-contained native runtime. Release checks
reject Bun and Node interpreter dependencies.

## Publish

Push a tag named `v<version>`. The release workflow runs Cargo, npm, package,
and script checks, builds the target archives and checksums, and creates a
draft GitHub release. Review the archive contents, checksums, and release notes
before publishing the draft.

Publishing the draft, pushing a tag, and publishing to a package registry are
separate outward actions that require explicit authorization.
