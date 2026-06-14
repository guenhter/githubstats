#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

docker run \
    --restart on-failure \
    -v "$DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash load.sh
