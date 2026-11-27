#!/usr/bin/env bash
# Capture docs/screenshot.png for the README hero.
# Requires Node.js. Installs Playwright (and Chromium) on first run.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

OUT="${1:-docs/screenshot.png}"

if [[ ! -d node_modules/playwright ]]; then
  echo "[screenshot] installing playwright…" >&2
  npm install --no-fund --no-audit
fi

# Ensure browser binaries are present (no-op if already downloaded).
npx playwright install chromium

node scripts/capture_readme_screenshot.mjs "$OUT"
