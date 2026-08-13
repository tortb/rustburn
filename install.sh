#!/bin/sh
#
# rustburn installer - installs the rb CLI from GitHub Releases.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/tortb/rustburn/master/install.sh | sh
#
# Installs to ~/.local/bin/rb. Requires no sudo and modifies no system
# directories. Only network endpoints used: api.github.com, github.com,
# objects.githubusercontent.com.
#
# Environment overrides (mainly for testing / pinning):
#   RUSTBURN_INSTALL_API_URL   URL of the latest-release API response
#   RUSTBURN_INSTALL_DL_BASE   base URL for release download assets

set -u

API_URL="${RUSTBURN_INSTALL_API_URL:-https://api.github.com/repos/tortb/rustburn/releases/latest}"
DL_BASE="${RUSTBURN_INSTALL_DL_BASE:-https://github.com/tortb/rustburn/releases/download}"

VERSION=""
TMP_DIR=""
TMP_INSTALL=""

die() {
    echo "rustburn installer: $*" >&2
    exit 1
}

cleanup() {
    if [ -n "$TMP_DIR" ]; then
        rm -rf "$TMP_DIR"
    fi
    if [ -n "$TMP_INSTALL" ]; then
        rm -f "$TMP_INSTALL"
    fi
}
trap cleanup EXIT INT TERM HUP

# --- required tools -------------------------------------------------------

for cmd in curl uname mktemp tar; do
    if ! command -v "$cmd" >/dev/null 2>&1; then
        die "required tool not found: $cmd (install it first)"
    fi
done

# --- platform detection ---------------------------------------------------

OS=$(uname -s)
ARCH=$(uname -m)

case "$OS" in
    MINGW* | MSYS* | CYGWIN*)
        echo "rustburn installer does not support native Windows shell." >&2
        echo "Please use WSL or download the Windows binary from GitHub Releases." >&2
        exit 1
        ;;
    Linux | Darwin) ;;
    *)
        die "Unsupported platform: $OS $ARCH"
        ;;
esac

case "$ARCH" in
    x86_64 | amd64) TARGET_ARCH=x86_64 ;;
    aarch64 | arm64) TARGET_ARCH=aarch64 ;;
    *)
        die "Unsupported platform: $OS $ARCH"
        ;;
esac

case "$OS-$TARGET_ARCH" in
    Linux-x86_64) TRIPLE=x86_64-unknown-linux-gnu ;;
    Linux-aarch64) TRIPLE=aarch64-unknown-linux-gnu ;;
    Darwin-x86_64) TRIPLE=x86_64-apple-darwin ;;
    Darwin-aarch64) TRIPLE=aarch64-apple-darwin ;;
esac

# --- fetch latest release -------------------------------------------------

TMP_DIR=$(mktemp -d /tmp/rustburn-install-XXXXXX) || die "unable to create temporary directory"

echo "Fetching latest release info from GitHub..."
curl -fsSL \
    --connect-timeout 10 \
    --max-time 60 \
    -H "Accept: application/vnd.github+json" \
    -H "User-Agent: rustburn-install/1.0" \
    "$API_URL" -o "$TMP_DIR/release.json" \
    || die "Installation failed: unable to contact GitHub."

VERSION=$(sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$TMP_DIR/release.json" | head -n 1)
if [ -z "$VERSION" ]; then
    die "Installation failed: unable to determine the latest release version from GitHub."
fi

ASSET="rb-$VERSION-$TRIPLE.tar.gz"
ASSET_URL="$DL_BASE/$VERSION/$ASSET"
SUMS_URL="$DL_BASE/$VERSION/SHA256SUMS"

# Verify the release metadata actually contains this asset before downloading,
# so a naming-convention mismatch fails loudly instead of fetching the wrong file.
if ! grep -Fq "\"$ASSET\"" "$TMP_DIR/release.json"; then
    die "Release $VERSION does not provide asset $ASSET. Aborting instead of downloading a mismatched file."
fi

echo "Downloading $ASSET..."
curl -fsSL \
    --connect-timeout 10 \
    --max-time 60 \
    -H "Accept: application/octet-stream" \
    -H "User-Agent: rustburn-install/1.0" \
    "$ASSET_URL" -o "$TMP_DIR/$ASSET" \
    || die "Installation failed: unable to download $ASSET."

echo "Downloading SHA256SUMS..."
curl -fsSL \
    --connect-timeout 10 \
    --max-time 60 \
    -H "User-Agent: rustburn-install/1.0" \
    "$SUMS_URL" -o "$TMP_DIR/SHA256SUMS" \
    || die "Installation failed: unable to download SHA256SUMS."

# --- SHA256 verification (mandatory) --------------------------------------

if command -v sha256sum >/dev/null 2>&1; then
    SHASUM_CMD="sha256sum"
elif command -v shasum >/dev/null 2>&1; then
    SHASUM_CMD="shasum -a 256"
else
    die "required tool not found: sha256sum or shasum"
fi

EXPECTED=$(awk -v asset="$ASSET" '$2 == asset || $2 == "*" asset {print $1; exit}' "$TMP_DIR/SHA256SUMS")
if [ -z "$EXPECTED" ]; then
    die "SHA256SUMS does not contain a checksum for $ASSET."
fi

ACTUAL=$($SHASUM_CMD "$TMP_DIR/$ASSET" | awk '{print $1}')
if [ "$ACTUAL" != "$EXPECTED" ]; then
    die "Checksum verification failed."
fi

# --- extract --------------------------------------------------------------

tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR" \
    || die "Downloaded archive is corrupt and could not be extracted."

if [ -f "$TMP_DIR/rb" ]; then
    RB_BIN="$TMP_DIR/rb"
elif [ -f "$TMP_DIR/release/rb" ]; then
    RB_BIN="$TMP_DIR/release/rb"
else
    die "Downloaded archive does not contain rb."
fi

# --- install (atomic replace, old rb preserved on failure) ----------------

mkdir -p "$HOME/.local/bin" || die "unable to create $HOME/.local/bin"

TMP_INSTALL="$HOME/.local/bin/.rb.install.$$"
cp "$RB_BIN" "$TMP_INSTALL" || die "Installation failed: unable to write $HOME/.local/bin/rb."
chmod 755 "$TMP_INSTALL" || die "Installation failed: unable to set permissions on rb."
mv -f "$TMP_INSTALL" "$HOME/.local/bin/rb" || die "Installation failed: unable to install rb."
TMP_INSTALL=""

# --- PATH check & output --------------------------------------------------

IN_PATH=false
case ":$PATH:" in
    *":$HOME/.local/bin:"*) IN_PATH=true ;;
esac

echo ""
echo "rustburn $VERSION installed successfully."
if [ "$IN_PATH" = true ]; then
    echo "Run:"
    echo "  rb"
else
    echo "Binary:"
    echo "  ~/.local/bin/rb"
    echo "~/.local/bin is not in PATH."
    echo "Add ~/.local/bin to your PATH, then run:"
    echo "  rb"
fi
