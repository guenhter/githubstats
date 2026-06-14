#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

docker run --rm \
    -v "$DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash load.sh
