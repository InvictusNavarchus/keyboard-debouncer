#!/usr/bin/env bash
# Launch keyboard-debouncer using settings from debouncer.conf.
#
# Config fields (see debouncer.conf.example for documentation):
#   KEYBOARD_NAME  — required; physical keyboard name as shown by evtest
#   KEYS           — required; comma-separated KEY_* names to debounce (e.g. KEY_K,KEY_L)
#   THRESHOLD_MS   — optional; debounce window in ms (default: 30)
#   LOG_FORWARD    — optional; true/false, log forwarded events (default: false)

set -euo pipefail

# ── Locate config file ──────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONF_FILE="${SCRIPT_DIR}/debouncer.conf"

if [ ! -f "$CONF_FILE" ]; then
    echo "Error: configuration file not found: $CONF_FILE" >&2
    echo "       Copy debouncer.conf.example to debouncer.conf and fill in your values." >&2
    exit 1
fi

# ── Helper: read a single key=value from the conf file ─────────────────────
# Usage: conf_get KEY
# Prints the value (everything after the first '='), or empty string if absent.
conf_get() {
    grep -m1 "^${1}=" "$CONF_FILE" | cut -d= -f2-
}

# ── KEYBOARD_NAME (required) ────────────────────────────────────────────────
KEYBOARD_NAME=$(conf_get KEYBOARD_NAME)
if [ -z "$KEYBOARD_NAME" ]; then
    echo "Error: KEYBOARD_NAME is required in $CONF_FILE" >&2
    exit 1
fi

# ── KEYS (required) ─────────────────────────────────────────────────────────
KEYS=$(conf_get KEYS)
if [ -z "$KEYS" ]; then
    echo "Error: KEYS is required in $CONF_FILE" >&2
    echo "       Specify which keys to debounce, e.g. KEYS=KEY_K,KEY_L" >&2
    echo "       Use KEY_* names exactly as shown by evtest." >&2
    exit 1
fi

# ── THRESHOLD_MS (optional, default: 30) ───────────────────────────────────
THRESHOLD_MS=$(conf_get THRESHOLD_MS)
if [ -z "$THRESHOLD_MS" ]; then
    THRESHOLD_MS=30
    echo "Note: THRESHOLD_MS not set in $CONF_FILE — using default: ${THRESHOLD_MS}ms" >&2
elif ! [[ "$THRESHOLD_MS" =~ ^[0-9]+$ ]] || [ "$THRESHOLD_MS" -eq 0 ]; then
    echo "Error: THRESHOLD_MS must be a positive integer, got: '$THRESHOLD_MS'" >&2
    exit 1
fi

# ── LOG_FORWARD (optional, default: false) ──────────────────────────────────
LOG_FORWARD=$(conf_get LOG_FORWARD)
if [ -z "$LOG_FORWARD" ]; then
    LOG_FORWARD=false
elif [ "$LOG_FORWARD" != "true" ] && [ "$LOG_FORWARD" != "false" ]; then
    echo "Error: LOG_FORWARD must be 'true' or 'false', got: '$LOG_FORWARD'" >&2
    exit 1
fi

# ── Locate the matching input event device ──────────────────────────────────
DEVICE=""
for dev in /sys/class/input/event*; do
    # Strip trailing whitespace/newlines — some kernels append a trailing space.
    name=$(tr -d '\n' < "$dev/device/name" | sed 's/[[:space:]]*$//')
    if [ "$name" = "$KEYBOARD_NAME" ]; then
        DEVICE="/dev/input/$(basename "$dev")"
        break
    fi
done

if [ -z "$DEVICE" ]; then
    echo "Error: no input device found with name '$KEYBOARD_NAME'" >&2
    echo "       Run 'evtest' to list connected devices and update KEYBOARD_NAME in $CONF_FILE." >&2
    exit 1
fi

# ── Build argument list ─────────────────────────────────────────────────────
DEBOUNCER_ARGS=("$DEVICE" "--keys" "$KEYS" "--threshold-ms" "$THRESHOLD_MS")
if [ "$LOG_FORWARD" = "true" ]; then
    DEBOUNCER_ARGS+=("--log-forward")
fi

# ── Launch ──────────────────────────────────────────────────────────────────
echo "Device   : $DEVICE ($KEYBOARD_NAME)"
echo "Keys     : $KEYS"
echo "Threshold: ${THRESHOLD_MS}ms"
echo "Log fwd  : $LOG_FORWARD"
exec sudo "${SCRIPT_DIR}/target/release/keyboard-debouncer" "${DEBOUNCER_ARGS[@]}"
