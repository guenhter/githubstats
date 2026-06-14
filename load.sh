#!/usr/bin/env bash
# Usage: source .env && ./load.sh
set -uo pipefail

YEARS=(2026)  # add more years as needed, e.g. (2024 2025 2026)

for year in "${YEARS[@]}"; do
    for month in 01 02 03 04 05 06 07 08 09 10 11 12; do
        ym="${year}${month}"
        archive="data/archive-${ym}.csv"

        echo "--- ${year}-${month} ---"

        if [[ -f "$archive" ]]; then
            echo "  [skip] $archive already exists"
        else
            ./github_archive_loader --year "$year" --month "$((10#$month))" --parallelism 10 --output "$archive"
        fi
    done
done
