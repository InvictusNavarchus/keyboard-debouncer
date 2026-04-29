use crate::debounce::DEFAULT_THRESHOLD_MS;
use evdev::Key;
use std::collections::HashMap;
use std::{env, path::PathBuf};

fn load_conf(path: &std::path::Path) -> HashMap<String, String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn find_device_by_name(target_name: &str) -> Option<PathBuf> {
    for (path, device) in evdev::enumerate() {
        if let Some(name) = device.name() {
            if name.trim() == target_name {
                return Some(path);
            }
        }
    }
    None
}

pub fn parse_args() -> Result<(PathBuf, Vec<Key>, u64, bool), Box<dyn std::error::Error>> {
    let mut args = env::args();
    let conf_path = if let Some(arg) = args.nth(1) {
        if arg == "--help" || arg == "-h" {
            println!(
                "Usage: kbd-debounce [CONFIG_PATH]\n\
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

    let log_forward = conf
        .get("LOG_FORWARD")
        .map(|v| v == "true")
        .unwrap_or(false);

    let device_path = if let Some(path_str) = conf.get("DEVICE_PATH") {
        PathBuf::from(path_str)
    } else if let Some(name) = conf.get("KEYBOARD_NAME") {
        find_device_by_name(name)
            .ok_or_else(|| format!("No input device found with name '{}'", name))?
    } else {
        return Err("Either DEVICE_PATH or KEYBOARD_NAME must be set in config".into());
    };

    if !device_path.exists() {
        return Err(format!("Device path {} does not exist", device_path.display()).into());
    }

    Ok((device_path, target_keys, threshold_ms, log_forward))
}
