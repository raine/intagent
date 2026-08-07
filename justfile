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
    bun run format

format-check:
    bun run format:check

lint:
    bun run lint

typecheck:
    bun run typecheck

test:
    bun test

build:
    bun run build

rust-format:
    checkle run rust-format

rust-clippy:
    checkle run rust-clippy

rust-build:
    checkle run rust-build

rust-test:
    checkle run rust-test

package-check:
    scripts/package-check

scripts-check:
    bash -n hooks/pre-commit scripts/install scripts/install-checkle scripts/install-git-hook-shims scripts/package-check
    @if command -v shellcheck >/dev/null 2>&1; then shellcheck hooks/pre-commit scripts/install-checkle scripts/install-git-hook-shims scripts/package-check; else printf '%s\n' 'shellcheck unavailable, bash syntax validation completed'; fi

install-hooks:
    scripts/install-git-hook-shims

run *ARGS:
    bun run src/cli.ts -- "$@"

install-dev:
    mkdir -p "${HOME}/.local/bin"
    ln -sf "$(pwd)/src/cli.ts" "${HOME}/.local/bin/intake"
    ln -sf "$(pwd)/src/sources/fastmail.ts" "${HOME}/.local/bin/intake-fastmail-source"
    ln -sf "$(pwd)/src/sources/github.ts" "${HOME}/.local/bin/intake-github-source"
