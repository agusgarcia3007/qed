#!/bin/sh
# qed installer: downloads a prebuilt binary from the latest GitHub release,
# or builds from source with cargo if no binary fits this machine.
#
#   curl -fsSL https://raw.githubusercontent.com/agusgarcia3007/qed/main/install.sh | sh
#
# Env vars:
#   QED_VERSION      tag to install (default: latest release)
#   QED_INSTALL_DIR  where to put the binary (default: ~/.local/bin)
set -eu

REPO="agusgarcia3007/qed"
VERSION="${QED_VERSION:-latest}"
DIR="${QED_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf 'qed-install: %s\n' "$*" >&2; }
die() { say "error: $*"; exit 1; }

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin) os=apple-darwin ;;
  Linux)  os=unknown-linux-musl ;;
  *) os="" ;;
esac
case "$arch" in
  x86_64|amd64)  arch=x86_64 ;;
  arm64|aarch64) arch=aarch64 ;;
  *) arch="" ;;
esac

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

fetched=""
if [ -n "$os" ] && [ -n "$arch" ]; then
  asset="qed-$arch-$os.tar.gz"
  if [ "$VERSION" = latest ]; then
    url="https://github.com/$REPO/releases/latest/download/$asset"
  else
    url="https://github.com/$REPO/releases/download/$VERSION/$asset"
  fi
  say "downloading $asset ($VERSION)"
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    # gh also works while the repo is private.
    tag=""; [ "$VERSION" = latest ] || tag="$VERSION"
    gh release download $tag -R "$REPO" -p "$asset" -D "$tmp" >/dev/null 2>&1 && fetched=1
  fi
  if [ -z "$fetched" ]; then
    curl -fsSL "$url" -o "$tmp/$asset" 2>/dev/null && fetched=1
  fi
  if [ -n "$fetched" ]; then
    tar -xzf "$tmp/$asset" -C "$tmp" || die "could not extract $asset"
  fi
fi

mkdir -p "$DIR"
if [ -n "$fetched" ]; then
  install -m 755 "$tmp/qed" "$DIR/qed"
else
  say "no prebuilt binary available; building from source with cargo"
  command -v cargo >/dev/null 2>&1 || die "cargo not found. Install Rust from https://rustup.rs and re-run."
  ref=""; [ "$VERSION" = latest ] || ref="--tag $VERSION"
  cargo install --git "https://github.com/$REPO" $ref --locked --root "$tmp/root" qed
  install -m 755 "$tmp/root/bin/qed" "$DIR/qed"
fi

say "installed $DIR/qed"
case ":$PATH:" in
  *":$DIR:"*) ;;
  *) say "note: $DIR is not on your PATH. Add this to your shell profile:"
     say "  export PATH=\"$DIR:\$PATH\"" ;;
esac
command -v claude >/dev/null 2>&1 || say "note: qed needs the Claude Code CLI ('claude'), logged in."
