set positional-arguments
set shell := ["bash", "-euo", "pipefail", "-c"]

default:
    @just --list

check mode="":
    checkle run {{mode}} project

check-ci: check
    #!/usr/bin/env bash
    set -euo pipefail
    if ! git diff --quiet || ! git diff --cached --quiet; then
      printf '%s\n' 'error: checks changed tracked files' >&2
      git diff --stat
      exit 1
    fi

format:
    npm run format --prefix web
    cargo fmt --all

format-check:
    checkle run browser rust-format

lint:
    npm run lint --prefix web
    checkle run rust-clippy

typecheck:
    npm run typecheck --prefix web

test:
    npm test --prefix web
    cargo test --all-targets

build:
    npm run build --prefix web
    cargo build --release --locked

browser-check:
    checkle run browser

rust-format:
    checkle run rust-format

rust-clippy:
    checkle run rust-clippy

rust-build:
    checkle run rust-release

rust-test:
    checkle run rust-test

package-check:
    scripts/package-check

scripts-check:
    bash -n hooks/pre-commit scripts/install scripts/install-checkle scripts/install-git-hook-shims scripts/package-check
    shellcheck hooks/pre-commit scripts/install scripts/install-checkle scripts/install-git-hook-shims scripts/package-check

install-hooks:
    scripts/install-git-hook-shims

run *ARGS:
    cargo run --bin intagent -- "$@"

install:
    scripts/install
