#!/bin/sh
# soli-sfu installer - works with any POSIX shell
# Usage: curl -sSL https://raw.githubusercontent.com/solisoft/sfu/main/install.sh | sh
#   or:  sh install.sh [--system | --user]

set -e

REPO="solisoft/sfu"
BIN="soli-sfu"
SYSTEM_DIR="/usr/local/bin"
SYSTEM_INSTALL=0
USER_INSTALL=0

for arg in "$@"; do
  case "$arg" in
    --system) SYSTEM_INSTALL=1 ;;
    --user)   USER_INSTALL=1 ;;
    --help|-h)
      echo "Usage: install.sh [--system | --user]"
      echo "  --system  Install to ${SYSTEM_DIR} (requires sudo when not root)"
      echo "  --user    Install to ~/.local/bin (even when running as root)"
      echo "  Default:  ~/.local/bin, or ${SYSTEM_DIR} when running as root"
      exit 0
      ;;
    *) echo "Unknown option: $arg"; exit 1 ;;
  esac
done

IS_ROOT=0
[ "$(id -u)" = "0" ] && IS_ROOT=1

if [ "$USER_INSTALL" = "1" ]; then
  INSTALL_DIR="$HOME/.local/bin"
elif [ "$SYSTEM_INSTALL" = "1" ] || [ "$IS_ROOT" = "1" ]; then
  INSTALL_DIR="$SYSTEM_DIR"
else
  INSTALL_DIR="$HOME/.local/bin"
fi

NEED_ELEVATION=0
if [ "$INSTALL_DIR" = "$SYSTEM_DIR" ] && [ "$IS_ROOT" != "1" ]; then
  NEED_ELEVATION=1
fi

# --- Detect platform ---
OS="$(uname -s)"
case "$OS" in
  Linux*)  OS="linux" ;;
  Darwin*) OS="darwin" ;;
  *) echo "Error: unsupported operating system: $OS"; exit 1 ;;
esac

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64|amd64)  ARCH="amd64" ;;
  aarch64|arm64) ARCH="arm64" ;;
  *) echo "Error: unsupported architecture: $ARCH"; exit 1 ;;
esac

echo "Detected platform: ${OS}-${ARCH}"

if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO- "$1"; }
else
  echo "Error: curl or wget is required"; exit 1
fi

# --- Get latest version tag ---
API_URL="https://api.github.com/repos/${REPO}/releases/latest"
TAG=""
if TAG=$(fetch "$API_URL" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/'); then
  if [ -z "$TAG" ]; then
    TAG=""
  fi
fi

if [ -z "$TAG" ]; then
  echo "Error: could not resolve the latest release of ${REPO}"
  exit 1
fi

echo "Installing ${BIN} ${TAG} ..."

# --- Download, verify, extract ---
TARBALL="${BIN}-${OS}-${ARCH}.tar.gz"
DOWNLOAD_URL="https://github.com/${REPO}/releases/download/${TAG}/${TARBALL}"
TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading ${DOWNLOAD_URL} ..."
fetch "$DOWNLOAD_URL" > "${TMP_DIR}/${TARBALL}"

# verify the published sha256 when a checksum tool is available
EXPECTED=$(fetch "${DOWNLOAD_URL}.sha256" 2>/dev/null || true)
if [ -n "$EXPECTED" ]; then
  if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL=$(sha256sum "${TMP_DIR}/${TARBALL}" | awk '{print $1}')
  elif command -v shasum >/dev/null 2>&1; then
    ACTUAL=$(shasum -a 256 "${TMP_DIR}/${TARBALL}" | awk '{print $1}')
  else
    ACTUAL=""
  fi
  if [ -n "$ACTUAL" ] && [ "$ACTUAL" != "$EXPECTED" ]; then
    echo "Error: checksum mismatch (expected ${EXPECTED}, got ${ACTUAL})"
    exit 1
  fi
fi

tar xzf "${TMP_DIR}/${TARBALL}" -C "$TMP_DIR"

# --- Install binary ---
if [ "$NEED_ELEVATION" = "1" ]; then
  echo "Installing to ${INSTALL_DIR} (requires sudo) ..."
  sudo install -m 755 "${TMP_DIR}/${BIN}" "${INSTALL_DIR}/${BIN}"
else
  echo "Installing to ${INSTALL_DIR} ..."
  mkdir -p "$INSTALL_DIR"
  install -m 755 "${TMP_DIR}/${BIN}" "${INSTALL_DIR}/${BIN}"
fi

# --- Check PATH ---
case ":${PATH}:" in
  *":${INSTALL_DIR}:"*) ;;
  *)
    echo "Note: ${INSTALL_DIR} is not in your PATH."
    echo "Add this to your shell profile:"
    echo "  export PATH=\"${INSTALL_DIR}:\$PATH\""
    ;;
esac

echo "Done. Try: ${BIN} --help  (or: ${BIN} mint-token <user> <room>)"
