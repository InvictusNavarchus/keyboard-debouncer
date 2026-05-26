# keyboard-debouncer

A CLI daemon for Linux that prevents keyboard chatter by intercepting events at the
OS level via `evdev` and `uinput`. It grabs your physical keyboard exclusively, filters
out high‑speed bounce, and re‑injects clean key events through a virtual device.

## Features

- **Normal debounce** – suppresses a re-press that arrives within `THRESHOLD_MS` after
  the last physical release.
- **Tiered extended debounce** – classifies every forwarded keypress by its hold duration
  into one of three tiers, each arming a progressively stricter debounce window for the
  next cycle:
  - *Micro* (`< MICRO_HOLD_THRESHOLD_MS`, default 20 ms) — hardware ghost contact; unambiguously chatter. Arms a 150 ms lockout.
  - *Short* (`< SHORT_HOLD_THRESHOLD_MS`, default 70 ms) — potentially suspicious hold. **Requires per-user calibration**: fast typists can produce holds in this range legitimately, causing false positives on rapid same-key repetition. Arms a 100 ms lockout.
  - *Normal* (≥ `SHORT_HOLD_THRESHOLD_MS`) — uses the base threshold.
- **Debounce all keys** (optional) – when enabled, all keys are debounced automatically
  instead of only a curated list. Modifier keys and controls (Shift, Ctrl, Alt, Meta,
  CapsLock, etc.) are intelligently excluded since they don't chatter and have different
  timing semantics.
- **Key health tracking** (optional) – passively records *every* key event (even
  non‑target keys) to an SQLite database. You can later query the data to identify
  switches that are starting to fail, **before** the chatter becomes noticeable.
- **Zero‑configuration discovery** – set your keyboard name once and the app auto finds the correct `/dev/input/eventX`

## How to use

1. **Find your keyboard** using `evtest`, or `libinput list-devices`, or `grep -r '' /sys/class/input/event*/device/name`
   Note the device name and the `KEY_*` names of the chattering keys.
2. **Build the binary**: `cargo build --release`
3. **Copy the example config**:
   ```
   cp debouncer.conf.example debouncer.conf
   ```
4. **Edit `debouncer.conf`** – provide at minimum `KEYBOARD_NAME` (or
   `DEVICE_PATH`) and the list of `KEYS`. All other fields have sensible
   defaults. See the table below.
5. **Launch** (root or input‑group member required):
   ```
   sudo ./target/release/keyboard-debouncer
   ```
   If you place the config at `/etc/debouncer.conf`, the daemon will also find
   it without an explicit argument.

   > **Tip**: Add your user to the `input` group so you can run the daemon
   > without `sudo` after a one‑time setup:
   > ```
   > sudo usermod -aG input $USER   # log out and back in
   > ```
   > Alternatively, create a udev rule that gives the `input` group read/write
   > access to `/dev/input/event*`.

## Running as a systemd service

Running the debouncer as a systemd service means it starts automatically at boot,
restarts on failure, and its output goes to the journal — no terminal session needed.

A ready-to-use service unit is included at [`keyboard-debouncer.service`](keyboard-debouncer.service).

### 1 — Build and install the binary

```bash
cargo build --release
sudo cp target/release/keyboard-debouncer /usr/local/bin/
sudo chmod 755 /usr/local/bin/keyboard-debouncer
```

### 2 — Install the config

```bash
sudo cp debouncer.conf.example /etc/debouncer.conf
sudo nano /etc/debouncer.conf   # set KEYBOARD_NAME and KEYS at minimum
```

### 3 — Create a dedicated system user

The service runs as an unprivileged `kbd-debouncer` user instead of root.
This user only needs access to `/dev/input/event*` (via the `input` group) and
`/dev/uinput` (via a udev rule below).

```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin \
    --groups input --comment "keyboard-debouncer daemon" kbd-debouncer
```

### 4 — Grant access to `/dev/uinput`

The `input` group covers reading real devices, but `/dev/uinput` (used to create
the virtual keyboard) is typically owned by `root`. Add a udev rule to fix this:

```bash
echo 'KERNEL=="uinput", GROUP="input", MODE="0660"' \
    | sudo tee /etc/udev/rules.d/99-uinput.rules
sudo udevadm control --reload-rules && sudo udevadm trigger
```

> **Note**: If `/dev/uinput` is not in the `input` group even after the udev rule
> (some distributions handle this differently), you can add `kbd-debouncer` to the
> group that owns it, or fall back to running the service as `root` by removing the
> `User=` and `Group=` lines from the unit file.

### 5 — Install and enable the service

```bash
sudo cp keyboard-debouncer.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now keyboard-debouncer
```

### 6 — Verify it's running

```bash
sudo systemctl status keyboard-debouncer
```

### Viewing logs

```bash
# Stream live output
journalctl -u keyboard-debouncer -f

# Show all logs since last boot
journalctl -u keyboard-debouncer -b
```

### Common operations

| Task | Command |
|------|---------|
| Restart after a config change | `sudo systemctl restart keyboard-debouncer` |
| Stop the daemon | `sudo systemctl stop keyboard-debouncer` |
| Disable autostart | `sudo systemctl disable keyboard-debouncer` |
| Check status | `sudo systemctl status keyboard-debouncer` |

### Health tracker database path

If you enable `TRACK_DB` in your config, use the path the service pre-creates for you:

```
TRACK_DB=/var/lib/keyboard-debouncer/keys.db
```

The `StateDirectory=keyboard-debouncer` directive in the unit file ensures
`/var/lib/keyboard-debouncer/` is created and owned by `kbd-debouncer` on first start.

---

## Configuration (`debouncer.conf`)

| Field                          | Required?      | Description |
|--------------------------------|----------------|-------------|
| `KEYBOARD_NAME`                | 1 of these 2   | Keyboard name as shown by `evtest` — used to auto-discover the event node. |
| `DEVICE_PATH`                  | 1 of these 2   | Direct path, e.g. `/dev/input/event10`. Overrides `KEYBOARD_NAME` if both are set. |
| `KEYS`                         | **Yes**        | Comma-separated keys to debounce, using `KEY_*` names from `evtest` (example: `KEY_K,KEY_L,KEY_ENTER`). Ignored if `DEBOUNCE_ALL_KEYS` is set. |
| `DEBOUNCE_ALL_KEYS`            | No             | `true` / `false` — debounce all keyboard keys automatically. Modifiers always excluded. Default: `false`. |
| `THRESHOLD_MS`                 | No             | Base debounce window in ms. Any re-press within this window of the last release is suppressed. Default: `30`. |
| `SHORT_HOLD_THRESHOLD_MS`      | No             | Hold below this (but above `MICRO_HOLD_THRESHOLD_MS`) arms the extended window. Default: `70`. **Most calibration-sensitive parameter** — fast typists may produce holds in this range legitimately; lower toward `MICRO_HOLD_THRESHOLD_MS` if you see false positives on fast same-key presses. |
| `EXTENDED_THRESHOLD_MS`        | No             | Debounce window after a *Short* hold, in ms. Default: `100`. |
| `MICRO_HOLD_THRESHOLD_MS`      | No             | Hold below this is classified as *Micro* (hardware ghost contact), arming the micro-extended window. Default: `20`. |
| `MICRO_EXTENDED_THRESHOLD_MS`  | No             | Debounce window after a *Micro* hold, in ms. Default: `150`. |
| `LOG_FORWARD`                  | No             | `true` / `false` — log every forwarded event immediately. Default: `false`. |
| `TRACK_DB`                     | No             | Path to an SQLite file for passive key-health recording. Created automatically. Disabled by default. |

See [`docs/debounce-algorithm.md`](docs/debounce-algorithm.md) for a detailed explanation
of how the tiered debounce mechanism works, including worked examples, the rationale
behind each default value, and a threshold tuning guide.

## How the health tracker works

When `TRACK_DB` is set, every key press, release, and auto‑repeat is written to
a local SQLite database with millisecond‑accurate timestamps, the key name
(e.g., `KEY_A`), the event value (`1` = down, `0` = up, `2` = auto‑repeat),
and a `suppressed` flag (`0` = forwarded, `1` = dropped by the debouncer).

## License

This app is licensed under GPLv3
