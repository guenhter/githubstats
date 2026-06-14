#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(dirname "$(realpath "$0")")

docker run \
    --detach \
    --name githubstats \
    --restart on-failure \
    -v "$SCRIPT_DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash load.sh
