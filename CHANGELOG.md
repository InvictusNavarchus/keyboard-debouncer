# Changelog

User-visible changes are recorded here. Add upcoming changes under `Unreleased`;
see [the release guide](docs/releasing.md) for the release procedure.

## [Unreleased]

## [0.4.0] - 2026-11-04

### Added

- Read-only Bun health reports with overall suppression rates, daily and hourly trends, and the top affected keys, separated by keyboard identity.
- Gap and hold timing distributions with percentiles, forwarded/suppressed sample counts, and a gap histogram for debounce threshold tuning.
- Monotonic gap and hold measurements stored with sub-millisecond precision, using the same captured timings as debounce decisions.
- Automatic configuration history: events reference the effective thresholds and filtering scope, unchanged settings reuse their period, and subsequent tuning attempts get new periods.
- `--latest-config` to isolate the newest configuration period per keyboard, and `--details` to include per-key timing tables.
- A `bun run analyze` shortcut for reports against the service's private tracker database.

### Changed

- Health reports default to the current local day. Use `--days N` for a rolling N-day window.
- Tracker databases migrate automatically to add timing measurements and configuration references. Historical events retain unknown settings; legacy timing samples are reconstructed separately from stored monotonic samples.

### Upgrading

Rebuild and restart the daemon to begin recording monotonic timings and configuration history. Existing v0.3.0 configurations continue to work; tracking still requires `KEYBOARD_ID`. Bun is needed only to run the analytics helper, which requires no package installation.

## [0.3.0] - 2026-11-02

### Breaking Changes

- Tracking now requires `KEYBOARD_ID` in `debouncer.conf`. The daemon refuses to start if tracking is requested without an identity configured.

### Added

- Per-keyboard event isolation in SQLite tracker using compact identity references.
- Automatic database migration for pre-0.3.0 tracker databases preserving unattributed history.

### Changed

- Upgraded `evdev` from 0.12 to 0.13, reducing dependency footprint and pruning legacy bitflags/proc-macros.
- Scoped documentation prerequisites to Linux platforms.

[Unreleased]: https://github.com/InvictusNavarchus/keyboard-debouncer/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/InvictusNavarchus/keyboard-debouncer/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/InvictusNavarchus/keyboard-debouncer/compare/v0.2.0...v0.3.0
