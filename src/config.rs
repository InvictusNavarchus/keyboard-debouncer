use crate::debounce::{
    DEFAULT_EXTENDED_THRESHOLD_MS, DEFAULT_SHORT_HOLD_THRESHOLD_MS, DEFAULT_THRESHOLD_MS,
};
use evdev::Key;
use std::collections::HashMap;
use std::time::Duration;
use std::{env, io, path::PathBuf};

/// Configuration for debounce filtering.
pub struct DebounceConfig {
    pub threshold_ms: u64,
    pub extended_threshold_ms: u64,
    pub short_hold_threshold_ms: u64,
    pub log_forward: bool,
    pub debounce_all: bool,
}

/// Top-level application configuration.
pub struct Config {
    pub device_path: PathBuf,
    pub keyboard_name: Option<String>, // Stored for re-resolution on USB re-enumeration
    pub keys: Vec<Key>,
    pub debounce: DebounceConfig,
    pub track_db: Option<PathBuf>,
}

fn load_conf(path: &std::path::Path) -> HashMap<String, String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

pub fn find_device_by_name(target_name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let poll_interval = Duration::from_millis(500);
    let mut last_permission_denied = 0usize;

    loop {
        let mut permission_denied_count = 0usize;
        let mut found_any_event_node = false;

        match std::fs::read_dir("/dev/input") {
            Err(e) => {
                eprintln!("Warning: cannot read /dev/input: {e} — retrying…");
                std::thread::sleep(poll_interval);
                continue;
            }
            Ok(entries) => {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    if !fname.starts_with("event") {
                        continue;
                    }
                    found_any_event_node = true;

                    match evdev::Device::open(&path) {
                        Ok(device) => {
                            if device.name().map(str::trim) == Some(target_name) {
                                eprintln!("Found device '{target_name}' at {}", path.display());
                                return Ok(path);
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                            permission_denied_count += 1;
                        }
                        Err(_) => {}
                    }
                }
            }
        }

        // Log permission errors once, not on every poll cycle
        if permission_denied_count > 0 && permission_denied_count != last_permission_denied {
            eprintln!(
                "Warning: {permission_denied_count} input device(s) unreadable due to permissions.\n\
                 Fix: sudo usermod -aG input $USER  (then log out/in)\n\
                 Continuing to wait for '{target_name}'…"
            );
            last_permission_denied = permission_denied_count;
        }

        if !found_any_event_node {
            eprintln!("Warning: no event nodes in /dev/input yet — waiting…");
        }

        std::thread::sleep(poll_interval);
    }
}

pub fn parse_args() -> Result<Config, Box<dyn std::error::Error>> {
    let mut args = env::args();
    let conf_path = if let Some(arg) = args.nth(1) {
        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: keyboard-debouncer [CONFIG_PATH]\n\
                 \n\
                 If no config path is provided, looks for `debouncer.conf` in the current directory, \n\
                 or `/etc/debouncer.conf`."
            );
            std::process::exit(0);
        }
        PathBuf::from(arg)
    } else {
        let local = PathBuf::from("debouncer.conf");
        let etc = PathBuf::from("/etc/debouncer.conf");
        if local.exists() {
            local
        } else if etc.exists() {
            etc
        } else {
            return Err(
                "Could not find debouncer.conf in current directory or /etc/. Please create one."
                    .into(),
            );
        }
    };

    let conf = load_conf(&conf_path);

    let keys_raw = conf
        .get("KEYS")
        .ok_or(format!("KEYS is required in {}", conf_path.display()))?;
    let mut target_keys: Vec<Key> = Vec::new();
    for name in keys_raw.split(',') {
        let name = name.trim();
        target_keys.push(name.parse::<Key>().map_err(|_| {
            format!("Unknown key name: '{name}'. Use evtest format, e.g. KEY_K, KEY_ENTER")
        })?);
    }
    if target_keys.is_empty() {
        return Err("KEYS value must not be empty".into());
    }

    let threshold_ms = conf
        .get("THRESHOLD_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_THRESHOLD_MS);

    let extended_threshold_ms = conf
        .get("EXTENDED_THRESHOLD_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_EXTENDED_THRESHOLD_MS);

    let short_hold_threshold_ms = conf
        .get("SHORT_HOLD_THRESHOLD_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SHORT_HOLD_THRESHOLD_MS);

    let log_forward = conf
        .get("LOG_FORWARD")
        .map(|v| v == "true")
        .unwrap_or(false);

    let debounce_all = conf
        .get("DEBOUNCE_ALL_KEYS")
        .map(|v| v == "true")
        .unwrap_or(false);

    let keyboard_name = conf.get("KEYBOARD_NAME").cloned();
    let device_path = if let Some(path_str) = conf.get("DEVICE_PATH") {
        let path = PathBuf::from(path_str);
        // Only validate explicit DEVICE_PATH exists; KEYBOARD_NAME resolution happens in main loop
        if !path.exists() {
            return Err(format!("Device path {} does not exist", path.display()).into());
        }
        path
    } else if keyboard_name.is_some() {
        // Device will be discovered in the main loop; use placeholder for now
        PathBuf::from("")
    } else {
        return Err("Either DEVICE_PATH or KEYBOARD_NAME must be set in config".into());
    };

    let track_db = conf.get("TRACK_DB").map(PathBuf::from);

    Ok(Config {
        device_path,
        keyboard_name,
        keys: target_keys,
        debounce: DebounceConfig {
            threshold_ms,
            extended_threshold_ms,
            short_hold_threshold_ms,
            log_forward,
            debounce_all,
        },
        track_db,
    })
}
