#!/bin/sh
set -e

REPO="aodihis/skim"
BIN_DIR="${HOME}/.local/bin"

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)
    case "$ARCH" in
      x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
      *) echo "Unsupported architecture: $ARCH" && exit 1 ;;
    esac
    ;;
  darwin)
    case "$ARCH" in
      x86_64) TARGET="x86_64-apple-darwin" ;;
      arm64)  TARGET="aarch64-apple-darwin" ;;
      *)      echo "Unsupported architecture: $ARCH" && exit 1 ;;
    esac
    ;;
  *)
    echo "Unsupported OS: $OS. On Windows use:"
    echo "  irm https://raw.githubusercontent.com/${REPO}/master/scripts/install.ps1 | iex"
    exit 1
    ;;
esac

VERSION=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')

if [ -z "$VERSION" ]; then
  echo "Could not determine latest release version." && exit 1
fi

URL="https://github.com/${REPO}/releases/download/${VERSION}/skim-${TARGET}"

mkdir -p "$BIN_DIR"
curl -fsSL "$URL" -o "$BIN_DIR/skim"
chmod +x "$BIN_DIR/skim"

echo ""
echo "skim ${VERSION} installed to ${BIN_DIR}/skim"
echo ""
echo "Make sure ${BIN_DIR} is in your PATH:"
echo "  export PATH=\"\$HOME/.local/bin:\$PATH\""
