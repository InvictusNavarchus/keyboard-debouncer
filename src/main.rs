//! keyboard-debouncer — system-level key chatter filter for Linux (evdev/uinput)
//!
//! Grabs a physical keyboard device exclusively, debounces a target key,
//! then re-injects clean events via a virtual uinput device.
//!
//! Architecture:
//!   Physical keyboard → /dev/input/eventX  (grabbed exclusively)
//!       ↓  [this daemon]
//!   Virtual keyboard  → /dev/input/eventY  (seen by X11/Wayland/apps)
mod config;
mod debounce;
mod tracker;

use chrono::{Local, Timelike};
use debounce::run_filter_loop;
use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    Device,
};
use std::error::Error as StdError;
use std::{os::unix::io::AsRawFd, time::Duration};

// ── entry point ───────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = run() {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut cfg = config::parse_args()?;

    println!("keyboard-debouncer starting");
    println!("  target keys: {:?}", cfg.keys);
    println!("  debounce all: {}", cfg.debounce.debounce_all);
    println!("  threshold : {} ms", cfg.debounce.threshold_ms);
    println!("  ext thres : {} ms", cfg.debounce.extended_threshold_ms);
    println!("  short hold: {} ms", cfg.debounce.short_hold_threshold_ms);
    println!("  log fwd   : {}", cfg.debounce.log_forward);
    if let Some(db) = &cfg.track_db {
        println!("  tracker   : {}", db.display());
    } else {
        println!("  tracker   : (disabled)");
    }
    println!();

    let tracker = tracker::Tracker::new(cfg.track_db.clone());
    let mut connected_before = false;
    let mut last_message_was_not_found = false;

    loop {
        // Re-resolve device path from name on each attempt (handles USB re-enumeration)
        if let Some(name) = &cfg.keyboard_name {
            match config::find_device_by_name(name) {
                Ok(path) => {
                    cfg.device_path = path;
                    connected_before = true;
                    last_message_was_not_found = false;
                }
                Err(_) => {
                    if !connected_before && !last_message_was_not_found {
                        eprintln!("⚠ Device not found. Waiting for connection…");
                        last_message_was_not_found = true;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                    continue;
                }
            }
        }

        match setup_and_filter(&cfg, &tracker) {
            Ok(()) => {
                eprintln!("⚠ Device disconnected. Waiting for reconnection…");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => {
                eprintln!("Fatal error: {e}");
                return Err(e);
            }
        }
    }
}

/// Setup the device and run the debounce filter loop until the device is disconnected.
///
/// Returns `Ok(())` if the device was unplugged (allowing reconnection retry).
/// Returns `Err(_)` for fatal errors.
fn setup_and_filter(
    cfg: &config::Config,
    tracker: &tracker::Tracker,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut real = Device::open(&cfg.device_path)?;

    println!("Device opened");
    println!("  name    : {}", real.name().unwrap_or("(unknown)"));
    println!("  path    : {}", cfg.device_path.display());

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

    // Hand off to the debounce filter loop. If the device is unplugged,
    // this will return Err(ENODEV) which we convert to Ok() to trigger reconnection.
    match run_filter_loop(
        &mut real,
        &mut virt,
        &cfg.keys,
        &cfg.debounce,
        tracker,
        cfg.debounce.debounce_all,
    ) {
        Err(e) if is_device_disconnected(&e) => Ok(()),
        other => other,
    }
}

// ── device disconnection detection ─────────────────────────────────────────────

/// Check if an error is caused by device disconnection.
/// Returns true for:
/// - ENODEV (errno 19): device removed from running file descriptor
/// - ENOENT (errno 2): device node deleted, can't open on reconnection attempt
fn is_device_disconnected(err: &Box<dyn std::error::Error>) -> bool {
    let mut current: Option<&dyn StdError> = Some(err.as_ref());
    while let Some(err) = current {
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            match io_err.raw_os_error() {
                Some(19) => return true, // ENODEV — device removed from running handle
                Some(2) => return true,  // ENOENT — device node gone, can't open
                _ => {}
            }
        }
        current = err.source();
    }
    false
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
    let mut builder = VirtualDeviceBuilder::new()?.name("keyboard-debouncer");

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

// ── helpers ───────────────────────────────────────────────────────────────────

/// Returns a local wall-clock timestamp string: `HH:MM:SS.mmm`
pub fn fmt_ts_from(now: std::time::SystemTime) -> String {
    let now: chrono::DateTime<Local> = now.into();
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        now.hour(),
        now.minute(),
        now.second(),
        now.timestamp_subsec_millis()
    )
}
