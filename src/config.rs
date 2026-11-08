use crate::debounce::{
    DEFAULT_EXTENDED_THRESHOLD_MS, DEFAULT_MICRO_EXTENDED_THRESHOLD_MS,
    DEFAULT_MICRO_HOLD_THRESHOLD_MS, DEFAULT_SHORT_HOLD_THRESHOLD_MS, DEFAULT_THRESHOLD_MS,
};
use evdev::KeyCode;
use std::collections::HashMap;
use std::{env, io, path::PathBuf};

/// Configuration for debounce filtering.
pub struct DebounceConfig {
    pub threshold_ms: u64,
    pub extended_threshold_ms: u64,
    pub short_hold_threshold_ms: u64,
    pub micro_hold_threshold_ms: u64,
    pub micro_extended_threshold_ms: u64,
    pub log_forward: bool,
    pub debounce_all: bool,
}

/// Top-level application configuration.
pub struct Config {
    pub device_path: PathBuf,
    pub keyboard_name: Option<String>, // Stored for re-resolution on USB re-enumeration
    pub keys: Vec<KeyCode>,
    pub debounce: DebounceConfig,
    pub track_db: Option<PathBuf>,
    pub keyboard_id: Option<String>,
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
    let mut permission_denied_count = 0usize;
    let mut found_any_event_node = false;

    match std::fs::read_dir("/dev/input") {
        Err(e) => {
            return Err(format!("Cannot read /dev/input: {e}").into());
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

    // Didn't find the device in this scan
    if permission_denied_count > 0 {
        Err(format!(
            "Device '{target_name}' not found — {permission_denied_count} input device(s) \
             unreadable due to permissions.\n\
             Fix: sudo usermod -aG input $USER  (then log out and back in)"
        )
        .into())
    } else if !found_any_event_node {
        Err(format!("Device '{target_name}' not found — no event nodes in /dev/input yet").into())
    } else {
        Err(format!("Device '{target_name}' not found in /dev/input").into())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum CliAction {
    Help,
    Version,
    Watch {
        log_forward: bool,
    },
    Run {
        config_path: Option<PathBuf>,
        log_forward: bool,
    },
}

pub fn parse_cli_args<I, T>(args: I) -> Result<CliAction, Box<dyn std::error::Error>>
where
    I: IntoIterator<Item = T>,
    T: AsRef<str>,
{
    let mut explicit_path: Option<PathBuf> = None;
    let mut watch = false;
    let mut log_forward = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        let s = arg.as_ref();
        match s {
            "-h" | "--help" => return Ok(CliAction::Help),
            "-V" | "--version" => return Ok(CliAction::Version),
            "-w" | "--watch" => {
                watch = true;
            }
            "--log-forward" => {
                log_forward = true;
            }
            "-c" | "--config" => {
                let val = iter
                    .next()
                    .ok_or_else(|| format!("Option '{s}' requires a path argument"))?;
                if explicit_path.is_some() {
                    return Err("Multiple configuration files specified".into());
                }
                explicit_path = Some(PathBuf::from(val.as_ref()));
            }
            s if s.starts_with('-') => {
                return Err(format!("Unknown option: '{s}'. Use --help for usage.").into());
            }
            _ => {
                if explicit_path.is_some() {
                    return Err("Multiple configuration files specified".into());
                }
                explicit_path = Some(PathBuf::from(s));
            }
        }
    }

    if watch {
        if explicit_path.is_some() {
            return Err("Option '--watch' cannot be combined with a configuration file".into());
        }
        return Ok(CliAction::Watch { log_forward });
    }

    Ok(CliAction::Run {
        config_path: explicit_path,
        log_forward,
    })
}

fn resolve_config_path(
    explicit_path: Option<PathBuf>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = explicit_path {
        Ok(path)
    } else {
        let local = PathBuf::from("debouncer.conf");
        let etc = PathBuf::from("/etc/debouncer.conf");
        if local.exists() {
            Ok(local)
        } else if etc.exists() {
            Ok(etc)
        } else {
            Err(
                "Could not find debouncer.conf in current directory or /etc/. Please create one."
                    .into(),
            )
        }
    }
}

pub fn parse_args() -> Result<Config, Box<dyn std::error::Error>> {
    let (conf_path, log_forward_cli) = match parse_cli_args(env::args().skip(1))? {
        CliAction::Help => {
            println!(
                "Usage: keyboard-debouncer [OPTIONS] [CONFIG_PATH]\n\
                 \n\
                 Options:\n\
                   -c, --config <PATH>  Path to configuration file\n\
                   -w, --watch          Attach to the running service's live event stream\n\
                       --log-forward    Log every forwarded target key event immediately\n\
                   -h, --help           Print help information\n\
                   -V, --version        Print version information\n\
                 \n\
                 If no config path is provided, looks for `debouncer.conf` in the current directory,\n\
                 or `/etc/debouncer.conf`."
            );
            std::process::exit(0);
        }
        CliAction::Version => {
            println!("keyboard-debouncer {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        CliAction::Watch { log_forward } => {
            crate::watch::run_client(log_forward)?;
            std::process::exit(0);
        }
        CliAction::Run {
            config_path,
            log_forward,
        } => (resolve_config_path(config_path)?, log_forward),
    };

    let conf = load_conf(&conf_path);

    let keys_raw = conf
        .get("KEYS")
        .ok_or(format!("KEYS is required in {}", conf_path.display()))?;
    let mut target_keys: Vec<KeyCode> = Vec::new();
    for name in keys_raw.split(',') {
        let name = name.trim();
        target_keys.push(name.parse::<KeyCode>().map_err(|_| {
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

    let micro_hold_threshold_ms = conf
        .get("MICRO_HOLD_THRESHOLD_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MICRO_HOLD_THRESHOLD_MS);

    let micro_extended_threshold_ms = conf
        .get("MICRO_EXTENDED_THRESHOLD_MS")
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_MICRO_EXTENDED_THRESHOLD_MS);

    let log_forward = log_forward_cli
        || conf
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
    let keyboard_id = parse_keyboard_id(&conf, track_db.is_some())?;

    Ok(Config {
        device_path,
        keyboard_name,
        keys: target_keys,
        debounce: DebounceConfig {
            threshold_ms,
            extended_threshold_ms,
            short_hold_threshold_ms,
            micro_hold_threshold_ms,
            micro_extended_threshold_ms,
            log_forward,
            debounce_all,
        },
        track_db,
        keyboard_id,
    })
}

fn parse_keyboard_id(
    conf: &HashMap<String, String>,
    tracking: bool,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let identity = conf
        .get("KEYBOARD_ID")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());
    if tracking && identity.is_none() {
        return Err("KEYBOARD_ID is required when TRACK_DB is enabled; choose a unique label for this physical keyboard".into());
    }
    Ok(identity.map(str::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_identity_is_required_only_for_tracking() {
        let mut conf = HashMap::new();
        assert!(parse_keyboard_id(&conf, true).is_err());
        assert_eq!(parse_keyboard_id(&conf, false).unwrap(), None);
        for blank in ["", "   "] {
            conf.insert("KEYBOARD_ID".into(), blank.into());
            assert!(parse_keyboard_id(&conf, true).is_err());
        }
        conf.insert("KEYBOARD_ID".into(), " desk-01 ".into());
        assert_eq!(
            parse_keyboard_id(&conf, true).unwrap().as_deref(),
            Some("desk-01")
        );
    }

    #[test]
    fn test_parse_cli_args_empty() {
        let res = parse_cli_args(Vec::<&str>::new()).unwrap();
        assert_eq!(
            res,
            CliAction::Run {
                config_path: None,
                log_forward: false
            }
        );
    }

    #[test]
    fn test_parse_cli_args_positional() {
        let res = parse_cli_args(vec!["/custom/path.conf"]).unwrap();
        assert_eq!(
            res,
            CliAction::Run {
                config_path: Some(PathBuf::from("/custom/path.conf")),
                log_forward: false,
            }
        );
    }

    #[test]
    fn test_parse_cli_args_long_flag() {
        let res = parse_cli_args(vec!["--config", "/etc/debouncer.conf"]).unwrap();
        assert_eq!(
            res,
            CliAction::Run {
                config_path: Some(PathBuf::from("/etc/debouncer.conf")),
                log_forward: false,
            }
        );
    }

    #[test]
    fn test_parse_cli_args_short_flag() {
        let res = parse_cli_args(vec!["-c", "/etc/debouncer.conf"]).unwrap();
        assert_eq!(
            res,
            CliAction::Run {
                config_path: Some(PathBuf::from("/etc/debouncer.conf")),
                log_forward: false,
            }
        );
    }

    #[test]
    fn test_parse_cli_args_watch_and_log_forward() {
        assert_eq!(
            parse_cli_args(vec!["--watch"]).unwrap(),
            CliAction::Watch { log_forward: false }
        );
        assert_eq!(
            parse_cli_args(vec!["-w", "--log-forward"]).unwrap(),
            CliAction::Watch { log_forward: true }
        );
        assert_eq!(
            parse_cli_args(vec!["--log-forward"]).unwrap(),
            CliAction::Run {
                config_path: None,
                log_forward: true
            }
        );
        assert!(parse_cli_args(vec!["--watch", "/etc/debouncer.conf"]).is_err());
        assert!(parse_cli_args(vec!["-w", "-c", "/etc/debouncer.conf"]).is_err());
    }

    #[test]
    fn test_parse_cli_args_help() {
        assert_eq!(parse_cli_args(vec!["-h"]).unwrap(), CliAction::Help);
        assert_eq!(parse_cli_args(vec!["--help"]).unwrap(), CliAction::Help);
    }

    #[test]
    fn test_parse_cli_args_version() {
        assert_eq!(parse_cli_args(vec!["-V"]).unwrap(), CliAction::Version);
        assert_eq!(
            parse_cli_args(vec!["--version"]).unwrap(),
            CliAction::Version
        );
    }

    #[test]
    fn test_parse_cli_args_missing_flag_value() {
        assert!(parse_cli_args(vec!["--config"]).is_err());
        assert!(parse_cli_args(vec!["-c"]).is_err());
    }

    #[test]
    fn test_parse_cli_args_unknown_flag() {
        assert!(parse_cli_args(vec!["--unknown"]).is_err());
        assert!(parse_cli_args(vec!["-u"]).is_err());
    }

    #[test]
    fn test_parse_cli_args_multiple_paths() {
        assert!(parse_cli_args(vec!["conf1.conf", "conf2.conf"]).is_err());
        assert!(parse_cli_args(vec!["--config", "conf1.conf", "conf2.conf"]).is_err());
        assert!(parse_cli_args(vec!["conf1.conf", "-c", "conf2.conf"]).is_err());
    }
}
