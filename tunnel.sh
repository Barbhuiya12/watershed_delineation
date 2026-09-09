#!/usr/bin/env bash
# ==============================================================================
# INCLINE Watershed Delineation System — Public Test Tunnel
# Powered by Cloudflare Quick Tunnels (Zero setup, Instant HTTPS)
# ==============================================================================

set -euo pipefail

PORT="${PORT:-8787}"

echo "================================================================="
echo "   INCLINE Watershed Delineation — Public Test Tunnel"
echo "================================================================="
echo "Connecting local server on port ${PORT} to public internet..."
echo ""

if ! command -v cloudflared &> /dev/null; then
    echo "Error: cloudflared not found. Install it with: brew install cloudflared"
    exit 1
fi

cloudflared tunnel --url "http://127.0.0.1:${PORT}"
