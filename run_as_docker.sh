#!/usr/bin/env bash
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

until docker run --rm \
    -v "$DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash load.sh
do
    echo "Container exited with failure — restarting in 5s..."
    sleep 5
done
