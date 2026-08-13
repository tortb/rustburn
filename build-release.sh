#!/bin/sh
#
# Build release assets for rustburn.
#
# Usage:
#   ./build-release.sh [VERSION] [--targets T1,T2,...]
#
# If VERSION is omitted, the version is read from Cargo.toml workspace metadata.
# If no targets are given, the script builds for the host target only.
#
# Supported targets:
#   x86_64-unknown-linux-gnu
#   aarch64-unknown-linux-gnu
#   x86_64-apple-darwin
#   aarch64-apple-darwin
#   x86_64-pc-windows-msvc
#
# Prerequisites (host):
#   - cargo, rustup
#   - For cross targets: cross (or the corresponding rustup target + linker)
#   - sha256sum / shasum
#   - zip (for Windows)

set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
cd "$SCRIPT_DIR"

# --- version ------------------------------------------------------------

if [ $# -gt 0 ] && echo "$1" | grep -qE '^v?[0-9]+\.[0-9]+\.[0-9]+'; then
    VERSION="$1"
    shift
else
    VERSION=$(grep '^version' Cargo.toml | head -n 1 | sed 's/.*"\(.*\)".*/\1/')
    VERSION="v${VERSION}"
fi

# --- targets ------------------------------------------------------------

ALL_TARGETS="x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu x86_64-apple-darwin aarch64-apple-darwin x86_64-pc-windows-msvc"
TARGETS=""

while [ $# -gt 0 ]; do
    case "$1" in
        --targets)
            TARGETS=$(echo "$2" | tr ',' ' ')
            shift 2
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 1
            ;;
    esac
done

if [ -z "$TARGETS" ]; then
    rustup show home >/dev/null 2>&1 || true
    TARGETS=$(rustc -vV 2>/dev/null | grep '^host:' | awk '{print $2}' || true)
    if [ -z "$TARGETS" ]; then
        echo "Unable to determine host target. Specify --targets explicitly." >&2
        exit 1
    fi
fi

# --- output dir ---------------------------------------------------------

OUT_DIR="$SCRIPT_DIR/target/release-artifacts"
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"

# --- build function ------------------------------------------------------

build_target() {
    TARGET="$1"
    EXE_NAME="rb"
    if echo "$TARGET" | grep -q windows; then
        EXE_NAME="rb.exe"
    fi

    echo "=== Building $TARGET ==="

    if command -v cross >/dev/null 2>&1 && [ "$TARGET" != "$(rustc -vV | grep '^host:' | awk '{print $2}')" ]; then
        cross build --release --target "$TARGET" -p rustburn
    else
        cargo build --release --target "$TARGET" -p rustburn
    fi

    BIN="target/$TARGET/release/$EXE_NAME"
    if [ ! -f "$BIN" ]; then
        echo "ERROR: binary not found at $BIN" >&2
        exit 1
    fi

    ASSET="rb-$VERSION-$TARGET"

    if echo "$TARGET" | grep -q windows; then
        # Windows: zip
        zip -j "$OUT_DIR/$ASSET.zip" "$BIN"
    else
        # Linux/macOS: tar.gz
        TMP=$(mktemp -d /tmp/rustburn-release-XXXXXX)
        cp "$BIN" "$TMP/rb"
        chmod 755 "$TMP/rb"
        tar -czf "$OUT_DIR/$ASSET.tar.gz" -C "$TMP" rb
        rm -rf "$TMP"
    fi

    echo "  -> $OUT_DIR/$ASSET"
}

# --- build all targets ---------------------------------------------------

for T in $TARGETS; do
    MATCHED=false
    for AT in $ALL_TARGETS; do
        if [ "$T" = "$AT" ]; then
            MATCHED=true
            break
        fi
    done
    if [ "$MATCHED" = false ]; then
        echo "WARNING: unsupported target '$T', skipping." >&2
        continue
    fi
    build_target "$T"
done

# --- SHA256SUMS ----------------------------------------------------------

cd "$OUT_DIR"
if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -- * > SHA256SUMS
elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -- * > SHA256SUMS
else
    echo "WARNING: sha256sum/shasum not found, skipping SHA256SUMS." >&2
fi

echo ""
echo "=== Release artifacts ==="
ls -la "$OUT_DIR"
echo ""
echo "Artifacts ready for upload:"
echo "  gh release create $VERSION $OUT_DIR/*"