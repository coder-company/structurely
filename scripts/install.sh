#!/usr/bin/env sh
set -eu

REPOSITORY="${STRUCTURELY_REPOSITORY:-coder-company/structurely}"
VERSION="${STRUCTURELY_VERSION:-latest}"
INSTALL_DIR="${STRUCTURELY_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME:?HOME is required}/.local/bin}}"
OS="${STRUCTURELY_OS:-$(uname -s)}"
ARCH="${STRUCTURELY_ARCH:-$(uname -m)}"

case "$OS" in
  Linux) platform="linux" ;;
  Darwin) platform="macos" ;;
  *) printf 'Structurely does not provide a native release for %s.\n' "$OS" >&2; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64) architecture="x86_64" ;;
  arm64|aarch64) architecture="aarch64" ;;
  *) printf 'Structurely does not provide a native release for %s/%s.\n' "$OS" "$ARCH" >&2; exit 1 ;;
esac

if [ "$platform" = "linux" ] && [ "$architecture" = "aarch64" ]; then
  printf '%s\n' \
    "Structurely does not yet publish a Linux aarch64 binary." \
    "Install Rust 1.88+ and run: cargo install --locked structurely" >&2
  exit 1
fi

asset="structurely-${platform}-${architecture}.tar.gz"
if [ -n "${STRUCTURELY_RELEASE_BASE_URL:-}" ]; then
  release_url="${STRUCTURELY_RELEASE_BASE_URL%/}"
elif [ "$VERSION" = "latest" ]; then
  release_url="https://github.com/${REPOSITORY}/releases/latest/download"
else
  case "$VERSION" in v*) tag="$VERSION" ;; *) tag="v$VERSION" ;; esac
  release_url="https://github.com/${REPOSITORY}/releases/download/${tag}"
fi

download() {
  if command -v curl >/dev/null 2>&1; then
    curl --fail --location --silent --show-error "$1" --output "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet --output-document="$2" "$1"
  else
    printf '%s\n' "Install curl or wget, then run this installer again." >&2
    exit 1
  fi
}

temporary="$(mktemp -d "${TMPDIR:-/tmp}/structurely-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

printf 'Downloading Structurely %s for %s/%s...\n' "$VERSION" "$platform" "$architecture"
download "$release_url/$asset" "$temporary/$asset"
download "$release_url/SHA256SUMS" "$temporary/SHA256SUMS"

expected="$(
  awk -v asset="$asset" '
    $2 == asset || $2 == "*" asset { print $1; found = 1; exit }
    END { if (!found) exit 1 }
  ' "$temporary/SHA256SUMS"
)" || {
  printf 'SHA256SUMS does not contain %s.\n' "$asset" >&2
  exit 1
}

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$temporary/$asset" | awk '{print $1}')"
else
  printf '%s\n' "A SHA-256 tool (sha256sum or shasum) is required." >&2
  exit 1
fi

if [ "$actual" != "$expected" ]; then
  printf 'Checksum verification failed for %s.\n' "$asset" >&2
  exit 1
fi

mkdir "$temporary/package"
tar -xzf "$temporary/$asset" -C "$temporary/package"
binary="$(find "$temporary/package" -type f -name structurely | head -n 1)"
if [ -z "$binary" ]; then
  printf 'The release archive does not contain the Structurely binary.\n' >&2
  exit 1
fi
chmod 755 "$binary"
"$binary" --version >/dev/null

mkdir -p "$INSTALL_DIR"
staged="$INSTALL_DIR/.structurely.new.$$"
cp "$binary" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$INSTALL_DIR/structurely"

printf 'Installed %s\n' "$("$INSTALL_DIR/structurely" --version)"
printf 'Binary: %s/structurely\n' "$INSTALL_DIR"
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) printf '\nAdd Structurely to PATH:\n  export PATH="%s:$PATH"\n' "$INSTALL_DIR" ;;
esac
printf '\nNext: cd your-project && structurely setup codex\n'
