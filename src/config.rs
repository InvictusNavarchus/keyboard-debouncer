use crate::debounce::{
    DEFAULT_EXTENDED_THRESHOLD_MS, DEFAULT_SHORT_HOLD_THRESHOLD_MS, DEFAULT_THRESHOLD_MS,
};
use evdev::Key;
use std::collections::HashMap;
use std::{env, io, path::PathBuf};

fn load_conf(path: &std::path::Path) -> HashMap<String, String> {
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content
        .lines()
        .filter(|l| !l.trim_start().starts_with('#') && l.contains('='))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn find_device_by_name(target_name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut permission_denied_count = 0usize;

    for entry in std::fs::read_dir("/dev/input")? {
        let path = entry?.path();

        // Only consider event nodes
        let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !fname.starts_with("event") {
            continue;
        }

        match evdev::Device::open(&path) {
            Ok(device) => {
                if device.name().map(str::trim) == Some(target_name) {
                    return Ok(path);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                permission_denied_count += 1;
            }
            Err(_) => {} // Unreadable for other reasons (e.g. not an event device), skip
        }
    }

    if permission_denied_count > 0 {
        Err(format!(
            "Device '{target_name}' not found — {permission_denied_count} input device(s) \
             were unreadable due to permissions.\n\
             \n\
             Fix (recommended, no root needed after setup):\n\
               sudo usermod -aG input $USER   # then log out and back in\n\
             \n\
             Or add a udev rule:\n\
               echo 'SUBSYSTEM==\"input\", GROUP=\"input\", MODE=\"0660\"' \
             | sudo tee /etc/udev/rules.d/99-keyboard-debouncer.rules\n\
               sudo udevadm control --reload && sudo udevadm trigger\n\
             \n\
             Or run once as root:\n\
               sudo keyboard-debouncer [config]"
        )
        .into())
    } else {
        Err(format!(
            "No input device found with name '{target_name}'.\n\
             Check available devices with: sudo libinput list-devices"
        )
        .into())
    }
}

pub fn parse_args(
) -> Result<(PathBuf, Vec<Key>, u64, u64, u64, bool, Option<PathBuf>), Box<dyn std::error::Error>> {
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

    let device_path = if let Some(path_str) = conf.get("DEVICE_PATH") {
        PathBuf::from(path_str)
    } else if let Some(name) = conf.get("KEYBOARD_NAME") {
        find_device_by_name(name)?
    } else {
        return Err("Either DEVICE_PATH or KEYBOARD_NAME must be set in config".into());
    };

    if !device_path.exists() {
        return Err(format!("Device path {} does not exist", device_path.display()).into());
    }

    let track_db = conf.get("TRACK_DB").map(|s| {
        let path_str = s.as_str();
        if path_str.starts_with("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                let mut pb = PathBuf::from(home);
                pb.push(&path_str[2..]);
                return pb;
            }
        }
        PathBuf::from(path_str)
    });

    Ok((
        device_path,
        target_keys,
        threshold_ms,
        extended_threshold_ms,
        short_hold_threshold_ms,
        log_forward,
        track_db,
    ))
}
