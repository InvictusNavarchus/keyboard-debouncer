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
//!   sudo ./kbd-debounce /dev/input/event4
//!   sudo ./kbd-debounce /dev/input/event4 --threshold-ms 12

mod debounce;

use debounce::{run_filter_loop, DEFAULT_THRESHOLD_MS, TARGET_KEYS};
use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    Device,
};
use std::{
    env,
    os::unix::io::AsRawFd,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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

    // Grab exclusively — events stop reaching X11/Wayland until re-injected
    real.grab()?;
    println!("Device grabbed.");

    // Drain any events already buffered in the kernel queue before the grab
    // (e.g. the Enter keypress used to launch this program from a terminal).
    // Without this, those events are immediately re-injected through the virtual
    // device causing rapid-fire input on startup.
    //
    // IMPORTANT: fetch_events() blocks when the kernel buffer is empty. We must
    // guard every call with events_available() (poll timeout=0) so the drain
    // loop never stalls waiting for input that would leak into run_filter_loop.
    print!("Waiting for all keys to be released…");
    loop {
        let keys_held = real
            .get_key_state()
            .map(|ks| ks.iter().next().is_some())
            .unwrap_or(false);

        // Drain all currently buffered events — non-blocking because we only
        // call fetch_events() when poll confirms data is ready.
        while events_available(real.as_raw_fd()) {
            for _ in real.fetch_events()? {}
        }

        if !keys_held {
            // Wait 50 ms for any in-flight UP events to arrive in the kernel
            // buffer (e.g. the release event for the Enter key used to launch
            // this program), then do one final drain before handing off.
            std::thread::sleep(Duration::from_millis(50));
            while events_available(real.as_raw_fd()) {
                for _ in real.fetch_events()? {}
            }
            break;
        }

        std::thread::sleep(Duration::from_millis(5)); // avoid busy-spin while held
    }
    println!(" done.\nRunning… (Ctrl-C to stop)\n");

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

/// Returns a UTC wall-clock timestamp string: `HH:MM:SS.mmm`
pub fn fmt_ts() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let ms = now.subsec_millis();
    let secs = total_secs % 60;
    let mins = (total_secs / 60) % 60;
    let hours = (total_secs / 3600) % 24;
    format!("{hours:02}:{mins:02}:{secs:02}.{ms:03}")
}
