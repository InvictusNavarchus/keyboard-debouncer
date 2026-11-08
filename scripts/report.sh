#!/usr/bin/env bash
set -euo pipefail

if ! command -v bun >/dev/null 2>&1; then
    echo "Bun is required for optional reports: https://bun.sh" >&2
    exit 1
fi

exec sudo "$(command -v bun)" /usr/local/share/keyboard-debouncer/scripts/analyze.ts "$@"
