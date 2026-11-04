use crate::config::DebounceConfig;
use evdev::KeyCode;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Durations measured once from the event's monotonic capture.
#[derive(Clone, Copy, Debug, Default)]
pub struct EventTiming {
    pub gap: Option<Duration>,
    pub hold: Option<Duration>,
}

#[derive(Debug)]
pub struct TrackEvent {
    pub ts_ms: u64,
    pub key: KeyCode,
    pub value: i32,
    pub suppressed: bool,
    pub timing: EventTiming,
}

pub struct Tracker {
    sender: Option<SyncSender<TrackEvent>>,
}

impl Tracker {
    pub fn new(
        db_path: Option<PathBuf>,
        identity: Option<&str>,
        config: &DebounceConfig,
        keys: &[KeyCode],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let Some(path) = db_path else {
            return Ok(Self { sender: None });
        };
        let identity = identity
            .filter(|id| !id.trim().is_empty())
            .ok_or("KEYBOARD_ID is required when TRACK_DB is enabled")?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(&path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let keyboard_id = initialize_database(&mut conn, identity)?;
        let period_id = resolve_period(&mut conn, keyboard_id, config, keys, SystemTime::now())?;

        // Resolve identity once; the worker writes only its integer ID with each event.
        let (tx, rx) = sync_channel::<TrackEvent>(10000);
        thread::spawn(move || {
            record_events(&mut conn, keyboard_id, Some(period_id), rx);
        });

        Ok(Self { sender: Some(tx) })
    }

    pub fn track(
        &self,
        key: KeyCode,
        value: i32,
        suppressed: bool,
        now: SystemTime,
        timing: EventTiming,
    ) {
        if let Some(sender) = &self.sender {
            let ts_ms = now
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);

            let event = TrackEvent {
                ts_ms,
                key,
                value,
                suppressed,
                timing,
            };

            match sender.try_send(event) {
                Ok(_) => {}
                Err(TrySendError::Full(_)) => {
                    eprintln!("Tracker: Buffer full, dropping event (is disk too slow?)");
                }
                Err(TrySendError::Disconnected(_)) => {
                    // Background thread died silently, disable tracking
                }
            }
        }
    }
}

/// Migrate without attributing historical events to the currently configured keyboard.
fn initialize_database(conn: &mut Connection, identity: &str) -> rusqlite::Result<i64> {
    conn.pragma_update(None, "foreign_keys", true)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS keyboards (
            id INTEGER PRIMARY KEY,
            identity TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS config_periods (
            id INTEGER PRIMARY KEY, keyboard_id INTEGER NOT NULL REFERENCES keyboards(id),
            started_at_ms INTEGER NOT NULL,
            threshold_ms INTEGER NOT NULL, short_hold_threshold_ms INTEGER NOT NULL,
            extended_threshold_ms INTEGER NOT NULL, micro_hold_threshold_ms INTEGER NOT NULL,
            micro_extended_threshold_ms INTEGER NOT NULL,
            debounce_all_keys INTEGER NOT NULL, target_keys TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_config_periods_keyboard ON config_periods(keyboard_id, id);
        CREATE TABLE IF NOT EXISTS key_events (
            ts_ms INTEGER NOT NULL,
            key TEXT NOT NULL,
            value INTEGER NOT NULL,
            suppressed INTEGER NOT NULL,
            keyboard_id INTEGER REFERENCES keyboards(id)
        );
        CREATE INDEX IF NOT EXISTS idx_key_events_ts ON key_events(ts_ms);",
    )?;
    let columns = {
        let mut stmt = tx.prepare("PRAGMA table_info(key_events)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        names.collect::<rusqlite::Result<Vec<_>>>()?
    };
    if !columns.iter().any(|name| name == "keyboard_id") {
        tx.execute_batch(
            "ALTER TABLE key_events ADD COLUMN keyboard_id INTEGER REFERENCES keyboards(id)",
        )?;
    }
    for (name, sql) in [
        ("config_period_id", "ALTER TABLE key_events ADD COLUMN config_period_id INTEGER REFERENCES config_periods(id)"),
        ("gap_ms", "ALTER TABLE key_events ADD COLUMN gap_ms REAL"),
        ("hold_ms", "ALTER TABLE key_events ADD COLUMN hold_ms REAL"),
        (
            "timing_version",
            "ALTER TABLE key_events ADD COLUMN timing_version INTEGER",
        ),
    ] {
        if !columns.iter().any(|column| column == name) {
            tx.execute_batch(sql)?;
        }
    }
    tx.execute(
        "INSERT INTO keyboards (identity) VALUES (?1) ON CONFLICT(identity) DO NOTHING",
        [identity],
    )?;
    let id = tx.query_row(
        "SELECT id FROM keyboards WHERE identity = ?1",
        [identity],
        |row| row.get(0),
    )?;
    tx.commit()?;
    Ok(id)
}

// Compare only the latest period for this keyboard: A -> B -> A is three trials.
fn resolve_period(
    conn: &mut Connection,
    keyboard_id: i64,
    config: &DebounceConfig,
    keys: &[KeyCode],
    now: SystemTime,
) -> Result<i64, Box<dyn std::error::Error>> {
    let mut names: Vec<String> = if config.debounce_all {
        Vec::new()
    } else {
        keys.iter().map(|key| format!("\"{key:?}\"")).collect()
    };
    names.sort();
    names.dedup();
    let targets = format!("[{}]", names.join(","));
    let threshold_ms = i64::try_from(config.threshold_ms)?;
    let short_hold_threshold_ms = i64::try_from(config.short_hold_threshold_ms)?;
    let extended_threshold_ms = i64::try_from(config.extended_threshold_ms)?;
    let micro_hold_threshold_ms = i64::try_from(config.micro_hold_threshold_ms)?;
    let micro_extended_threshold_ms = i64::try_from(config.micro_extended_threshold_ms)?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let settings = params![
        keyboard_id,
        threshold_ms,
        short_hold_threshold_ms,
        extended_threshold_ms,
        micro_hold_threshold_ms,
        micro_extended_threshold_ms,
        config.debounce_all,
        targets
    ];
    let existing = tx
        .query_row(
            "SELECT id FROM config_periods
        WHERE id = (SELECT id FROM config_periods WHERE keyboard_id = ?1 ORDER BY id DESC LIMIT 1)
        AND threshold_ms = ?2 AND short_hold_threshold_ms = ?3 AND extended_threshold_ms = ?4
        AND micro_hold_threshold_ms = ?5 AND micro_extended_threshold_ms = ?6
        AND debounce_all_keys = ?7 AND target_keys = ?8",
            settings,
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let id = if let Some(id) = existing {
        id
    } else {
        let started = i64::try_from(now.duration_since(UNIX_EPOCH)?.as_millis())?;
        tx.execute("INSERT INTO config_periods (keyboard_id, threshold_ms, short_hold_threshold_ms,
            extended_threshold_ms, micro_hold_threshold_ms, micro_extended_threshold_ms,
            debounce_all_keys, target_keys, started_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![keyboard_id, threshold_ms, short_hold_threshold_ms,
                extended_threshold_ms, micro_hold_threshold_ms,
                micro_extended_threshold_ms, config.debounce_all, targets, started])?;
        tx.last_insert_rowid()
    };
    tx.commit()?;
    Ok(id)
}

fn record_events(
    conn: &mut Connection,
    keyboard_id: i64,
    period_id: Option<i64>,
    rx: std::sync::mpsc::Receiver<TrackEvent>,
) {
    // Read from channel and insert in batches
    let mut buffer = Vec::with_capacity(100);

    while let Ok(event) = rx.recv() {
        buffer.push(event);

        // Drain remaining events up to a reasonable batch size
        while buffer.len() < 1000 {
            match rx.try_recv() {
                Ok(ev) => buffer.push(ev),
                Err(_) => break,
            }
        }

        let tx_conn = match conn.transaction() {
            Ok(tx) => tx,
            Err(e) => {
                eprintln!("Tracker: Failed to start transaction: {}", e);
                buffer.clear();
                continue;
            }
        };

        let mut success = true;
        {
            if let Ok(mut stmt) = tx_conn.prepare_cached(
                "INSERT INTO key_events (ts_ms, key, value, suppressed, keyboard_id, gap_ms, hold_ms, timing_version, config_period_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
            ) {
                for ev in &buffer {
                    if let Err(e) = stmt.execute(params![
                        ev.ts_ms as i64,
                        format!("{:?}", ev.key),
                        ev.value,
                        if ev.suppressed { 1 } else { 0 },
                        keyboard_id,
                        ev.timing.gap.map(|d| d.as_secs_f64() * 1000.0),
                        ev.timing.hold.map(|d| d.as_secs_f64() * 1000.0),
                        period_id
                    ]) {
                        eprintln!("Tracker: Failed to insert event: {}", e);
                        success = false;
                        break;
                    }
                }
            } else {
                eprintln!("Tracker: Failed to prepare statement");
                success = false;
            }
        }

        if success {
            if let Err(e) = tx_conn.commit() {
                eprintln!("Tracker: Failed to commit transaction: {}", e);
            }
        }

        buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DebounceConfig {
        DebounceConfig {
            threshold_ms: 30,
            short_hold_threshold_ms: 70,
            extended_threshold_ms: 100,
            micro_hold_threshold_ms: 20,
            micro_extended_threshold_ms: 150,
            debounce_all: false,
            log_forward: false,
        }
    }

    fn record(conn: &mut Connection, id: i64, suppressed: bool) {
        let (tx, rx) = sync_channel(10);
        tx.send(TrackEvent {
            ts_ms: 123,
            key: KeyCode::KEY_A,
            value: 1,
            suppressed,
            timing: EventTiming::default(),
        })
        .unwrap();
        drop(tx);
        record_events(conn, id, None, rx);
    }

    #[test]
    fn identities_separate_events_and_survive_reinitialization() {
        let mut conn = Connection::open_in_memory().unwrap();
        let first = initialize_database(&mut conn, "desk-01").unwrap();
        record(&mut conn, first, false);
        let second = initialize_database(&mut conn, "desk-02").unwrap();
        assert_ne!(first, second);
        record(&mut conn, second, true);
        assert_eq!(initialize_database(&mut conn, "desk-01").unwrap(), first);
        record(&mut conn, first, false);
        let mut stmt = conn
            .prepare(
                "SELECT k.identity, e.ts_ms, e.key, e.value, e.suppressed, typeof(e.keyboard_id)
             FROM key_events e JOIN keyboards k ON k.id = e.keyboard_id ORDER BY e.rowid",
            )
            .unwrap();
        let rows: Vec<_> = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i32>(3)?,
                    r.get::<_, i32>(4)?,
                    r.get::<_, String>(5)?,
                ))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (
                    "desk-01".into(),
                    123,
                    "KEY_A".into(),
                    1,
                    0,
                    "integer".into()
                ),
                (
                    "desk-02".into(),
                    123,
                    "KEY_A".into(),
                    1,
                    1,
                    "integer".into()
                ),
                (
                    "desk-01".into(),
                    123,
                    "KEY_A".into(),
                    1,
                    0,
                    "integer".into()
                ),
            ]
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM keyboards", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }

    #[test]
    fn migration_preserves_unattributed_history_and_is_repeatable() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE key_events (ts_ms INTEGER NOT NULL, key TEXT NOT NULL, value INTEGER NOT NULL, suppressed INTEGER NOT NULL);
            INSERT INTO key_events VALUES (42, 'KEY_B', 0, 1);").unwrap();
        let id = initialize_database(&mut conn, "new-keyboard").unwrap();
        assert_eq!(initialize_database(&mut conn, "new-keyboard").unwrap(), id);
        record(&mut conn, id, false);
        let old = conn.query_row("SELECT ts_ms, key, value, suppressed, keyboard_id FROM key_events WHERE ts_ms = 42", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i32>(2)?, r.get::<_, i32>(3)?, r.get::<_, Option<i64>>(4)?))).unwrap();
        assert_eq!(old, (42, "KEY_B".into(), 0, 1, None));
        assert_eq!(
            conn.query_row(
                "SELECT config_period_id FROM key_events WHERE ts_ms = 42",
                [],
                |r| r.get::<_, Option<i64>>(0)
            )
            .unwrap(),
            None
        );
        assert!(conn
            .execute("INSERT INTO key_events (ts_ms, key, value, suppressed, keyboard_id) VALUES (0, 'KEY_A', 1, 0, 999)", [])
            .is_err());
    }

    #[test]
    fn identity_and_recordings_survive_database_reopen() {
        let path = std::env::temp_dir().join(format!(
            "keyboard-debouncer-{}-{}.db",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let id = {
            let mut conn = Connection::open(&path).unwrap();
            let id = initialize_database(&mut conn, "desk-01").unwrap();
            record(&mut conn, id, false);
            id
        };
        {
            let mut conn = Connection::open(&path).unwrap();
            assert_eq!(initialize_database(&mut conn, "desk-01").unwrap(), id);
            record(&mut conn, id, true);
            assert_eq!(
                conn.query_row(
                    "SELECT count(*) FROM key_events WHERE keyboard_id = ?1",
                    [id],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
                2
            );
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn periods_follow_effective_changes_per_keyboard_and_events_keep_their_period() {
        let mut conn = Connection::open_in_memory().unwrap();
        let keyboard = initialize_database(&mut conn, "desk").unwrap();
        let other = initialize_database(&mut conn, "travel").unwrap();
        let mut cfg = config();
        let now = UNIX_EPOCH + Duration::from_millis(1000);
        let keys = [KeyCode::KEY_N, KeyCode::KEY_A];
        let first = resolve_period(&mut conn, keyboard, &cfg, &keys, now).unwrap();
        cfg.log_forward = true;
        assert_eq!(
            resolve_period(
                &mut conn,
                keyboard,
                &cfg,
                &[KeyCode::KEY_A, KeyCode::KEY_N, KeyCode::KEY_A],
                now + Duration::from_secs(1)
            )
            .unwrap(),
            first
        );
        let travel = resolve_period(&mut conn, other, &cfg, &keys, now).unwrap();
        assert_ne!(travel, first);
        cfg.micro_hold_threshold_ms = 30;
        let second = resolve_period(&mut conn, keyboard, &cfg, &keys, now).unwrap();
        assert_ne!(first, second);
        cfg.micro_hold_threshold_ms = 20;
        let third = resolve_period(&mut conn, keyboard, &cfg, &keys, now).unwrap();
        assert_ne!(third, first);
        assert_ne!(third, second);
        let started: i64 = conn
            .query_row(
                "SELECT started_at_ms FROM config_periods WHERE id = ?",
                [first],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(started, 1000);
        cfg.debounce_all = true;
        let all = resolve_period(&mut conn, keyboard, &cfg, &keys, now).unwrap();
        assert_eq!(
            resolve_period(&mut conn, keyboard, &cfg, &[], now).unwrap(),
            all
        );
        // Cached period IDs, rather than timestamps or the latest global period,
        // attribute events even when settings change or the wall clock moves.
        for period in [first, second, third] {
            let (tx, rx) = sync_channel(1);
            tx.send(TrackEvent {
                ts_ms: 123,
                key: KeyCode::KEY_N,
                value: 1,
                suppressed: false,
                timing: EventTiming::default(),
            })
            .unwrap();
            drop(tx);
            record_events(&mut conn, keyboard, Some(period), rx);
        }
        let linked: Vec<i64> = conn
            .prepare("SELECT config_period_id FROM key_events ORDER BY rowid")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(linked, vec![first, second, third]);
        assert_eq!(initialize_database(&mut conn, "desk").unwrap(), keyboard);
    }

    #[test]
    fn every_filter_setting_can_start_a_new_period() {
        let mut conn = Connection::open_in_memory().unwrap();
        let id = initialize_database(&mut conn, "desk").unwrap();
        let mut cfg = config();
        let now = UNIX_EPOCH;
        let mut previous = resolve_period(&mut conn, id, &cfg, &[], now).unwrap();
        for index in 0..5 {
            match index {
                0 => cfg.threshold_ms += 1,
                1 => cfg.short_hold_threshold_ms += 1,
                2 => cfg.extended_threshold_ms += 1,
                3 => cfg.micro_hold_threshold_ms += 1,
                _ => cfg.micro_extended_threshold_ms += 1,
            }
            let next = resolve_period(&mut conn, id, &cfg, &[], now).unwrap();
            assert_ne!(previous, next);
            previous = next;
        }
        assert_ne!(
            previous,
            resolve_period(&mut conn, id, &cfg, &[KeyCode::KEY_A], now).unwrap()
        );
        assert!(resolve_period(
            &mut conn,
            id,
            &DebounceConfig {
                threshold_ms: u64::MAX,
                ..cfg
            },
            &[],
            now
        )
        .is_err());
    }

    #[test]
    fn disabled_tracking_needs_no_identity() {
        assert!(Tracker::new(None, None, &config(), &[])
            .unwrap()
            .sender
            .is_none());
    }

    #[test]
    fn captures_and_submillisecond_durations_survive_recording_and_migration() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE keyboards (id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE);
            INSERT INTO keyboards VALUES (1, 'desk');
            CREATE TABLE key_events (ts_ms INTEGER NOT NULL, key TEXT NOT NULL, value INTEGER NOT NULL, suppressed INTEGER NOT NULL, keyboard_id INTEGER REFERENCES keyboards(id));
            INSERT INTO key_events VALUES (1, 'KEY_A', 1, 0, 1);").unwrap();
        let id = initialize_database(&mut conn, "desk").unwrap();
        assert_eq!(initialize_database(&mut conn, "desk").unwrap(), id);
        let old: (Option<f64>, Option<f64>, Option<i64>) = conn
            .query_row(
                "SELECT gap_ms, hold_ms, timing_version FROM key_events WHERE ts_ms = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(old, (None, None, None));

        let (tx, rx) = sync_channel(10);
        let tracker = Tracker { sender: Some(tx) };
        tracker.track(
            KeyCode::KEY_A,
            1,
            true,
            UNIX_EPOCH + Duration::from_millis(1234),
            EventTiming {
                gap: Some(Duration::from_micros(12_345)),
                hold: None,
            },
        );
        tracker.track(
            KeyCode::KEY_A,
            0,
            true,
            UNIX_EPOCH + Duration::from_millis(1233),
            EventTiming {
                gap: None,
                hold: Some(Duration::from_micros(678)),
            },
        );
        tracker.track(
            KeyCode::KEY_A,
            2,
            false,
            UNIX_EPOCH + Duration::from_millis(1235),
            EventTiming::default(),
        );
        drop(tracker);
        record_events(&mut conn, id, None, rx);
        let rows: Vec<(i64, Option<f64>, Option<f64>, i64)> = conn.prepare(
            "SELECT ts_ms, gap_ms, hold_ms, timing_version FROM key_events WHERE timing_version = 1 ORDER BY rowid"
        ).unwrap().query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].0, 1234);
        assert!((rows[0].1.unwrap() - 12.345).abs() < 1e-9);
        assert_eq!(rows[0].2, None);
        assert_eq!(rows[1].0, 1233);
        assert_eq!(rows[1].1, None);
        assert!((rows[1].2.unwrap() - 0.678).abs() < 1e-9);
        assert_eq!(rows[2], (1235, None, None, 1));
    }

    #[test]
    fn tracking_rejects_missing_or_blank_identity_before_opening_database() {
        for identity in [None, Some(""), Some("   ")] {
            let err = Tracker::new(
                Some(PathBuf::from("/nonexistent/keys.db")),
                identity,
                &config(),
                &[],
            )
            .err()
            .unwrap();
            assert!(err.to_string().contains("KEYBOARD_ID"));
        }
    }
}
