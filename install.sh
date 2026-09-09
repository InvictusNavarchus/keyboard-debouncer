#!/usr/bin/env bash
#
# keyboard-debouncer system installer
#
# Note on privileges:
# This installer requires root (sudo) ONCE to provision system-level assets:
#   - Installing the binary to /usr/local/bin/
#   - Registering the 'uinput' kernel module at boot (/etc/modules-load.d/)
#   - Creating the dedicated unprivileged system user ('kbd-debouncer')
#   - Setting udev device permissions for /dev/uinput
#   - Installing the systemd service to /etc/systemd/system/
#
# Once installed, the daemon itself runs strictly UNPRIVILEGED as 'kbd-debouncer'
# with zero root access, sandboxed by systemd directives.
#
# This script installs; it does not build. Run `cargo build --release` as your
# normal user first, or point --binary at a prebuilt binary.
#
set -euo pipefail

DEFAULT_BINARY="target/release/keyboard-debouncer"
BINARY_SOURCE=""

usage() {
    echo "Usage: sudo ./install.sh [--binary <PATH>]"
    echo ""
    echo "  --binary <PATH>  Install this binary instead of $DEFAULT_BINARY."
    echo "                   Relative paths resolve against your current directory."
    echo "  -h, --help       Print this help."
}

# Parsed before the root check so --help works unprivileged.
while [ $# -gt 0 ]; do
    case "$1" in
        --binary)
            if [ $# -lt 2 ]; then
                echo "Error: --binary requires a path argument." >&2
                exit 1
            fi
            BINARY_SOURCE="$2"
            shift 2
            ;;
        --binary=*)
            BINARY_SOURCE="${1#--binary=}"
            shift
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            echo "Error: unknown option '$1'. Use --help for usage." >&2
            exit 1
            ;;
    esac
done

# Absolutise while we are still in the caller's directory: the cd below would
# otherwise silently reinterpret a relative path against the script's location.
if [ -n "$BINARY_SOURCE" ]; then
    case "$BINARY_SOURCE" in
        /*) ;;
        *) BINARY_SOURCE="$PWD/$BINARY_SOURCE" ;;
    esac
fi

if [ "${EUID:-$(id -u)}" -ne 0 ]; then
    echo "Error: Installation requires root privileges to configure system files, udev rules," >&2
    echo "       and the dedicated 'kbd-debouncer' unprivileged system user." >&2
    echo "       Please run: sudo ./install.sh" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Kept in sync by hand with uninstall.sh, which must know the same paths to be
# able to reverse this script while remaining standalone.
UDEV_RULE_PATH=/etc/udev/rules.d/99-keyboard-debouncer.rules
LEGACY_UDEV_RULE_PATH=/etc/udev/rules.d/99-uinput.rules
UDEV_RULE_CONTENT='KERNEL=="uinput", GROUP="input", MODE="0660"'

if [ -n "$BINARY_SOURCE" ]; then
    if [ ! -f "$BINARY_SOURCE" ]; then
        echo "Error: No binary at $BINARY_SOURCE." >&2
        exit 1
    fi
else
    BINARY_SOURCE="$DEFAULT_BINARY"
    if [ ! -f "$BINARY_SOURCE" ]; then
        echo "Error: Release binary not found at $DEFAULT_BINARY." >&2
        echo "       Build it as your normal user, then re-run the installer:" >&2
        echo "         cargo build --release" >&2
        echo "         sudo ./install.sh" >&2
        echo "       Or install a prebuilt binary with: --binary <PATH>" >&2
        exit 1
    fi
fi

echo "==> Installing binary to /usr/local/bin..."
install -D -m 755 "$BINARY_SOURCE" /usr/local/bin/keyboard-debouncer

echo "==> Ensuring 'uinput' kernel module loads on boot..."
mkdir -p /etc/modules-load.d
echo "uinput" > /etc/modules-load.d/uinput.conf
modprobe uinput 2>/dev/null || true

echo "==> Creating system user 'kbd-debouncer'..."
if ! id -u kbd-debouncer >/dev/null 2>&1; then
    useradd --system --no-create-home --shell /usr/sbin/nologin \
        --groups input --comment "keyboard-debouncer daemon" kbd-debouncer
    echo "    Created user 'kbd-debouncer' in group 'input'."
else
    usermod -aG input kbd-debouncer
    echo "    User 'kbd-debouncer' already exists; ensured membership in 'input'."
fi

echo "==> Setting up udev rules for /dev/uinput..."
mkdir -p /etc/udev/rules.d
printf '%s\n' "$UDEV_RULE_CONTENT" > "$UDEV_RULE_PATH"

# Releases up to v0.1.0 wrote this rule under a name describing the *device*
# rather than this package, so another package could legitimately own that path.
# Reclaim it only when its content is byte-identical to what we used to write;
# a hand-edited or third-party file with the same name is left alone. cmp
# rather than $(cat) because command substitution strips trailing newlines,
# which would match a file differing from ours by exactly that.
if [ -f "$LEGACY_UDEV_RULE_PATH" ] \
    && printf '%s\n' "$UDEV_RULE_CONTENT" | cmp -s - "$LEGACY_UDEV_RULE_PATH"; then
    rm -f "$LEGACY_UDEV_RULE_PATH"
    echo "    Removed superseded $LEGACY_UDEV_RULE_PATH."
fi

if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger 2>/dev/null || true
fi

if [ -d /var/lib/keyboard-debouncer ]; then
    echo "==> Securing existing /var/lib/keyboard-debouncer permissions..."
    chown -R kbd-debouncer:input /var/lib/keyboard-debouncer
    chmod 700 /var/lib/keyboard-debouncer
    chmod -R go-rwx /var/lib/keyboard-debouncer/* 2>/dev/null || true
fi

echo "==> Installing configuration..."
if [ ! -f /etc/debouncer.conf ]; then
    if [ -f debouncer.conf ]; then
        install -D -m 644 debouncer.conf /etc/debouncer.conf
        echo "    Copied existing debouncer.conf to /etc/debouncer.conf."
    elif [ -f debouncer.conf.example ]; then
        install -D -m 644 debouncer.conf.example /etc/debouncer.conf
        echo "    Created /etc/debouncer.conf from example. EDIT THIS FILE with your keyboard details!"
    fi
else
    echo "    /etc/debouncer.conf already exists, skipping overwrite."
fi

echo "==> Installing systemd service..."
install -D -m 644 keyboard-debouncer.service /etc/systemd/system/keyboard-debouncer.service

SERVICE_WAS_ACTIVE=0
if command -v systemctl >/dev/null 2>&1; then
    if systemctl is-active --quiet keyboard-debouncer; then
        SERVICE_WAS_ACTIVE=1
    fi
    systemctl daemon-reload
fi

# The `install` above replaced the inode at /usr/local/bin, but an already-running
# process keeps executing the old inode, and `daemon-reload` only re-reads unit
# files — neither picks up the new binary. Without this explicit restart an
# upgrade reports success while the old code keeps running.
if [ "$SERVICE_WAS_ACTIVE" -eq 1 ]; then
    echo "==> Service is running; restarting to activate the new binary..."
    systemctl restart keyboard-debouncer
fi

echo ""
if [ "$SERVICE_WAS_ACTIVE" -eq 1 ]; then
    echo "Upgrade successful — service restarted on the new binary."
    echo "Verify with:"
    echo "  keyboard-debouncer --version"
    echo "  sudo systemctl status keyboard-debouncer"
else
    echo "Installation successful!"
    echo "Next steps:"
    echo "  1. Edit your settings: sudo nano /etc/debouncer.conf"
    echo "  2. Enable and start:   sudo systemctl enable --now keyboard-debouncer"
    echo "  3. Check status:       sudo systemctl status keyboard-debouncer"
fi
