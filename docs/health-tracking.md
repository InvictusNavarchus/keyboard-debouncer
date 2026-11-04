# Health tracking and reports

Health tracking is optional. It records every key event, including keys outside
the debounce targets, to a local SQLite database so you can inspect suppression
patterns and tune the filter. Basic filtering does not require tracking or Bun.

## Enable tracking

Add these fields to `/etc/debouncer.conf`:

```ini
TRACK_DB=/var/lib/keyboard-debouncer/keys.db
KEYBOARD_ID=keychron-k8-desk-01
```

Choose a unique `KEYBOARD_ID` for your physical keyboard. Keep it across restarts,
reconnects, and USB port changes. Use a new identity for a replacement keyboard,
even an identical model; restore the old identity if you switch back. The label
identifies history, while `KEYBOARD_NAME` or `DEVICE_PATH` selects the device.
The daemon trusts the label and cannot detect a hardware swap automatically.

Restart the service to start recording:

```bash
sudo systemctl restart keyboard-debouncer
```

The systemd unit creates `/var/lib/keyboard-debouncer/`, owned by `kbd-debouncer`
with mode `0700` and `UMask=0077`. This keeps the sensitive key-event history
private to the service user and root. Use that location with the service;
foreground runs with another database path need suitable directory permissions.

## Run a health report

Run the following commands from the repository root.

With [Bun](https://bun.sh) installed, run the read-only analytics helper. No package
installation is needed to run it:

```bash
bun scripts/analyze.ts --db /path/to/keys.db
bun scripts/analyze.ts --db /path/to/keys.db --days 14
bun scripts/analyze.ts --help
```

The database path defaults to `/var/lib/keyboard-debouncer/keys.db`. For the
service's private database, use the shortcut from the repository root:

```bash
bun run analyze
bun run analyze --days 14
bun run analyze --latest-config
bun run analyze --details
bun run analyze --latest-config --details
```

The shortcut runs the report through `sudo`, using your Bun executable's full path
so Bun does not need to be on sudo's PATH. It may prompt for your password. All
arguments are forwarded; use `--db PATH` to override the default database.

Use `--latest-config` to report only the newest configuration period for each
keyboard, intersected with the report window. Empty latest periods show zero
presses instead of falling back to older results. The report displays the period
ID, load time, and settings. Without this flag, the report combines periods in the
window, lists their settings, and counts presses with unknown configurations.
Legacy databases remain readable; latest-period mode requires recorded periods.

The default window is **today, from local midnight until the report starts**.
Use `--days N` for the past **N × 24 hours** instead; `N` must be a positive whole
number. Each keyboard gets its own report:

- Overall presses, suppressed presses, and suppression rate.
- Daily totals in local time, including dates with no recorded presses. The first
  and last dates may cover only part of a day.
- All 24 local hours, combining the same hour across dates to reveal patterns.
- Top 10 affected keys ranked by suppressed count, with total presses and per-key
  rates so low-volume keys have context.
- Gap and hold sample counts, minimum, p5, median, p95, and maximum in milliseconds,
  separately for forwarded and suppressed presses, overall. Add `--details` to
  include timing tables for each top affected key; the compact top-keys ranking
  is always displayed when suppressions exist.
  `Mono` counts stored monotonic samples; `Approx` counts reconstructed historical
  samples. `Samples` is their sum; distributions and histograms combine both.
- Overall gap histogram with buckets `<20`, `20–<30`, `30–<70`, `70–<100`,
  `100–<150`, and `≥150` ms. Counts separate forwarded and suppressed presses.

**Gap** means the same key's physical release → next press, including suppressed
releases; it is not the interval between two presses. Gap samples are classified
by the next press's decision. **Hold** means a press → its matching release,
classified by that press's decision. Auto-repeats do not affect either measurement.
Percentiles use linear interpolation; empty samples display `—`. Zero-ms durations
are retained because events can share a stored millisecond. Long idle gaps remain
included, so use the histogram alongside percentiles when tuning debounce windows.

New recordings use the daemon's stored monotonic durations directly, including
when a predecessor falls outside the reporting window or its recording was lost.
The event ending the gap or hold must be inside the window, selected by `ts_ms`.
A null duration on a version `1` row is skipped; it is never reconstructed across
a restart. Wall-clock adjustments affect calendar placement, not these durations.

Historical timings are reconstructed from integer wall-clock timestamps, in
recording order, using only events inside the selected window. Pairs are kept
separate by keyboard and key. Missing preceding releases, incomplete holds,
inconsistent press/release decisions, and negative durations are skipped and
counted. Historical first presses usually have no gap sample. Events outside the
window are not used to complete historical pairs. Restarts, lost recordings, and
wall-clock changes that leave plausible positive durations cannot be identified
reliably in historical data.

Use the gap distributions to assess debounce windows, and hold distributions to
assess micro/short hold cutoffs. Forwarded does not guarantee intentional typing,
and suppressed does not guarantee hardware chatter. For historical events without a
configuration reference, threshold changes are unknown. These statistics describe
observed events; they do not provide an automatic ideal threshold.

**Suppression rate = suppressed key-downs / all recorded key-downs.** Releases and
auto-repeats are excluded. This measures the daemon's decisions, including possible
false positives, rather than confirmed hardware faults. Keys outside the configured
debounce targets also contribute to the denominator. Comparisons across configuration
changes therefore need care. A missing recording cannot distinguish idle time from
a stopped tracker. A zero denominator displays `—`.

Historical records without a keyboard identity are reported separately. Databases
from before identity tracking are supported without migration. Queries use a
consistent read-only snapshot and work while the daemon records in WAL mode.
Time grouping uses the machine's local timezone; set `TZ=UTC` before running Bun
if you prefer UTC.

## Recorded events and timing

When `TRACK_DB` is set, every key press, release, and auto‑repeat is written to
a local SQLite database with millisecond‑accurate timestamps, the key name
(e.g., `KEY_A`), the event value (`1` = down, `0` = up, `2` = auto‑repeat),
and a `suppressed` flag (`0` = forwarded, `1` = dropped by the debouncer).

For each key event, the daemon captures the monotonic clock and wall clock once,
before filtering and logging. Gap/hold calculations, threshold decisions, and state
updates share that monotonic capture; logs and `ts_ms` share the wall-clock capture.
The tracker stores the calculated durations as nullable `gap_ms` and `hold_ms`
SQLite `REAL` values, preserving sub-millisecond precision. A press stores the gap
since the same key's last physical release; a release stores the hold since its
last press. This includes suppressed cycles and keys outside the debounce targets.
Auto-repeats have neither duration. The first press/release without a known prior
release/press has a null duration. These are daemon processing timings, not kernel
event timestamps.

The `keyboards` table stores each text identity once, with an integer primary key.
Each event stores only its integer `keyboard_id` reference. Identity resolution
happens once at startup, outside the event recording path; reconnects reuse it.
No additional event index is created.

For example, inspect recent events with their identities (including old records):

```sql
SELECT e.ts_ms, k.identity, e.key, e.value, e.suppressed
FROM key_events AS e
LEFT JOIN keyboards AS k ON k.id = e.keyboard_id
ORDER BY e.ts_ms DESC
LIMIT 20;
```

## Upgrading existing databases

**Before upgrading a tracking-enabled installation, ensure `KEYBOARD_ID` is set
in its config.** Missing or blank identities prevent startup. Database
initialization errors also prevent startup rather than silently continuing
without the requested recording.

Existing databases migrate automatically on startup. Historical events keep a
`NULL` keyboard reference because their source cannot be inferred.

Startup also adds the timing columns plus `timing_version` to existing databases.
Historical rows retain null timing fields and a null version; new rows use version
`1`, including rows whose duration is unavailable. This distinguishes old data
from intentionally missing measurements after a restart. No historical durations
are backfilled. Upgrade and restart the daemon to begin recording them.

## Configuration history

When tracking is enabled, startup records the effective debounce settings in
`config_periods`. Each period contains its keyboard reference, load timestamp,
five thresholds, debounce-all mode, and a sorted JSON array of target keys.
Settings resolve once at startup; each event stores only `config_period_id`.
No event index is added for that reference.

The daemon compares settings with the latest period for that keyboard. Identical
settings reuse it, including across restarts; changing any filtering setting starts
a new period. Returning to an older setting creates another period, so `20 → 30 →
20` remains three distinct tuning attempts. Key ordering, duplicate keys, and log
preferences do not start periods. Target keys are recorded as `[]` when debounce-all
mode makes that list irrelevant. Period IDs are generated automatically by SQLite.

Edit the configuration and restart to apply changes. `started_at_ms` is when the
settings were loaded, not when the file was edited. Migration leaves historical
event references null; their settings cannot be inferred. Periods are immutable.
This assumes one daemon writer per physical keyboard, as in the service setup.

## Inspecting the database directly

Use `sudo` to access the service's private database:

```bash
sudo sqlite3 /var/lib/keyboard-debouncer/keys.db "SELECT * FROM key_events ORDER BY ts_ms DESC LIMIT 10;"
```

## Developing the analytics helper

From the repository root, install the development dependencies and run the checks:

```bash
cd scripts
bun install --frozen-lockfile
bun run typecheck
bun run test
```

[Back to the README](../README.md)
