# keyboard-debouncer

Stops unwanted repeated characters caused by keyboard chatter. This Linux daemon
reads your physical keyboard, filters repeated presses on the keys you choose,
and forwards clean events through a virtual keyboard using `evdev` and `uinput`.

Set your keyboard name once; the daemon finds its input device again after
reconnects. Optional health tracking records key events locally for later analysis.

## Prerequisites

- Linux with systemd for the recommended setup below.
- Rust and Cargo when building from source.
- A C compiler and development headers when building the bundled SQLite library from source.
- `evtest` to identify your keyboard and affected keys.

See [build prerequisites](docs/installation.md#build-prerequisites) for distribution
packages. Basic filtering needs neither Bun nor a tracker database.

## Recommended setup

Install the systemd service so filtering starts at boot and runs without an open
terminal. The installer configures device permissions; the service runs as the
unprivileged `kbd-debouncer` user.

### 1. Identify your keyboard and affected keys

```bash
sudo evtest
```

Select your physical keyboard and press the keys that chatter. Note its device
name and the affected `KEY_*` names, such as `KEY_K`. Exit with Ctrl-C.

### 2. Build and install

```bash
git clone https://github.com/InvictusNavarchus/keyboard-debouncer.git
cd keyboard-debouncer
cargo build --release
sudo ./install.sh
```

If you already have a checkout, run the last two commands from its root.
The installer creates `/etc/debouncer.conf` and installs the service. Existing
configuration is preserved.

For releases with prebuilt Linux x86_64 assets, you can install without a Rust or
C toolchain. See [prebuilt installation](docs/installation.md#prebuilt-binaries)
for downloads, checksum verification, and the installer command.

### 3. Set your keyboard and keys

```bash
sudo nano /etc/debouncer.conf
```

Set `KEYBOARD_NAME` to the exact name from `evtest` and `KEYS` to the keys that
chatter. These two fields are enough for basic filtering:

```ini
KEYBOARD_NAME=your-keyboard-name-in-evtest
KEYS=KEY_K,KEY_L
```

Start with the default thresholds and leave tracking disabled. You can tune the
filter after checking how it behaves with your keyboard and typing speed.

### 4. Start and verify

```bash
sudo systemctl enable --now keyboard-debouncer
sudo systemctl status keyboard-debouncer
sudo journalctl -u keyboard-debouncer -b -n 30 --no-pager
```

Look for `Device grabbed.` and `Running…` in the logs. An active service may
still be waiting for a keyboard; if it reports `Device not found`, check that
`KEYBOARD_NAME` matches and the keyboard is connected.

Try normal typing and intentional double letters on the affected keys. If a
wanted second press disappears, follow the [threshold tuning guidance](docs/configuration.md#tuning-thresholds).
The defaults can suppress legitimate rapid same-key presses, especially for fast
typists, so this check is part of setup.

For manual installation or a temporary foreground run, see
[installation details](docs/installation.md).

## Everyday operations

Edit `/etc/debouncer.conf` to change the keyboard, keys, or thresholds, then
restart the service to apply the changes.

| Task                          | Command                                     |
| ----------------------------- | ------------------------------------------- |
| Restart after a config change | `sudo systemctl restart keyboard-debouncer` |
| Check status                  | `sudo systemctl status keyboard-debouncer`  |
| Follow logs                   | `sudo journalctl -u keyboard-debouncer -f`  |
| Show logs since boot          | `sudo journalctl -u keyboard-debouncer -b`  |
| Stop filtering                | `sudo systemctl stop keyboard-debouncer`    |
| Disable autostart             | `sudo systemctl disable keyboard-debouncer` |
| Check installed version       | `keyboard-debouncer --version`              |

### Upgrading

If tracking was enabled on an older version, ensure `KEYBOARD_ID` is set before
upgrading; see [tracking and migration details](docs/health-tracking.md#upgrading-existing-databases).

From the repository root:

```bash
git pull --ff-only
cargo build --release
sudo ./install.sh
```

Your `/etc/debouncer.conf` is preserved. The installer restarts an active service
to load the new binary. Check the startup version in the logs to verify the upgrade.

### Uninstalling

From the repository root:

```bash
sudo ./uninstall.sh
```

This stops and disables the service and removes its installed components.
Your configuration and tracker database are kept. To delete those too, run
`sudo ./uninstall.sh --purge`, which asks for confirmation before deletion.
See [uninstall details](docs/installation.md#uninstall-details) for shared resources
that are deliberately retained.

## Optional health tracking

To record key events for analysis, add these settings to `/etc/debouncer.conf`,
using a unique label for your physical keyboard:

```ini
TRACK_DB=/var/lib/keyboard-debouncer/keys.db
KEYBOARD_ID=keychron-k8-desk-01
```

Restart the service to begin recording:

```bash
sudo systemctl restart keyboard-debouncer
```

Use the keyboard normally to collect data. With Bun installed, run the report
from the repository root:

```bash
bun run analyze
```

The report covers today in local time and shows suppressed presses and the most
affected keys. It uses `sudo` to read the private database. Tracking records all
keys, including those outside your configured targets; a suppressed event is a
filter decision, not proof of a defective switch.

See [health tracking and reports](docs/health-tracking.md) for longer reporting
windows, tuning comparisons, database privacy, and recording details.

## Further documentation

- [Installation details](docs/installation.md): build packages, manual setup, and foreground execution.
- [Configuration reference](docs/configuration.md): all settings, device selection, and debounce-all mode.
- [Debounce algorithm and tuning](docs/debounce-algorithm.md): filtering tiers, default thresholds, and worked examples.
- [Health tracking and reports](docs/health-tracking.md): analytics options, timing measurements, and database history.

## Releases

See [CHANGELOG.md](CHANGELOG.md) for release notes and
[the release guide](docs/releasing.md) for the `cargo-release` workflow.

## License

This app is licensed under GPLv3
