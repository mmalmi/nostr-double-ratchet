#!/bin/bash
set -euo pipefail

# Build script for iOS frameworks
#
# Prerequisites:
#   - Xcode installed (with command line tools)
#   - Rust targets installed:
#     rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
#
# Usage:
#   ./build-ios.sh [--release]
#
# Output:
#   target/ios/NdrFfi.xcframework

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

echo "==> Building ndr-ffi for iOS ($BUILD_TYPE)"
echo "    Project root: $PROJECT_ROOT"
echo "    Rust root: $RUST_ROOT"

cd "$RUST_ROOT"

# Check if we're on macOS
if [[ "$(uname)" != "Darwin" ]]; then
    echo "Error: iOS builds require macOS"
    exit 1
fi

# Targets
IOS_TARGETS=(
    "aarch64-apple-ios"        # iOS devices
    "aarch64-apple-ios-sim"    # iOS Simulator (Apple Silicon)
    "x86_64-apple-ios"         # iOS Simulator (Intel)
)

OUTPUT_DIR="$RUST_ROOT/target/ios"
mkdir -p "$OUTPUT_DIR"

# Build each target
for TARGET in "${IOS_TARGETS[@]}"; do
    echo ""
    echo "==> Building for $TARGET"
    cargo build -p ndr-ffi --target "$TARGET" $CARGO_FLAGS
done

echo ""
echo "==> Creating XCFramework"

FRAMEWORK_NAME="NdrFfi"
XCFRAMEWORK_PATH="$OUTPUT_DIR/$FRAMEWORK_NAME.xcframework"

# Remove old framework
rm -rf "$XCFRAMEWORK_PATH"

# Get library paths
if [[ "$BUILD_TYPE" == "release" ]]; then
    IOS_LIB="$RUST_ROOT/target/aarch64-apple-ios/release/libndr_ffi.a"
    SIM_ARM_LIB="$RUST_ROOT/target/aarch64-apple-ios-sim/release/libndr_ffi.a"
    SIM_X64_LIB="$RUST_ROOT/target/x86_64-apple-ios/release/libndr_ffi.a"
else
    IOS_LIB="$RUST_ROOT/target/aarch64-apple-ios/debug/libndr_ffi.a"
    SIM_ARM_LIB="$RUST_ROOT/target/aarch64-apple-ios-sim/debug/libndr_ffi.a"
    SIM_X64_LIB="$RUST_ROOT/target/x86_64-apple-ios/debug/libndr_ffi.a"
fi

# Create fat library for simulators
SIM_FAT_LIB="$OUTPUT_DIR/libndr_ffi_sim.a"
if [[ -f "$SIM_ARM_LIB" ]] && [[ -f "$SIM_X64_LIB" ]]; then
    echo "    Creating fat library for simulators..."
    lipo -create "$SIM_ARM_LIB" "$SIM_X64_LIB" -output "$SIM_FAT_LIB"
elif [[ -f "$SIM_ARM_LIB" ]]; then
    cp "$SIM_ARM_LIB" "$SIM_FAT_LIB"
elif [[ -f "$SIM_X64_LIB" ]]; then
    cp "$SIM_X64_LIB" "$SIM_FAT_LIB"
fi

# Generate Swift bindings
echo ""
echo "==> Generating Swift bindings"

BINDINGS_DIR="$OUTPUT_DIR/bindings"
HEADERS_DIR="$OUTPUT_DIR/headers"
mkdir -p "$BINDINGS_DIR" "$HEADERS_DIR"

# Build the library for the host to run uniffi-bindgen
cargo build -p ndr-ffi

# Generate bindings using uniffi-bindgen-library-mode if available
# Otherwise use the built binary
if cargo run -p ndr-ffi --features uniffi/cli -- \
    generate --library "$RUST_ROOT/target/debug/libndr_ffi.dylib" \
    --language swift \
    --out-dir "$BINDINGS_DIR" 2>/dev/null; then
    echo "    Swift bindings generated in $BINDINGS_DIR"
    
    # Move header file
    if [[ -f "$BINDINGS_DIR/ndr_ffiFFI.h" ]]; then
        cp "$BINDINGS_DIR/ndr_ffiFFI.h" "$HEADERS_DIR/"
    fi
else
    echo "    Note: Swift binding generation requires additional setup."
    echo "    You can generate bindings manually with uniffi-bindgen."
fi

# Create module map
cat > "$HEADERS_DIR/module.modulemap" << 'MODULEMAP'
framework module NdrFfi {
    umbrella header "ndr_ffiFFI.h"
    export *
    module * { export * }
}
MODULEMAP

# Create XCFramework
echo ""
echo "==> Assembling XCFramework"

XCODE_ARGS=()

# Add iOS device library
if [[ -f "$IOS_LIB" ]]; then
    XCODE_ARGS+=(-library "$IOS_LIB" -headers "$HEADERS_DIR")
fi

# Add simulator library
if [[ -f "$SIM_FAT_LIB" ]]; then
    XCODE_ARGS+=(-library "$SIM_FAT_LIB" -headers "$HEADERS_DIR")
fi

if [[ ${#XCODE_ARGS[@]} -gt 0 ]]; then
    xcodebuild -create-xcframework \
        "${XCODE_ARGS[@]}" \
        -output "$XCFRAMEWORK_PATH"
    
    echo "    XCFramework created: $XCFRAMEWORK_PATH"
else
    echo "    Warning: No libraries found to create XCFramework"
fi

echo ""
echo "==> iOS build complete!"
echo "    XCFramework: $XCFRAMEWORK_PATH"
echo "    Swift bindings: $BINDINGS_DIR"
echo ""
echo "To use in your iOS project:"
echo "  1. Drag NdrFfi.xcframework into your Xcode project"
echo "  2. Add the Swift binding files to your project"
echo "  3. Import NdrFfi in your Swift code"
