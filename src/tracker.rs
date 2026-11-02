use evdev::KeyCode;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct TrackEvent {
    pub ts_ms: u64,
    pub key: KeyCode,
    pub value: i32,
    pub suppressed: bool,
}

pub struct Tracker {
    sender: Option<SyncSender<TrackEvent>>,
}

impl Tracker {
    pub fn new(
        db_path: Option<PathBuf>,
        identity: Option<&str>,
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

        // Resolve identity once; the worker writes only its integer ID with each event.
        let (tx, rx) = sync_channel::<TrackEvent>(10000);
        thread::spawn(move || {
            record_events(&mut conn, keyboard_id, rx);
        });

        Ok(Self { sender: Some(tx) })
    }

    pub fn track(&self, key: KeyCode, value: i32, suppressed: bool, now: SystemTime) {
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
        CREATE TABLE IF NOT EXISTS key_events (
            ts_ms INTEGER NOT NULL,
            key TEXT NOT NULL,
            value INTEGER NOT NULL,
            suppressed INTEGER NOT NULL,
            keyboard_id INTEGER REFERENCES keyboards(id)
        );
        CREATE INDEX IF NOT EXISTS idx_key_events_ts ON key_events(ts_ms);",
    )?;
    let has_keyboard_id = {
        let mut stmt = tx.prepare("PRAGMA table_info(key_events)")?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
        let names = names.collect::<rusqlite::Result<Vec<_>>>()?;
        names.iter().any(|name| name == "keyboard_id")
    };
    if !has_keyboard_id {
        tx.execute_batch(
            "ALTER TABLE key_events ADD COLUMN keyboard_id INTEGER REFERENCES keyboards(id)",
        )?;
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

fn record_events(
    conn: &mut Connection,
    keyboard_id: i64,
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
                "INSERT INTO key_events (ts_ms, key, value, suppressed, keyboard_id)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            ) {
                for ev in &buffer {
                    if let Err(e) = stmt.execute(params![
                        ev.ts_ms as i64,
                        format!("{:?}", ev.key),
                        ev.value,
                        if ev.suppressed { 1 } else { 0 },
                        keyboard_id
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

    fn record(conn: &mut Connection, id: i64, suppressed: bool) {
        let (tx, rx) = sync_channel(10);
        tx.send(TrackEvent {
            ts_ms: 123,
            key: KeyCode::KEY_A,
            value: 1,
            suppressed,
        })
        .unwrap();
        drop(tx);
        record_events(conn, id, rx);
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
        assert!(conn
            .execute("INSERT INTO key_events VALUES (0, 'KEY_A', 1, 0, 999)", [])
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
    fn disabled_tracking_needs_no_identity() {
        assert!(Tracker::new(None, None).unwrap().sender.is_none());
    }

    #[test]
    fn tracking_rejects_missing_or_blank_identity_before_opening_database() {
        for identity in [None, Some(""), Some("   ")] {
            let err = Tracker::new(Some(PathBuf::from("/nonexistent/keys.db")), identity)
                .err()
                .unwrap();
            assert!(err.to_string().contains("KEYBOARD_ID"));
        }
    }
}
