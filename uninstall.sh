#!/usr/bin/env bash
#
# keyboard-debouncer system uninstaller — reverses install.sh.
#
# Requires root for the same reasons the installer does: it removes system
# files, the udev rule, and the dedicated 'kbd-debouncer' system user.
#
# /etc/modules-load.d/uinput.conf is deliberately never removed — see the
# reasoning at that step.
#
# By default this removes only what the installer provisioned and leaves YOUR
# data alone — /etc/debouncer.conf and the tracker database under
# /var/lib/keyboard-debouncer/ are preserved. Everything the installer creates
# can be recreated in seconds; a hand-tuned config and an accumulated chatter
# history cannot. Pass --purge to delete those too.
#
set -euo pipefail

SERVICE_NAME=keyboard-debouncer
BINARY_PATH=/usr/local/bin/keyboard-debouncer
UNIT_PATH=/etc/systemd/system/keyboard-debouncer.service
CONFIG_PATH=/etc/debouncer.conf
STATE_DIR=/var/lib/keyboard-debouncer
DAEMON_USER=kbd-debouncer

# Kept in sync by hand with install.sh. Duplicated rather than sourced from a
# shared file so this script stays standalone — it must work when fetched on its
# own, without the rest of the tree.
MODULES_LOAD_PATH=/etc/modules-load.d/uinput.conf
UDEV_RULE_PATH=/etc/udev/rules.d/99-keyboard-debouncer.rules
LEGACY_UDEV_RULE_PATH=/etc/udev/rules.d/99-uinput.rules
UDEV_RULE_CONTENT='KERNEL=="uinput", GROUP="input", MODE="0660"'

PURGE=0
for arg in "$@"; do
    case "$arg" in
        --purge) PURGE=1 ;;
        -h|--help)
            echo "Usage: sudo ./uninstall.sh [--purge]"
            echo ""
            echo "Removes the binary, systemd unit, udev rule, boot-time module"
            echo "entry, and the '$DAEMON_USER' system user."
            echo ""
            echo "  --purge   Also delete $CONFIG_PATH and"
            echo "            $STATE_DIR (your tuned configuration"
            echo "            and the key health tracker database). Irreversible."
            exit 0
            ;;
        *)
            echo "Error: unknown option '$arg'. Use --help for usage." >&2
            exit 1
            ;;
    esac
done

if [ "${EUID:-$(id -u)}" -ne 0 ]; then
    echo "Error: Uninstallation requires root privileges to remove system files," >&2
    echo "       the udev rule, and the '$DAEMON_USER' system user." >&2
    echo "       Please run: sudo ./uninstall.sh" >&2
    exit 1
fi

if [ "$PURGE" -eq 1 ]; then
    echo "--purge will PERMANENTLY delete:"
    if [ -f "$CONFIG_PATH" ]; then
        echo "    $CONFIG_PATH (your tuned configuration)"
    fi
    if [ -d "$STATE_DIR" ]; then
        echo "    $STATE_DIR (the key health tracker database)"
    fi
    echo ""
    printf "Proceed? [y/N] "
    # A closed stdin makes `read` fail, which under `set -e` aborts before any
    # deletion — the right default for an irreversible action run unattended.
    read -r reply
    case "$reply" in
        [yY] | [yY][eE][sS]) ;;
        *)
            echo "Aborted. Nothing was removed."
            exit 1
            ;;
    esac
fi

# Stop the daemon before taking anything away from it. It holds an exclusive
# grab (EVIOCGRAB) on the physical keyboard, so a running instance keeps
# swallowing every keystroke it filters regardless of what we delete on disk.
if command -v systemctl >/dev/null 2>&1; then
    if systemctl is-active --quiet "$SERVICE_NAME"; then
        echo "==> Stopping $SERVICE_NAME..."
        systemctl stop "$SERVICE_NAME"
    fi
    if systemctl is-enabled "$SERVICE_NAME" >/dev/null 2>&1; then
        echo "==> Disabling $SERVICE_NAME..."
        systemctl disable "$SERVICE_NAME" >/dev/null
    fi
fi

if [ -f "$UNIT_PATH" ]; then
    echo "==> Removing $UNIT_PATH..."
    rm -f "$UNIT_PATH"
fi
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload
    # Clears a lingering 'failed' state so the removed unit stops showing up in
    # `systemctl --failed`. Absent unit means nothing to reset, hence the guard.
    systemctl reset-failed "$SERVICE_NAME" >/dev/null 2>&1 || true
fi

if [ -f "$BINARY_PATH" ]; then
    echo "==> Removing $BINARY_PATH..."
    rm -f "$BINARY_PATH"
fi

echo "==> Removing udev rule..."
rm -f "$UDEV_RULE_PATH"
# Releases up to v0.1.0 used a device-named rule file that we do not own. Remove
# it only when it is byte-identical to what those releases wrote, so a
# hand-edited or third-party file of the same name survives.
if [ -f "$LEGACY_UDEV_RULE_PATH" ] \
    && [ "$(cat "$LEGACY_UDEV_RULE_PATH")" = "$UDEV_RULE_CONTENT" ]; then
    rm -f "$LEGACY_UDEV_RULE_PATH"
fi
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger 2>/dev/null || true
fi

# uinput is shared infrastructure: other tools (input remappers, controller
# drivers) may depend on this entry force-loading it at boot, and /dev/uinput
# cannot be opened to trigger an on-demand load because the node does not exist
# until the module is in.
#
# Unlike the udev rule, this file carries no fingerprint: 'uinput' is the only
# plausible content, so it cannot distinguish ours from one a user or another
# package wrote. The harm is asymmetric — leaving seven bytes behind costs
# tidiness, while deleting a file we did not own breaks unrelated software at
# the next boot with almost no diagnostic trail. So it always stays.
if [ -f "$MODULES_LOAD_PATH" ]; then
    echo "==> Leaving $MODULES_LOAD_PATH in place."
    echo "    Other software may rely on uinput loading at boot. Remove it by"
    echo "    hand if nothing else on this system needs /dev/uinput."
fi

if [ "$PURGE" -eq 1 ]; then
    if [ -f "$CONFIG_PATH" ]; then
        echo "==> Removing $CONFIG_PATH..."
        rm -f "$CONFIG_PATH"
    fi
    if [ -d "$STATE_DIR" ]; then
        echo "==> Removing $STATE_DIR..."
        rm -rf "$STATE_DIR"
    fi
elif [ -d "$STATE_DIR" ]; then
    # Hand preserved data to root before the owning account disappears. Files
    # left owned by an unmapped uid would be inherited by whichever account a
    # future useradd assigns that uid to — which for a keystroke database means
    # handing an unrelated service read access to it.
    echo "==> Preserving $STATE_DIR; reassigning ownership to root..."
    chown -R root:root "$STATE_DIR"
fi

if id -u "$DAEMON_USER" >/dev/null 2>&1; then
    echo "==> Removing system user '$DAEMON_USER'..."
    userdel "$DAEMON_USER"
fi

echo ""
echo "Uninstallation complete."
if [ "$PURGE" -eq 0 ]; then
    preserved=0
    if [ -f "$CONFIG_PATH" ] || [ -d "$STATE_DIR" ]; then
        preserved=1
    fi
    if [ "$preserved" -eq 1 ]; then
        echo ""
        echo "Preserved (remove with 'sudo ./uninstall.sh --purge'):"
        if [ -f "$CONFIG_PATH" ]; then
            echo "    $CONFIG_PATH"
        fi
        if [ -d "$STATE_DIR" ]; then
            echo "    $STATE_DIR"
        fi
    fi
fi
