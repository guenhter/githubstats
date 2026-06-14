#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(dirname "$(realpath "$0")")

docker rm -f githubstats 2>/dev/null || true

docker run \
    -d \
    --name githubstats \
    --restart on-failure \
    -v "$SCRIPT_DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash load.sh
