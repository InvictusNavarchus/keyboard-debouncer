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
set -euo pipefail

if [ "${EUID:-$(id -u)}" -ne 0 ]; then
    echo "Error: Installation requires root privileges to configure system files, udev rules," >&2
    echo "       and the dedicated 'kbd-debouncer' unprivileged system user." >&2
    echo "       Please run: sudo ./install.sh" >&2
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

if [ ! -f "target/release/keyboard-debouncer" ]; then
    echo "==> Release binary not found at target/release/keyboard-debouncer."
    if [ -n "${SUDO_USER:-}" ] && [ "$SUDO_USER" != "root" ]; then
        USER_HOME="$(getent passwd "$SUDO_USER" | cut -d: -f6)"
        CARGO_BIN="$USER_HOME/.cargo/bin"
        if command -v cargo >/dev/null 2>&1; then
            echo "==> Building release binary as '$SUDO_USER'..."
            sudo -u "$SUDO_USER" cargo build --release
        elif [ -x "$CARGO_BIN/cargo" ]; then
            echo "==> Building release binary as '$SUDO_USER' via $CARGO_BIN/cargo..."
            sudo -u "$SUDO_USER" env PATH="$CARGO_BIN:$PATH" cargo build --release
        fi
    elif command -v cargo >/dev/null 2>&1; then
        echo "==> Building release binary..."
        cargo build --release
    fi
fi

if [ ! -f "target/release/keyboard-debouncer" ]; then
    echo "Error: Could not build or locate target/release/keyboard-debouncer." >&2
    echo "Please build the project first as a regular user:" >&2
    echo "  cargo build --release" >&2
    echo "Then re-run the installer:" >&2
    echo "  sudo ./install.sh" >&2
    exit 1
fi

echo "==> Installing binary to /usr/local/bin..."
install -D -m 755 target/release/keyboard-debouncer /usr/local/bin/keyboard-debouncer

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
cat << 'EOF' > /etc/udev/rules.d/99-uinput.rules
KERNEL=="uinput", GROUP="input", MODE="0660"
EOF
if command -v udevadm >/dev/null 2>&1; then
    udevadm control --reload-rules 2>/dev/null || true
    udevadm trigger 2>/dev/null || true
fi

echo "==> Installing configuration..."
if [ ! -f /etc/debouncer.conf ]; then
    if [ -f debouncer.conf.example ]; then
        install -D -m 644 debouncer.conf.example /etc/debouncer.conf
        echo "    Created /etc/debouncer.conf from example. EDIT THIS FILE with your keyboard details!"
    fi
else
    echo "    /etc/debouncer.conf already exists, skipping overwrite."
fi

echo "==> Installing systemd service..."
install -D -m 644 keyboard-debouncer.service /etc/systemd/system/keyboard-debouncer.service
if command -v systemctl >/dev/null 2>&1; then
    systemctl daemon-reload
fi

echo ""
echo "Installation successful!"
echo "Next steps:"
echo "  1. Edit your settings: sudo nano /etc/debouncer.conf"
echo "  2. Enable and start:   sudo systemctl enable --now keyboard-debouncer"
echo "  3. Check status:       sudo systemctl status keyboard-debouncer"
