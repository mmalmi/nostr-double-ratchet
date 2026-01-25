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

# Target configurations
# ABI name -> Rust target
declare -A TARGETS=(
    ["arm64-v8a"]="aarch64-linux-android"
    ["armeabi-v7a"]="armv7-linux-androideabi"
    ["x86_64"]="x86_64-linux-android"
    ["x86"]="i686-linux-android"
)

OUTPUT_DIR="$RUST_ROOT/target/android/jniLibs"
mkdir -p "$OUTPUT_DIR"

# Build each target
for ABI in "${!TARGETS[@]}"; do
    TARGET="${TARGETS[$ABI]}"
    echo ""
    echo "==> Building for $ABI ($TARGET)"
    
    cargo ndk -t "$TARGET" build -p ndr-ffi $CARGO_FLAGS
    
    # Copy output
    ABI_DIR="$OUTPUT_DIR/$ABI"
    mkdir -p "$ABI_DIR"
    
    if [[ "$BUILD_TYPE" == "release" ]]; then
        SRC="$RUST_ROOT/target/$TARGET/release/libndr_ffi.so"
    else
        SRC="$RUST_ROOT/target/$TARGET/debug/libndr_ffi.so"
    fi
    
    if [[ -f "$SRC" ]]; then
        cp "$SRC" "$ABI_DIR/"
        echo "    Copied to $ABI_DIR/libndr_ffi.so"
    else
        echo "    Warning: $SRC not found"
    fi
done

# Generate Kotlin bindings
echo ""
echo "==> Generating Kotlin bindings"

BINDINGS_DIR="$RUST_ROOT/target/android/bindings"
mkdir -p "$BINDINGS_DIR"

cargo run -p ndr-ffi --features uniffi/cli -- \
    generate --library "$RUST_ROOT/target/aarch64-linux-android/$BUILD_TYPE/libndr_ffi.so" \
    --language kotlin \
    --out-dir "$BINDINGS_DIR" 2>/dev/null || {
    echo "    Note: Binding generation requires the library to be built first."
    echo "    You can generate bindings manually with:"
    echo "    cargo run -p ndr-ffi --features uniffi/cli -- generate --library <lib.so> --language kotlin --out-dir <dir>"
}

echo ""
echo "==> Android build complete!"
echo "    JNI libs: $OUTPUT_DIR"
echo "    Bindings: $BINDINGS_DIR"
echo ""
echo "To use in your Android project:"
echo "  1. Copy jniLibs/ to your module's src/main/"
echo "  2. Copy bindings/*.kt to your source directory"
echo "  3. Add to build.gradle: implementation 'net.java.dev.jna:jna:5.13.0@aar'"
