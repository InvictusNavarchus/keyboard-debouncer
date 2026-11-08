# Configuration

For the recommended setup, edit `/etc/debouncer.conf`. A minimal configuration is:

```ini
KEYBOARD_NAME=your-keyboard-name-in-evtest
KEYS=KEY_K,KEY_L
```

Replace the keyboard name and key list with your values. Other fields are optional;
omitted fields use the defaults below. Lines starting with `#` are comments.
The [example configuration](../debouncer.conf.example) includes all settings.
Restart the service after editing:

```bash
sudo systemctl restart keyboard-debouncer
```

## Fields

| Field                         | Required?     | Description                                                                                                                                                                                                                                                                                      |
| ----------------------------- | ------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `KEYBOARD_NAME`               | 1 of these 2  | Keyboard name as shown by `evtest` — used to auto-discover the event node.                                                                                                                                                                                                                       |
| `DEVICE_PATH`                 | 1 of these 2  | Direct path, e.g. `/dev/input/event10`. For direct device selection, leave `KEYBOARD_NAME` unset; see device selection below.                                                                                                                                                                    |
| `KEYS`                        | **Yes**       | Comma-separated keys to debounce, using `KEY_*` names from `evtest` (example: `KEY_K,KEY_L,KEY_ENTER`). Still required and validated when `DEBOUNCE_ALL_KEYS=true`, although the list does not limit filtering in that mode.                                                                     |
| `DEBOUNCE_ALL_KEYS`           | No            | `true` / `false` — debounce all keyboard keys automatically. Modifiers always excluded. Default: `false`.                                                                                                                                                                                        |
| `THRESHOLD_MS`                | No            | Base debounce window for release-to-press (`gap` in logs), in ms. A key re-press with `gap` below this is suppressed. Default: `30`.                                                                                                                                                             |
| `SHORT_HOLD_THRESHOLD_MS`     | No            | Key press-to-release duration (`hold` in logs) below this (but $\ge$ `MICRO_HOLD_THRESHOLD_MS`) classifies the hold as _Short_ and arms `EXTENDED_THRESHOLD_MS` for the next press. Default: `70`. **Most calibration-sensitive parameter** — fast typists may produce holds in this range legitimately; lower toward `MICRO_HOLD_THRESHOLD_MS` if you see false positives on fast same-key presses. |
| `EXTENDED_THRESHOLD_MS`       | No            | Debounce window for release-to-press (`gap` in logs) after a _Short_ hold, in ms. Default: `100`.                                                                                                                                                                                                |
| `MICRO_HOLD_THRESHOLD_MS`     | No            | Key press-to-release duration (`hold` in logs) below this is classified as _Micro_ (hardware ghost contact), arming `MICRO_EXTENDED_THRESHOLD_MS` for the next press. Default: `20`.                                                                                                          |
| `MICRO_EXTENDED_THRESHOLD_MS` | No            | Debounce window for release-to-press (`gap` in logs) after a _Micro_ hold, in ms. Default: `150`.                                                                                                                                                                                                |
| `LOG_FORWARD`                 | No            | `true` / `false` — log every forwarded event immediately. Default: `false`.                                                                                                                                                                                                                      |
| `KEYBOARD_ID`                 | When tracking | User-assigned unique label for this physical keyboard. Required when `TRACK_DB` is set.                                                                                                                                                                                                          |
| `TRACK_DB`                    | No            | Path to an SQLite file for passive key-health recording. Created automatically. Disabled by default.                                                                                                                                                                                             |

See [the debounce algorithm guide](debounce-algorithm.md) for a detailed explanation
of how the tiered debounce mechanism works, including worked examples, the rationale
behind each default value, and a threshold tuning guide.

### Understanding `gap` and `hold` in logs

When monitoring debounce events via `keyboard-debouncer watch` or `journalctl -u keyboard-debouncer`, logs display two distinct timing measurements:

- **`gap=...ms` (Key Down / Press)**: Measured duration between releasing a key (**UP**) and pressing it down again (**DN**). This value is evaluated against the active debounce window:
  - `THRESHOLD_MS` (Normal tier)
  - `EXTENDED_THRESHOLD_MS` (Short tier)
  - `MICRO_EXTENDED_THRESHOLD_MS` (Micro tier)
  If `gap` is less than the active window, the press is suppressed as chatter.

- **`hold=...ms` (Key Up / Release)**: Measured duration the key was held down (**DN** to **UP**). This value is evaluated on key release against:
  - `MICRO_HOLD_THRESHOLD_MS` (hold < 20 ms $\rightarrow$ Micro tier)
  - `SHORT_HOLD_THRESHOLD_MS` (hold < 70 ms $\rightarrow$ Short tier)
  This classification determines which debounce window (`THRESHOLD_MS`, `EXTENDED_THRESHOLD_MS`, or `MICRO_EXTENDED_THRESHOLD_MS`) is armed for the *next* press of that key.

## Device selection

Prefer `KEYBOARD_NAME`: it matches the physical device's name exactly after
trimming whitespace and rediscovers its event node on reconnect. It also waits
for the keyboard if it is disconnected at startup.

To select a device directly, set `DEVICE_PATH` and remove or comment out
`KEYBOARD_NAME`. The path must exist at startup. Event numbers such as `event10`
can change across boots or reconnects.

Avoid setting both fields. Currently the config parser validates `DEVICE_PATH`,
but the running daemon still performs name-based discovery whenever
`KEYBOARD_NAME` is present.

## Choosing keys

Start with only the keys that chatter. With `DEBOUNCE_ALL_KEYS=true`, the daemon
filters all eligible keys and excludes modifier/control keys such as Shift, Ctrl,
Alt, Meta, CapsLock, NumLock, ScrollLock, and Fn. A valid, nonempty `KEYS` list is
still required by the config parser in this mode.

## Tuning thresholds

Start with the defaults, then check both chatter suppression and deliberate
same-key repetition. Fast typing can produce short holds that arm the extended
window and suppress an intentional second press. If this happens, lower
`SHORT_HOLD_THRESHOLD_MS` toward `MICRO_HOLD_THRESHOLD_MS` and restart the service.
This reduces protection against chatter after medium-length holds.

Use the [threshold tuning guide](debounce-algorithm.md#threshold-decision-guide)
and logs to decide which threshold to adjust. The defaults are a starting point
for your keyboard and typing speed, not a guarantee that every suppressed press
is chatter.

## Optional tracking

See [health tracking](health-tracking.md) for database setup, keyboard identities,
reports, and upgrade considerations. Tracking is disabled unless `TRACK_DB` is set.

[Back to the README](../README.md)
