#!/usr/bin/env bash
#
# keyboard-debouncer system installer
#
# Note on privileges:
# This installer requires root (sudo) for installation and updates to provision system-level assets:
#   - Installing the binary to /usr/local/bin/
#   - Registering the 'uinput' kernel module at boot (/etc/modules-load.d/)
#   - Creating the dedicated unprivileged system user ('kbd-debouncer')
#   - Setting udev device permissions for /dev/uinput
#   - Installing the systemd service to /etc/systemd/system/
#
# Once installed, the daemon itself runs strictly UNPRIVILEGED as 'kbd-debouncer'
# with zero root access, sandboxed by systemd directives.
#
# With no --binary option, install the bundled executable or download the latest
# release. Downloads and checksum verification run before requesting sudo.
# Source builds are explicit: --binary ./target/release/keyboard-debouncer.
#
set -euo pipefail

REPO="InvictusNavarchus/keyboard-debouncer"
BINARY_SOURCE=""
TMP_DIR=""
trap 'if [ -n "$TMP_DIR" ]; then rm -rf "$TMP_DIR"; fi' EXIT

usage() {
    echo "Usage: bash install.sh [--binary <PATH>]"
    echo ""
    echo "Downloads the latest release, or uses the executable beside this installer."
    echo "Run the same command again to update. VERSION=vX.Y.Z selects a release."
    echo "  --binary <PATH>  Install a local binary, with no download."
    echo "  -h, --help       Print this help."
}

die() {
    echo "Error: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --binary)
            [ $# -ge 2 ] && [ -n "$2" ] || die "--binary requires a path."
            BINARY_SOURCE="$2"
            shift 2
            ;;
        --binary=*)
            BINARY_SOURCE="${1#--binary=}"
            [ -n "$BINARY_SOURCE" ] || die "--binary requires a path."
            shift
            ;;
        -h | --help) usage; exit 0 ;;
        *) die "Unknown option '$1'. Use --help for usage." ;;
    esac
done

SCRIPT_DIR=""
if [ -n "${BASH_SOURCE[0]:-}" ] && [ -f "${BASH_SOURCE[0]}" ]; then
    SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fi

[ "$(uname -s)" = Linux ] || die "keyboard-debouncer only runs on Linux."
command -v systemctl >/dev/null 2>&1 || die "systemd is required for service installation."

if [ -z "$BINARY_SOURCE" ] && [ -z "${VERSION:-}" ] && [ -n "$SCRIPT_DIR" ] && [ -x "$SCRIPT_DIR/keyboard-debouncer" ]; then
    BINARY_SOURCE="$SCRIPT_DIR/keyboard-debouncer"
fi

if [ -z "$BINARY_SOURCE" ]; then
    for tool in curl tar sha256sum; do
        command -v "$tool" >/dev/null 2>&1 || die "$tool is required to download and verify releases."
    done
    case "$(uname -m)" in
        x86_64 | amd64) target=x86_64-unknown-linux-musl ;;
        *) die "Prebuilt releases support Linux x86_64. Build from source and use --binary on other architectures." ;;
    esac
    version="${VERSION:-latest}"
    if [ "$version" = latest ]; then
        url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")
        version="${url##*/}"
    fi
    [[ "$version" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$ ]] || die "Expected a release tag such as v0.5.0; got '$version'."
    package="keyboard-debouncer-$version-$target"
    archive="$package.tar.gz"
    base="https://github.com/$REPO/releases/download/$version"
    TMP_DIR=$(mktemp -d)
    echo "==> Downloading $version ($target)..."
    curl -fsSL "$base/$archive" -o "$TMP_DIR/$archive"
    curl -fsSL "$base/$archive.sha256" -o "$TMP_DIR/$archive.sha256"
    (cd "$TMP_DIR" && sha256sum --check "$archive.sha256") || die "Release checksum verification failed."
    tar -xzf "$TMP_DIR/$archive" -C "$TMP_DIR"
    [ -f "$TMP_DIR/$package/install.sh" ] || die "The release archive has no installer."
    # Use the installer and service files from the verified release together.
    if [ "$(id -u)" -eq 0 ]; then
        bash "$TMP_DIR/$package/install.sh" --binary "$TMP_DIR/$package/keyboard-debouncer"
    else
        command -v sudo >/dev/null 2>&1 || die "sudo is required to install system files."
        sudo bash "$TMP_DIR/$package/install.sh" --binary "$TMP_DIR/$package/keyboard-debouncer"
    fi
    exit 0
fi

case "$BINARY_SOURCE" in
    /*) ;;
    *) BINARY_SOURCE="$PWD/$BINARY_SOURCE" ;;
esac
[ -f "$BINARY_SOURCE" ] && [ -x "$BINARY_SOURCE" ] || die "No executable binary at $BINARY_SOURCE."
[ -n "$SCRIPT_DIR" ] || die "--binary requires a local installer and its companion files."
cd "$SCRIPT_DIR"
for file in keyboard-debouncer.service debouncer.conf.example uninstall.sh scripts/analyze.ts scripts/timing.ts scripts/report.sh README.md CHANGELOG.md LICENSE package.json; do
    [ -f "$file" ] || die "Missing installation file: $file. Use the complete release bundle or source checkout."
done
[ -d docs ] || die "Missing installation documentation. Use the complete release bundle or source checkout."

if [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "sudo is required to install system files."
    exec sudo bash "$SCRIPT_DIR/install.sh" --binary "$BINARY_SOURCE"
fi

UDEV_RULE_PATH=/etc/udev/rules.d/99-keyboard-debouncer.rules
LEGACY_UDEV_RULE_PATH=/etc/udev/rules.d/99-uinput.rules
UDEV_RULE_CONTENT='KERNEL=="uinput", GROUP="input", MODE="0660"'

# Preserve operator masks and refuse to overwrite foreign unit links.
UNIT_PATH=/etc/systemd/system/keyboard-debouncer.service
UNIT_MASKED=0
if command -v systemctl >/dev/null 2>&1; then
    UNIT_STATE=$(systemctl is-enabled keyboard-debouncer.service 2>/dev/null || true)
    case "$UNIT_STATE" in masked|masked-runtime) UNIT_MASKED=1 ;; esac
fi
if [ -L "$UNIT_PATH" ]; then
    if [ "$(readlink -f "$UNIT_PATH")" = /dev/null ]; then
        UNIT_MASKED=1
    else
        echo "Error: Refusing to replace symlink unit $UNIT_PATH." >&2
        exit 1
    fi
elif [ -e "$UNIT_PATH" ] && [ ! -f "$UNIT_PATH" ]; then
    echo "Error: Refusing to replace non-regular unit $UNIT_PATH." >&2
    exit 1
elif [ -f "$UNIT_PATH" ] && [ ! -s "$UNIT_PATH" ]; then
    UNIT_MASKED=1
fi

echo "==> Installing binary to /usr/local/bin..."
# install -d creates traversable parents even with a restrictive root umask,
# but must not reset an existing administrator-managed destination's mode.
if [ ! -d /usr/local/bin ]; then
    install -d -m 755 /usr/local/bin
fi
INSTALL_STAGE=$(mktemp -d /usr/local/bin/.keyboard-debouncer-install.XXXXXX)
trap 'rm -rf -- "$INSTALL_STAGE"' EXIT
install -m 755 "$BINARY_SOURCE" "$INSTALL_STAGE/keyboard-debouncer"
mv -fT "$INSTALL_STAGE/keyboard-debouncer" /usr/local/bin/keyboard-debouncer

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

echo "==> Installing reporting helpers, documentation, and uninstaller..."
SHARE_DIR=/usr/local/share/keyboard-debouncer
install -d -m 755 "$SHARE_DIR/scripts"
install -m 644 scripts/analyze.ts scripts/timing.ts "$SHARE_DIR/scripts/"
install -m 644 package.json README.md CHANGELOG.md LICENSE "$SHARE_DIR/"
cp -R docs "$SHARE_DIR/"
install -D -m 755 scripts/report.sh /usr/local/bin/keyboard-debouncer-report
install -D -m 755 uninstall.sh /usr/local/bin/keyboard-debouncer-uninstall

echo "==> Installing systemd service..."
if [ "$UNIT_MASKED" -eq 0 ]; then
    install -D -m 644 keyboard-debouncer.service "$UNIT_PATH"
else
    echo "    Existing service mask preserved."
fi

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
if [ "$SERVICE_WAS_ACTIVE" -eq 1 ] && [ "$UNIT_MASKED" -eq 0 ]; then
    echo "==> Service is running; restarting to activate the new binary..."
    systemctl try-restart keyboard-debouncer
fi

echo ""
if [ "$SERVICE_WAS_ACTIVE" -eq 1 ] && [ "$UNIT_MASKED" -eq 0 ]; then
    echo "Upgrade successful — service restarted on the new binary."
    echo "Verify with:"
    echo "  keyboard-debouncer --version"
    echo "  sudo systemctl status keyboard-debouncer"
else
    echo "Installation successful!"
    echo "Next steps:"
    echo "  1. Edit your settings: sudo nano /etc/debouncer.conf"
    if [ "$UNIT_MASKED" -eq 1 ]; then
        echo "     Service definition was not replaced because a mask is in force."
        echo "     To activate, explicitly unmask and rerun this installer with the same --binary option first."
    fi
    echo "  2. Activation unchanged; when ready: sudo systemctl enable --now keyboard-debouncer"
    echo "     Enablement and drop-ins are preserved."
    echo "  3. Check status:       sudo systemctl status keyboard-debouncer"
fi
