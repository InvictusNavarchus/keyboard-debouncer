# keyboard-debouncer

A CLI daemon for Linux that prevents keyboard chatter by intercepting events at the
OS level via `evdev` and `uinput`. It grabs your physical keyboard exclusively, filters
out high‑speed bounce, and re‑injects clean key events through a virtual device.

## Features

- **Normal debounce** – suppresses a re‑press that arrives within `THRESHOLD_MS` after
  the last physical release.
- **Extended debounce** – if a legitimate press was abnormally short (held for less than
  `SHORT_HOLD_THRESHOLD_MS`), the next press must survive a longer
  `EXTENDED_THRESHOLD_MS` window. This catches a second bounce mode where the switch
  briefly loses contact then re‑engages tens of milliseconds later.
- **Key health tracking** (optional) – passively records *every* key event (even
  non‑target keys) to an SQLite database. You can later query the data to identify
  switches that are starting to fail, **before** the chatter becomes noticeable.
- **Zero‑configuration discovery** – set your keyboard name once and the app auto finds the correct `/dev/input/eventX`

## How to use

1. **Find your keyboard** using `evtest` (or `sudo libinput list-devices`).
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

## Configuration (`debouncer.conf`)

| Field                    | Required?      | Description |
|--------------------------|----------------|-------------|
| `KEYBOARD_NAME`          | 1 of these 2   | Keyboard name as shown by `evtest` – used to auto‑discover the event node. |
| `DEVICE_PATH`            | 1 of these 2   | Direct path, e.g. `/dev/input/event10`. Overrides `KEYBOARD_NAME` if both are set. Useful when the event number is stable. |
| `KEYS`                   | **Yes**        | Comma‑separated keys to debounce, using `KEY_*` names from `evtest` (example: `KEY_K,KEY_L,KEY_ENTER`). |
| `THRESHOLD_MS`           | No             | Normal debounce window in ms. Any re‑press within this window of the last release is suppressed. Default: `30`. |
| `EXTENDED_THRESHOLD_MS`  | No             | Extended window in ms, used when the previous press was abnormally short. Default: `100`. |
| `SHORT_HOLD_THRESHOLD_MS`| No             | Hold duration in ms; if a legitimate press is held for less than this, the next press is subject to the extended window. Default: `50`. |
| `LOG_FORWARD`            | No             | `true` / `false` – log every forwarded event immediately instead of only on context (when a suppress follows). Default: `false`. |
| `TRACK_DB`               | No             | Path to an SQLite file, e.g. `/var/lib/keyboard-debouncer/keys.db`. When set, the daemon passively records every key event (including non‑target keys) for health analysis. The database is created automatically. Make sure the parent directory exists and restrict permissions (e.g. `chmod 600`). Disabled by default. |

Refer to `debouncer.conf.example` for a fully commented template including all fields above.

## How the health tracker works

When `TRACK_DB` is set, every key press, release, and auto‑repeat is written to
a local SQLite database with millisecond‑accurate timestamps, the key name
(e.g., `KEY_A`), the event value (`1` = down, `0` = up, `2` = auto‑repeat),
and a `suppressed` flag (`0` = forwarded, `1` = dropped by the debouncer).

## License

This app is licensed under GPLv3
