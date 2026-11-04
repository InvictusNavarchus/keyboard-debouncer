# Installation details

For the recommended automated systemd setup, follow the
[README](../README.md#recommended-setup). This guide covers build prerequisites,
alternative device discovery, foreground execution, and manual service installation.
Run repository commands from the repository root unless a step says otherwise.

## Build prerequisites

Install Rust and Cargo before building. The setup instructions also use `evtest`
to identify the keyboard and key names; install it with your distribution's package
manager if it is not already available.

**Platform: Linux only.** The daemon grabs `/dev/input/event*` through `evdev`, re-injects
events via `/dev/uinput`, and ships a udev rule plus a systemd unit — none of which exist
on macOS or Windows. The `evdev` crate is Linux-only, so the build fails on those
platforms whatever toolchain is installed. WSL2 compiles it, but has no physical keyboard
to grab.

You also need a C compiler and development headers, required by `libsqlite3-sys` to build
the bundled SQLite. Install the standard toolchain for your distribution:

- **Ubuntu / Debian / Pop!\_OS:**

  ```bash
  sudo apt update && sudo apt install build-essential
  ```

- **Fedora / RHEL (Fedora 41+ / DNF5):**

  ```bash
  sudo dnf group install development-tools
  ```

- **Arch Linux / Manjaro:**
  ```bash
  sudo pacman -S base-devel
  ```

## Other ways to identify the keyboard

`sudo evtest` shows device names and the `KEY_*` names used in the configuration.
Select your physical keyboard, press the affected keys, note their names, and
exit with Ctrl-C before starting the debouncer.

You can also list device names with:

```bash
libinput list-devices
```

Or inspect the kernel's input device names:

```bash
grep -r '' /sys/class/input/event*/device/name
```

Prefer `KEYBOARD_NAME` so the daemon can rediscover the device when its event
number changes. See [device selection](configuration.md#device-selection) if you
need a direct device path.

## What the installer configures

`sudo ./install.sh` installs the binary you have already built. It:

- Installs the release binary to `/usr/local/bin/keyboard-debouncer`.
- Configures the `uinput` module to load at boot.
- Creates the `kbd-debouncer` system user and adds it to the `input` group.
- Grants the `input` group access to `/dev/uinput` through a udev rule.
- Creates `/etc/debouncer.conf` from your local `debouncer.conf`, if present,
  otherwise from the example. An existing `/etc/debouncer.conf` is preserved.
- Installs the [systemd unit](../keyboard-debouncer.service), reloads systemd,
  and restarts the service if it is already running.

On a fresh installation, edit `/etc/debouncer.conf` and enable the service as
shown in the README. The installer does not start an inactive service.

The installer uses `sudo` to configure system files, accounts, and device
permissions. The service runs as the unprivileged `kbd-debouncer` user with
`ProtectSystem=strict`, `ProtectHome=true`, and `NoNewPrivileges=true`.

### Installing a binary from another location

By default, the installer uses `target/release/keyboard-debouncer`. To install a
binary you built elsewhere or obtained separately:

```bash
sudo ./install.sh --binary /path/to/keyboard-debouncer
```

### Upgrade behavior

Re-running the installer preserves `/etc/debouncer.conf` and creates the service
account only if absent. The udev rule and module-load entry are rewritten to their
expected contents on each run.

An active service is restarted to pick up the installed binary. Replacing the
binary on disk does not change the executable used by an existing process, and
`systemctl daemon-reload` only reloads unit definitions. The daemon logs its
version at startup; inspect `journalctl -u keyboard-debouncer -b` to check which
version a run used.

## Running in the foreground

Use this mode for a temporary run or debugging. Stop any service instance first
so two processes do not compete for the same keyboard:

```bash
sudo systemctl stop keyboard-debouncer
```

If you have not installed the service, skip that command. Build and create a
local configuration:

```bash
cargo build --release
cp debouncer.conf.example debouncer.conf
```

Foreground execution needs access to both the physical device and `/dev/uinput`.
If no service installation has provisioned them, follow the module and udev steps
under [manual installation](#manual-installation) before launching the daemon.

Set `KEYBOARD_NAME` and `KEYS` in `debouncer.conf`, then run:

```bash
sudo ./target/release/keyboard-debouncer --config ./debouncer.conf
```

Exit with Ctrl-C. If you stopped the service for this run, start it again afterward:

```bash
sudo systemctl start keyboard-debouncer
```

For access without `sudo`, add your user to the `input` group and log out and
back in:

```bash
sudo usermod -aG input "$USER"
```

Group membership alone does not configure `/dev/uinput`; its module and
permissions must also be set up.

If no config path is specified, the daemon checks `debouncer.conf` in the current
directory, then `/etc/debouncer.conf`. Use `-c` or `--config <PATH>` to select a file,
`-h`/`--help` for usage, and `-V`/`--version` for the build version. A positional
config path is also accepted.

## Manual installation

If you prefer setting up the components manually:

### 1 — Build and install the binary

```bash
cargo build --release
sudo install -m 755 target/release/keyboard-debouncer /usr/local/bin/
```

### 2 — Install the config

```bash
sudo cp debouncer.conf.example /etc/debouncer.conf
sudo nano /etc/debouncer.conf   # set KEYBOARD_NAME and KEYS at minimum
```

### 3 — Ensure `uinput` kernel module loads on boot

```bash
echo "uinput" | sudo tee /etc/modules-load.d/uinput.conf
sudo modprobe uinput
```

### 4 — Create a dedicated system user

The service runs as an unprivileged `kbd-debouncer` user instead of root.
This user only needs access to `/dev/input/event*` (via the `input` group) and
`/dev/uinput` (via the udev rule below).

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
    --groups input --comment "keyboard-debouncer daemon" kbd-debouncer
```

### 5 — Grant access to `/dev/uinput`

The `input` group covers reading real devices, but `/dev/uinput` (used to create
the virtual keyboard) is typically owned by `root`. Add a udev rule to fix this:

```bash
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' \
    | sudo tee /etc/udev/rules.d/99-keyboard-debouncer.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

> **Note**: If `/dev/uinput` is not in the `input` group even after the udev rule
> (some distributions handle this differently), you can add `kbd-debouncer` to the
> group that owns it, or fall back to running the service as `root` by removing the
> `User=` and `Group=` lines from the unit file.

### 6 — Install and enable the service

```bash
sudo install -m 644 keyboard-debouncer.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now keyboard-debouncer
```

> [!IMPORTANT]
> These steps assume a **fresh** install. If you are repeating them to upgrade an
> existing one, `enable --now` will not restart a unit that is already running, so
> the daemon carries on executing the old binary. Finish with an explicit restart:
>
> ```bash
> sudo systemctl restart keyboard-debouncer
> ```
>
> See [Upgrading](../README.md#upgrading) for the recommended upgrade procedure.

### 7 — Verify it's running

```bash
sudo systemctl status keyboard-debouncer
```

## Uninstall details

The [README](../README.md#uninstalling) covers normal removal and `--purge`.
The uninstaller deliberately leaves these resources alone:

- **`/etc/modules-load.d/uinput.conf` is never removed**, only reported. `uinput`
  is shared infrastructure and other tools may rely on it loading at boot, and
  because `uinput` is the only sensible content for that file there is no way to
  tell ours from one you or another package wrote. Deleting it wrongly would
  break unrelated software one reboot later with nothing pointing back here, so
  it stays. Remove it by hand if nothing else on the system needs `/dev/uinput`.
- **A `kbd-debouncer` account this installer did not create.** Ownership is
  checked against the comment field `install.sh` sets, not the name alone.
- **The `input` group**, which comes from your distribution rather than from this
  project.

If a copy of the daemon is still running outside systemd — started by hand
rather than through the service — the uninstaller warns and tells you how to
stop it, but continues.

[Back to the README](../README.md)
