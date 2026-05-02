use evdev::Key;
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct TrackEvent {
    pub ts_ms: u64,
    pub key: Key,
    pub value: i32,
    pub suppressed: bool,
}

pub struct Tracker {
    sender: Option<SyncSender<TrackEvent>>,
}

impl Tracker {
    pub fn new(db_path: Option<PathBuf>) -> Self {
        let Some(path) = db_path else {
            return Self { sender: None };
        };

        // Channel capacity of 10,000 should be plenty to handle spikes without dropping
        let (tx, rx) = sync_channel::<TrackEvent>(10000);

        thread::spawn(move || {
            // Create parent directories if they don't exist
            if let Some(parent) = path.parent() {
                if !parent.exists() {
                    if let Err(e) = std::fs::create_dir_all(parent) {
                        eprintln!("Tracker: Failed to create directories for DB: {}", e);
                        return;
                    }
                }
            }

            let mut conn = match Connection::open(&path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!(
                        "Tracker: Failed to open database at {}: {}",
                        path.display(),
                        e
                    );
                    return;
                }
            };

            // Setup WAL for better concurrency and schema
            if let Err(e) = conn.pragma_update(None, "journal_mode", "WAL") {
                eprintln!("Tracker: Failed to enable WAL mode: {}", e);
            }
            if let Err(e) = conn.pragma_update(None, "synchronous", "NORMAL") {
                eprintln!("Tracker: Failed to set synchronous mode: {}", e);
            }

            if let Err(e) = conn.execute(
                "CREATE TABLE IF NOT EXISTS key_events (
                    ts_ms INTEGER NOT NULL,
                    key TEXT NOT NULL,
                    value INTEGER NOT NULL,
                    suppressed INTEGER NOT NULL
                )",
                [],
            ) {
                eprintln!("Tracker: Failed to create table: {}", e);
                return;
            }

            // Create index for fast time-based queries
            if let Err(e) = conn.execute(
                "CREATE INDEX IF NOT EXISTS idx_key_events_ts ON key_events(ts_ms)",
                [],
            ) {
                eprintln!("Tracker: Failed to create index: {}", e);
            }

            // Read from channel and insert in batches
            let mut buffer = Vec::with_capacity(100);

            loop {
                // Wait for at least one event
                match rx.recv() {
                    Ok(event) => {
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
                                "INSERT INTO key_events (ts_ms, key, value, suppressed) VALUES (?1, ?2, ?3, ?4)"
                            ) {
                                for ev in &buffer {
                                    if let Err(e) = stmt.execute(params![
                                        ev.ts_ms as i64,
                                        format!("{:?}", ev.key),
                                        ev.value,
                                        if ev.suppressed { 1 } else { 0 }
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
                    Err(_) => {
                        // Channel closed (main thread exited)
                        break;
                    }
                }
            }
        });

        Self { sender: Some(tx) }
    }

    pub fn track(&self, key: Key, value: i32, suppressed: bool) {
        if let Some(sender) = &self.sender {
            let ts_ms = SystemTime::now()
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
