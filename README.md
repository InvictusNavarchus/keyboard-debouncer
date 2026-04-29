A CLI daemon for Linux that prevents keyboard chatter by intercepting events at the OS level via evdev/uinput.

## How to use

1. Find your keyboard using `evtest` — note the device name and the `KEY_*` names of chattering keys
2. Build the binary: `cargo build --release`
3. Copy the example config: `cp debouncer.conf.example debouncer.conf`
4. Edit `debouncer.conf` — set at minimum `KEYBOARD_NAME` (or `DEVICE_PATH`) and `KEYS`
5. Launch: `sudo ./target/release/keyboard-debouncer`

## Config (`debouncer.conf`)

| Field | Required | Description |
|---|---|---|
| `KEYBOARD_NAME` | One of these two | Keyboard name as shown by `evtest` — used to auto-discover the event node |
| `DEVICE_PATH` | One of these two | Direct path, e.g. `/dev/input/event10` — overrides `KEYBOARD_NAME` if both are set |
| `KEYS` | **Yes** | Comma-separated keys to debounce, using `KEY_*` names from `evtest` (e.g. `KEY_K,KEY_L`) |
| `THRESHOLD_MS` | No | Debounce window in ms — any re-press within this window is suppressed (default: `30`) |
| `LOG_FORWARD` | No | `true`/`false` — log forwarded events immediately; default `false` (shown only on context) |

See `debouncer.conf.example` for a fully annotated template.

## License

This app is licensed under GPLv3
