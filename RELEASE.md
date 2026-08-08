# Release Process

## Prepare

1. Update the version in `Cargo.toml`, `package.json`, and `web/package.json`.
2. Run `cargo update --workspace` only when dependency versions change, then
   review `Cargo.lock`. Rig stays pinned to the reviewed revision in
   `Cargo.toml` and the source patch under `vendor/rig`.
3. Run `bun install --frozen-lockfile` for the Bun backend and
   `npm install --prefix web` for the browser. Commit changes to `bun.lock` and
   `web/package-lock.json`.
4. Regenerate `bun.nix` with
   `bun2nix --lock-file bun.lock --output-file bun.nix` when `bun.lock` changes.
5. Review the npm integrity hashes in `web/package-lock.json` when browser
   dependencies change. Nix imports that lockfile directly for the fixed browser
   dependency derivation.
6. Run `just check-ci`, `nix flake check --print-build-logs`, and
   `scripts/package-check`.
7. Commit the version and reviewed lockfile or Nix expression changes.

## Build Artifacts

The release workflow builds Rust 1.94.0 executables for macOS arm64 and Linux
x86-64 musl. Each archive contains:

- `bin/intake`
- `bin/intake-fastmail-source`
- `bin/intake-github-source`
- `share/intake/skills`
- `LICENSE`

Every archive has a SHA-256 checksum file. The Linux musl build uses bundled
SQLite and rustls so the executables do not require Bun, Node, OpenSSL, or a
system SQLite library.

## Publish

Push a tag named `v<version>`. The release workflow reruns all Bun, Cargo, npm,
package, and script checks, builds the target archives and checksums, and
creates a draft GitHub release. Review the archive contents, checksums, and
release notes before publishing the draft.

Publishing the draft, pushing a tag, and publishing to a package registry are
separate outward actions that require explicit authorization.
