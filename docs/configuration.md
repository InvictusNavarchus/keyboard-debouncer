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
| `THRESHOLD_MS`                | No            | Base debounce window in ms. Any re-press within this window of the last release is suppressed. Default: `30`.                                                                                                                                                                                    |
| `SHORT_HOLD_THRESHOLD_MS`     | No            | Hold below this (but above `MICRO_HOLD_THRESHOLD_MS`) arms the extended window. Default: `70`. **Most calibration-sensitive parameter** — fast typists may produce holds in this range legitimately; lower toward `MICRO_HOLD_THRESHOLD_MS` if you see false positives on fast same-key presses. |
| `EXTENDED_THRESHOLD_MS`       | No            | Debounce window after a _Short_ hold, in ms. Default: `100`.                                                                                                                                                                                                                                     |
| `MICRO_HOLD_THRESHOLD_MS`     | No            | Hold below this is classified as _Micro_ (hardware ghost contact), arming the micro-extended window. Default: `20`.                                                                                                                                                                              |
| `MICRO_EXTENDED_THRESHOLD_MS` | No            | Debounce window after a _Micro_ hold, in ms. Default: `150`.                                                                                                                                                                                                                                     |
| `LOG_FORWARD`                 | No            | `true` / `false` — log every forwarded event immediately. Default: `false`.                                                                                                                                                                                                                      |
| `KEYBOARD_ID`                 | When tracking | User-assigned unique label for this physical keyboard. Required when `TRACK_DB` is set.                                                                                                                                                                                                          |
| `TRACK_DB`                    | No            | Path to an SQLite file for passive key-health recording. Created automatically. Disabled by default.                                                                                                                                                                                             |

See [the debounce algorithm guide](debounce-algorithm.md) for a detailed explanation
of how the tiered debounce mechanism works, including worked examples, the rationale
behind each default value, and a threshold tuning guide.

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
