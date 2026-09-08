#!/usr/bin/env bash
set -e

# ==============================================================================
# INCLINE Watershed Delineation System — Production & Local Run Script
# ==============================================================================

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Source environment variables if .env exists
if [ -f .env ]; then
    export $(grep -v '^#' .env | xargs)
fi

# Fallback dataset search paths
DEFAULT_DATASET="${INCLINE_DATASET_PATH:-}"
if [ -z "$DEFAULT_DATASET" ]; then
    if [ -d "/Users/siddikbarbhuiya/grit-hfx-v0.3.0" ]; then
        DEFAULT_DATASET="/Users/siddikbarbhuiya/grit-hfx-v0.3.0"
    elif [ -d "/opt/datasets/grit-hfx-v0.3.0" ]; then
        DEFAULT_DATASET="/opt/datasets/grit-hfx-v0.3.0"
    else
        DEFAULT_DATASET="https://basin-delineations-public.upstream.tech/grit/hfx-v0.3.0/"
    fi
fi

DATASET="${1:-$DEFAULT_DATASET}"
PORT="${PORT:-8787}"
HOST="${HOST:-0.0.0.0}"

echo "=================================================================="
echo " INCLINE Watershed Delineation System"
echo " Dataset : $DATASET"
echo " Host    : $HOST"
echo " Port    : $PORT"
echo "=================================================================="

# Ensure release binary is compiled
if [ ! -f "target/release/incline-watershed-system" ]; then
    echo "Building release binary..."
    cargo build --release --bin incline-watershed-system
fi

exec ./target/release/incline-watershed-system --dataset "$DATASET" --port "$PORT" --host "$HOST"
