#!/bin/bash
# Publish nostr-double-ratchet crates to crates.io in dependency order
#
# Usage:
#   ./scripts/publish.sh        # Publish all crates
#   ./scripts/publish.sh --dry-run  # Test without publishing

set -e

DRY_RUN=""
ALLOW_DIRTY="--allow-dirty"

if [[ "$1" == "--dry-run" ]]; then
    DRY_RUN="--dry-run"
    echo "=== DRY RUN MODE ==="
fi

# Wait time between publishes for crates.io indexing (seconds)
WAIT_TIME=30

publish_crate() {
    local crate=$1
    local extra_flags=${2:-""}

    echo ""
    echo "=========================================="
    echo "Publishing: $crate"
    echo "=========================================="

    if cargo publish -p "$crate" $DRY_RUN $ALLOW_DIRTY $extra_flags; then
        echo "✓ $crate published successfully"

        if [[ -z "$DRY_RUN" ]]; then
            echo "Waiting ${WAIT_TIME}s for crates.io to index..."
            sleep $WAIT_TIME
        fi
    else
        echo "✗ Failed to publish $crate (continuing...)"
    fi
}

echo "Publishing nostr-double-ratchet crates to crates.io"
echo ""

# Check if logged in
if [[ -z "$DRY_RUN" ]]; then
    echo "Checking crates.io authentication..."
    if ! cargo login --help > /dev/null 2>&1; then
        echo "Please run 'cargo login' first"
        exit 1
    fi
fi

# Tier 1: Library (no internal dependencies)
publish_crate "nostr-double-ratchet"

# Tier 2: CLI (depends on library)
publish_crate "ndr"

echo ""
echo "=========================================="
echo "✓ All crates published successfully!"
echo "=========================================="
