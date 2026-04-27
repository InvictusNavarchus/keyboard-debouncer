//! kbd-debounce — system-level key chatter filter for Linux (evdev/uinput)
//!
//! Grabs a physical keyboard device exclusively, debounces a target key,
//! then re-injects clean events via a virtual uinput device.
//!
//! Architecture:
//!   Physical keyboard → /dev/input/eventX  (grabbed exclusively)
//!       ↓  [this daemon]
//!   Virtual keyboard  → /dev/input/eventY  (seen by X11/Wayland/apps)
//!
//! Usage:
//!   sudo ./keyboard-debouncer /dev/input/event4
//!   sudo ./keyboard-debouncer /dev/input/event4 --threshold-ms 12

mod debounce;

use chrono::{Local, Timelike};
use debounce::{run_filter_loop, DEFAULT_THRESHOLD_MS, TARGET_KEYS};
use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    Device,
};
use std::{env, os::unix::io::AsRawFd, path::PathBuf, time::Duration};

// ── entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (device_path, threshold, log_forward) = parse_args()?;

    println!("kbd-debounce starting");
    println!("  device    : {}", device_path.display());
    println!("  target keys: {TARGET_KEYS:?}");
    println!("  threshold : {threshold} ms");
    println!("  log fwd   : {log_forward}");

    let mut real = Device::open(&device_path)?;
    println!("  name      : {}", real.name().unwrap_or("(unknown)"));

    let mut virt = build_virtual_device(&real)?;

    // CRITICAL: wait for all keys to be released BEFORE grabbing the device.
    //
    // If we grab first and then drain buffered UP events, X11/Wayland never
    // sees those UPs and believes the keys (e.g. Enter used to launch this
    // program) are still held — triggering endless auto-repeat on startup.
    //
    // By waiting here, the physical UP events flow through the normal kernel
    // → X11 path naturally, leaving X11's state clean before we take over.
    print!("Waiting for all keys to be released…");
    loop {
        let keys_held = real
            .get_key_state()
            .map(|ks| ks.iter().next().is_some())
            .unwrap_or(false);
        if !keys_held {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    println!(" done.");

    // Now safe to grab exclusively — X11 state is clean.
    real.grab()?;
    println!("Device grabbed.");

    // Brief sleep + drain to discard any events that sneaked into the kernel
    // buffer in the tiny window between the last key-state check and grab().
    std::thread::sleep(Duration::from_millis(50));
    while events_available(real.as_raw_fd()) {
        for _ in real.fetch_events()? {}
    }
    println!("Running… (Ctrl-C to stop)\n");

    // Hand off to the debounce filter loop
    run_filter_loop(&mut real, &mut virt, threshold, log_forward)?;
    Ok(())
}

// ── startup drain helper ─────────────────────────────────────────────────────

/// Returns `true` if the device fd has events ready to read right now.
///
/// Uses `poll(timeout=0)` which returns immediately regardless of readiness —
/// it never blocks. This lets us call `fetch_events()` (which *does* block on
/// an empty buffer) only when we know data is waiting, avoiding a stall in the
/// startup drain loop.
fn events_available(fd: std::os::unix::io::RawFd) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: pfd is a valid stack-allocated pollfd, count=1, timeout=0.
    let ret = unsafe { libc::poll(&mut pfd, 1, 0) };
    ret > 0 && (pfd.revents & libc::POLLIN) != 0
}

// ── virtual device construction ───────────────────────────────────────────────

/// Clone the capabilities of the real device into a new virtual uinput device.
/// We mirror keys, LEDs, and misc event types so the virtual keyboard is
/// indistinguishable from the physical one to applications.
fn build_virtual_device(real: &Device) -> Result<VirtualDevice, Box<dyn std::error::Error>> {
    let mut builder = VirtualDeviceBuilder::new()?.name("kbd-debounce");

    // Mirror supported keys (required; without this typing does nothing)
    if let Some(keys) = real.supported_keys() {
        builder = builder.with_keys(keys)?;
    }

    // Mirror misc events (some keyboards use them)
    if let Some(misc) = real.misc_properties() {
        builder = builder.with_msc(misc)?;
    }

    Ok(builder.build()?)
}

// ── argument parsing ──────────────────────────────────────────────────────────

fn parse_args() -> Result<(PathBuf, u64, bool), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let mut device_path: Option<PathBuf> = None;
    let mut threshold_ms = DEFAULT_THRESHOLD_MS;
    let mut log_forward = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--threshold-ms" => {
                i += 1;
                threshold_ms = args
                    .get(i)
                    .ok_or("--threshold-ms requires a value")?
                    .parse::<u64>()
                    .map_err(|_| "--threshold-ms value must be a positive integer")?;
            }
            "--log-forward" | "--verbose" | "-v" => {
                log_forward = true;
            }
            "--help" | "-h" => {
                println!(
                    "Usage: kbd-debounce <DEVICE_PATH> [OPTIONS]\n\
                     \n\
                     Options:\n\
                     DEVICE_PATH          path to keyboard, e.g. /dev/input/event4\n\
                     --threshold-ms N     debounce window in ms (default: {DEFAULT_THRESHOLD_MS})\n\
                     --log-forward, -v    log forwarded events immediately (default: forward logs\n\
                                          shown only when followed by a suppress for context)"
                );
                std::process::exit(0);
            }
            path if path.starts_with('/') => {
                device_path = Some(PathBuf::from(path));
            }
            other => {
                return Err(format!("Unknown argument: {other}").into());
            }
        }
        i += 1;
    }

    let path = match device_path {
        Some(p) => p,
        None => {
            return Err(
                "Error: DEVICE_PATH is required. Run `kbd-debounce --help` for usage.".into(),
            );
        }
    };

    Ok((path, threshold_ms, log_forward))
}

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns a local wall-clock timestamp string: `HH:MM:SS.mmm`
pub fn fmt_ts() -> String {
    let now: chrono::DateTime<Local> = Local::now();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        now.hour(),
        now.minute(),
        now.second(),
        now.timestamp_subsec_millis()
    )
}
