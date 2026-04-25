#!/bin/bash
set -euo pipefail

# Build script for Android native libraries
#
# Prerequisites:
#   - Android NDK installed
#   - Rust targets installed:
#     rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android i686-linux-android
#   - cargo-ndk installed:
#     cargo install cargo-ndk
#
# Usage:
#   ./build-android.sh [--release]
#
# Output:
#   target/android/jniLibs/{arm64-v8a,armeabi-v7a,x86_64,x86}/libndr_ffi.so

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RUST_ROOT="$PROJECT_ROOT/rust"

# Parse arguments
BUILD_TYPE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    BUILD_TYPE="release"
    CARGO_FLAGS="--release"
fi

echo "==> Building ndr-ffi for Android ($BUILD_TYPE)"
echo "    Project root: $PROJECT_ROOT"
echo "    Rust root: $RUST_ROOT"

# Check prerequisites
if ! command -v cargo-ndk &> /dev/null; then
    echo "Error: cargo-ndk not found. Install with: cargo install cargo-ndk"
    exit 1
fi

if [[ -z "${ANDROID_NDK_HOME:-}" ]] && [[ -z "${NDK_HOME:-}" ]]; then
    echo "Warning: ANDROID_NDK_HOME or NDK_HOME not set. cargo-ndk will try to find NDK automatically."
fi

cd "$RUST_ROOT"

OUTPUT_DIR="$RUST_ROOT/target/android/jniLibs"
mkdir -p "$OUTPUT_DIR"

echo ""
echo "==> Building native libraries"
echo "    Output: $OUTPUT_DIR"

# Note: Use Android ABI names for cargo-ndk targets.
cargo ndk \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -t x86 \
    -o "$OUTPUT_DIR" \
    build -p ndr-ffi --lib $CARGO_FLAGS

# Generate Kotlin bindings
echo ""
echo "==> Generating Kotlin bindings"

BINDINGS_DIR="$RUST_ROOT/target/android/bindings"
mkdir -p "$BINDINGS_DIR"

# Pick one built library for UniFFI metadata extraction.
LIB_FOR_BINDGEN="$OUTPUT_DIR/arm64-v8a/libndr_ffi.so"
if [[ ! -f "$LIB_FOR_BINDGEN" ]]; then
    # Fall back to the first ABI that exists.
    for ABI in arm64-v8a armeabi-v7a x86_64 x86; do
        if [[ -f "$OUTPUT_DIR/$ABI/libndr_ffi.so" ]]; then
            LIB_FOR_BINDGEN="$OUTPUT_DIR/$ABI/libndr_ffi.so"
            break
        fi
    done
fi

cargo run -p ndr-ffi --features bindgen-cli --bin uniffi-bindgen -- \
    generate --library "$LIB_FOR_BINDGEN" \
    --language kotlin \
    --out-dir "$BINDINGS_DIR" 2>/dev/null || {
    echo "    Note: Binding generation requires the library to be built first."
    echo "    You can generate bindings manually with:"
    echo "    cargo run -p ndr-ffi --features bindgen-cli --bin uniffi-bindgen -- generate --library <lib.so> --language kotlin --out-dir <dir>"
}

if [[ "$BUILD_TYPE" == "release" ]]; then
    STRIP_BIN="${LLVM_STRIP:-}"
    if [[ -z "$STRIP_BIN" ]]; then
        NDK_ROOT="${ANDROID_NDK_HOME:-${NDK_HOME:-}}"
        if [[ -n "$NDK_ROOT" ]]; then
            STRIP_BIN="$(find "$NDK_ROOT/toolchains/llvm/prebuilt" -path "*/bin/llvm-strip" -print -quit 2>/dev/null || true)"
        fi
    fi
    if [[ -z "$STRIP_BIN" ]]; then
        STRIP_BIN="$(command -v llvm-strip || true)"
    fi

    if [[ -n "$STRIP_BIN" ]]; then
        echo ""
        echo "==> Stripping release native libraries"
        echo "    Strip tool: $STRIP_BIN"
        for ABI in arm64-v8a armeabi-v7a x86_64 x86; do
            LIB="$OUTPUT_DIR/$ABI/libndr_ffi.so"
            if [[ -f "$LIB" ]]; then
                "$STRIP_BIN" --strip-unneeded "$LIB"
            fi
        done
    else
        echo "Warning: llvm-strip not found; release libraries keep non-runtime symbol tables."
    fi
fi

echo ""
echo "==> Android build complete!"
echo "    JNI libs: $OUTPUT_DIR"
echo "    Bindings: $BINDINGS_DIR"
echo ""
echo "To use in your Android project:"
echo "  1. Copy jniLibs/ to your module's src/main/"
echo "  2. Copy bindings/*.kt to your source directory"
echo "  3. Add to build.gradle: implementation 'net.java.dev.jna:jna:5.13.0@aar'"
