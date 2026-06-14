#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR=$(dirname "$(realpath "$0")")

docker container rm -f githubstats 2>/dev/null || true

docker container run \
    -d \
    --name githubstats \
    --restart on-failure \
    -v "$SCRIPT_DIR:/workspace" \
    -w /workspace \
    ubuntu:latest \
    bash -c "apt-get update -qq && apt-get install -y -qq ca-certificates && bash load.sh"
