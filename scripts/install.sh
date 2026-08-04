#!/usr/bin/env sh
set -eu

REPOSITORY="${STRUCTURELY_REPOSITORY:-coder-company/structurely}"
VERSION="${STRUCTURELY_VERSION:-latest}"
INSTALL_DIR="${STRUCTURELY_INSTALL_DIR:-${XDG_BIN_HOME:-${HOME:?HOME is required}/.local/bin}}"
OS="${STRUCTURELY_OS:-$(uname -s)}"
ARCH="${STRUCTURELY_ARCH:-$(uname -m)}"

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ -z "${STRUCTURELY_NO_COLOR:-}" ]; then
  reset="$(printf '\033[0m')"
  bold="$(printf '\033[1m')"
  dim="$(printf '\033[2m')"
  indigo="$(printf '\033[38;5;99m')"
  green="$(printf '\033[38;5;78m')"
  red="$(printf '\033[38;5;203m')"
else
  reset="" bold="" dim="" indigo="" green="" red=""
fi

say() { printf '%s\n' "$*"; }
step() { printf '%s[%s/%s]%s %s%s%s\n' "$indigo" "$1" "$2" "$reset" "$bold" "$3" "$reset"; }
detail() { printf '      %s%s%s\n' "$dim" "$1" "$reset"; }
ok() { printf '      %sverified%s %s\n' "$green" "$reset" "$1"; }
warn() { printf '      %swarning%s %s\n' "$red" "$reset" "$1" >&2; }
die() { printf '\n%sInstallation stopped:%s %s\n' "$red" "$reset" "$1" >&2; exit 1; }

say ""
printf '  %sStructurely%s\n' "$bold" "$reset"
printf '  %sLocal-first code intelligence for coding agents%s\n\n' "$dim" "$reset"

case "$OS" in
  Linux) platform="linux" ;;
  Darwin) platform="macos" ;;
  *) die "No native release is available for $OS." ;;
esac

case "$ARCH" in
  x86_64|amd64) architecture="x86_64" ;;
  arm64|aarch64) architecture="aarch64" ;;
  *) die "No native release is available for $OS/$ARCH." ;;
esac

if [ "$platform" = "linux" ] && [ "$architecture" = "aarch64" ]; then
  printf '%s\n' \
    "No native Linux aarch64 release is published yet." \
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
    curl --fail --location --silent --show-error --retry 2 --connect-timeout 15 "$1" --output "$2"
  elif command -v wget >/dev/null 2>&1; then
    wget --quiet --tries=3 --timeout=15 --output-document="$2" "$1"
  else
    die "Install curl or wget, then run this installer again."
  fi
}

temporary="$(mktemp -d "${TMPDIR:-/tmp}/structurely-install.XXXXXX")"
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

step 1 4 "Detect platform"
detail "Target       $platform/$architecture"
detail "Destination  $INSTALL_DIR/structurely"
detail "Release      $VERSION"

step 2 4 "Download release"
detail "$asset"
download "$release_url/$asset" "$temporary/$asset"
download "$release_url/SHA256SUMS" "$temporary/SHA256SUMS"

step 3 4 "Verify and stage"
expected="$(
  awk -v asset="$asset" '
    $2 == asset || $2 == "*" asset { print $1; found = 1; exit }
    END { if (!found) exit 1 }
  ' "$temporary/SHA256SUMS"
)" || die "SHA256SUMS does not contain $asset. The existing installation was not changed."

if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$temporary/$asset" | awk '{print $1}')"
elif command -v shasum >/dev/null 2>&1; then
  actual="$(shasum -a 256 "$temporary/$asset" | awk '{print $1}')"
else
  die "A SHA-256 tool (sha256sum or shasum) is required."
fi

if [ "$actual" != "$expected" ]; then
  die "Checksum verification failed for $asset. The existing installation was not changed."
fi
ok "SHA-256 checksum"

mkdir "$temporary/package"
tar -xzf "$temporary/$asset" -C "$temporary/package"
binary="$temporary/package/structurely"
if [ ! -f "$binary" ] || [ -L "$binary" ]; then
  die "The verified release archive does not contain a regular Structurely binary."
fi
chmod 755 "$binary"
installed_version="$("$binary" --version)" || die "The downloaded binary could not start on this machine."
ok "$installed_version starts correctly"

destination="$INSTALL_DIR/structurely"
previous_version=""
if [ -x "$destination" ]; then
  previous_version="$("$destination" --version 2>/dev/null || true)"
fi

step 4 4 "Install atomically"
mkdir -p "$INSTALL_DIR"
staged="$INSTALL_DIR/.structurely.new.$$"
cp "$binary" "$staged"
chmod 755 "$staged"
mv -f "$staged" "$destination"
if [ -n "$previous_version" ] && [ "$previous_version" != "$installed_version" ]; then
  detail "Updated      $previous_version -> $installed_version"
elif [ -n "$previous_version" ]; then
  detail "Reinstalled  $installed_version"
else
  detail "Installed    $installed_version"
fi
detail "Binary       $destination"

say ""
printf '  %sStructurely is ready.%s\n' "$green$bold" "$reset"

path_hint=""
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) path_hint="export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

if [ -n "$path_hint" ]; then
  say ""
  printf '  %sAdd it to this shell%s\n' "$bold" "$reset"
  printf '    %s\n' "$path_hint"
fi

say ""
printf '  %sStart in a repository%s\n' "$bold" "$reset"
say "    cd your-project"
say "    structurely setup codex"

dashboard_setup="${STRUCTURELY_DASHBOARD_SETUP:-}"
if [ -z "$dashboard_setup" ]; then
  if [ -n "${CI:-}" ] || [ ! -t 0 ] || [ ! -t 1 ]; then
    dashboard_setup="skip"
  else
    dashboard_setup="prompt"
  fi
fi

if [ "$dashboard_setup" = "prompt" ]; then
  say ""
  printf '  %sOptional private dashboard%s\n' "$bold" "$reset"
  detail "The shell may be hosted; repository data always stays on this machine."
  say ""
  say "    1  Local only        Start it yourself when needed"
  say "    2  Cloudflare Pages  Deploy the static shell"
  say "    3  Vercel           Deploy the static shell"
  say "    4  Not now          Finish installation"
  say ""
  printf '  Choose %s[1-4, default 4]%s: ' "$dim" "$reset"
  IFS= read -r dashboard_choice || dashboard_choice=""
  case "$dashboard_choice" in
    1|local) dashboard_setup="local" ;;
    2|cloudflare) dashboard_setup="cloudflare" ;;
    3|vercel) dashboard_setup="vercel" ;;
    *) dashboard_setup="skip" ;;
  esac
fi

case "$dashboard_setup" in
  cloudflare|vercel)
    say ""
    printf '  %sDashboard deployment%s\n' "$bold" "$reset"
    detail "Provider      $dashboard_setup"
    detail "Upload        Static shell only; no repository data"
    detail "Requirement   Authenticated provider CLI already installed"
    if "$destination" dashboard deploy "$dashboard_setup"; then
      ok "dashboard deployment"
    else
      dashboard_status=$?
      warn "Dashboard deployment failed (exit $dashboard_status); Structurely remains installed."
      detail "Retry with: structurely dashboard deploy $dashboard_setup"
    fi
    ;;
  local)
    say ""
    printf '  %sDashboard ready for local use%s\n' "$bold" "$reset"
    say "    structurely add ."
    say "    structurely dashboard start"
    ;;
  skip|"") ;;
  *) warn "Ignoring STRUCTURELY_DASHBOARD_SETUP=$dashboard_setup; expected cloudflare, vercel, local, skip, or prompt." ;;
esac

say ""
