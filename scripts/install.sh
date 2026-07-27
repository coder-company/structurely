#!/usr/bin/env sh
set -eu

REPOSITORY="${STRUCTURELY_REPOSITORY:-https://github.com/coder-company/structurely}"
VERSION="${STRUCTURELY_VERSION:-}"

if ! command -v cargo >/dev/null 2>&1; then
  printf '%s\n' \
    "Structurely currently installs through Cargo, but cargo was not found." \
    "Install Rust from https://rustup.rs and run this installer again." >&2
  exit 1
fi

set -- cargo install --locked --force --git "$REPOSITORY"
if [ -n "$VERSION" ]; then
  set -- "$@" --tag "$VERSION"
fi
set -- "$@" structurely

printf 'Installing Structurely from %s%s\n' \
  "$REPOSITORY" "${VERSION:+ at $VERSION}"
"$@"

printf '%s\n' \
  "Structurely installed successfully." \
  "Run: structurely --help"

