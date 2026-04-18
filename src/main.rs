//! kbd-debounce — system-level key chatter filter for Linux (evdev/uinput)
//!
//! Grabs a physical keyboard device exclusively, debounces a target key
//! (default: KEY_E), then re-injects clean events via a virtual uinput device.
//!
//! Architecture:
//!   Physical keyboard → /dev/input/eventX  (grabbed exclusively)
//!       ↓  [this daemon]
//!   Virtual keyboard  → /dev/input/eventY  (seen by X11/Wayland/apps)
//!
//! Usage:
//!   sudo ./kbd-debounce                  # auto-detect keyboard
//!   sudo ./kbd-debounce /dev/input/event4
//!   sudo ./kbd-debounce /dev/input/event4 --threshold-ms 12

use evdev::{
    uinput::{VirtualDevice, VirtualDeviceBuilder},
    AttributeSet, Device, EventType, InputEvent, InputEventKind, Key,
};
use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

// ── configuration ────────────────────────────────────────────────────────────

/// Key to debounce. Swap this out if a different key starts misbehaving.
const TARGET_KEY: Key = Key::KEY_E;

/// Default debounce window. Any DN event for TARGET_KEY arriving within this
/// duration after the *last forwarded* DN is treated as chatter and dropped
/// (together with its matching UP).
///
/// From the log analysis: bounce intervals were 7–20 ms, real inter-key gaps
/// are well above 30 ms even at 150 WPM. 15 ms is a safe middle ground.
const DEFAULT_THRESHOLD_MS: u64 = 15;

// ── entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (device_path, threshold) = parse_args()?;

    println!("kbd-debounce starting");
    println!("  device    : {}", device_path.display());
    println!("  target key: {TARGET_KEY:?}");
    println!("  threshold : {threshold} ms");

    let mut real = Device::open(&device_path)?;
    println!("  name      : {}", real.name().unwrap_or("(unknown)"));

    let mut virt = build_virtual_device(&real)?;

    // Grab exclusively — events stop reaching X11/Wayland until re-injected
    real.grab()?;
    println!("Device grabbed. Running… (Ctrl-C to stop)\n");

    run_filter_loop(&mut real, &mut virt, threshold)?;
    Ok(())
}

// ── filter loop ───────────────────────────────────────────────────────────────

/// Core event loop.
///
/// State tracked per-loop (only for TARGET_KEY):
/// - `last_forwarded_dn`  — Instant of the last DN we actually let through
/// - `suppressed`         — true while we are inside a suppressed press/release
///                          pair (so we can also swallow the matching UP)
fn run_filter_loop(
    real: &mut Device,
    virt: &mut VirtualDevice,
    threshold_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let threshold = Duration::from_millis(threshold_ms);

    let mut last_forwarded_dn: Option<Instant> = None;
    let mut suppressed = false; // are we currently dropping a bounce press?

    loop {
        for event in real.fetch_events()? {
            let forward = should_forward(
                &event,
                threshold,
                &mut last_forwarded_dn,
                &mut suppressed,
            );

            if forward {
                virt.emit(&[event])?;
            } else {
                log_suppressed(&event);
            }
        }
    }
}

/// Returns `true` if the event should be forwarded to the virtual device.
///
/// Logic for TARGET_KEY DN (value == 1):
///   • If no previous DN was forwarded, let it through and record the time.
///   • If the gap since the last forwarded DN is ≥ threshold → new legitimate
///     press: forward it, reset state.
///   • If the gap is < threshold → bounce: suppress this DN *and* its UP.
///
/// Logic for TARGET_KEY UP (value == 0):
///   • If we just suppressed the matching DN, suppress this UP too (prevents
///     a stray "key release" with no matching "key press").
///   • Otherwise forward normally.
///
/// Logic for TARGET_KEY repeat (value == 2) and all other keys:
///   • Forward unconditionally.
fn should_forward(
    event: &InputEvent,
    threshold: Duration,
    last_forwarded_dn: &mut Option<Instant>,
    suppressed: &mut bool,
) -> bool {
    // Only inspect key events for our target
    if event.kind() != InputEventKind::Key(TARGET_KEY) {
        return true;
    }

    match event.value() {
        // ── Key Down ─────────────────────────────────────────────────────────
        1 => {
            let now = Instant::now();
            let is_bounce = last_forwarded_dn
                .map(|t| now.duration_since(t) < threshold)
                .unwrap_or(false);

            if is_bounce {
                *suppressed = true;
                false
            } else {
                *last_forwarded_dn = Some(now);
                *suppressed = false;
                true
            }
        }

        // ── Key Up ────────────────────────────────────────────────────────────
        0 => {
            if *suppressed {
                // Drop the UP that pairs with the suppressed DN
                *suppressed = false;
                false
            } else {
                true
            }
        }

        // ── Key Repeat (auto-repeat, value == 2) ─────────────────────────────
        // Auto-repeat fires ~300 ms after press then at ~30 Hz — very different
        // from bounce. Always forward.
        _ => true,
    }
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

    // Mirror LEDs so Num/Caps/Scroll-lock indicators still work
    if let Some(leds) = real.supported_leds() {
        builder = builder.with_leds(leds)?;
    }

    // Mirror misc events (some keyboards use them)
    if let Some(misc) = real.misc_properties() {
        builder = builder.with_misc(misc)?;
    }

    Ok(builder.build()?)
}

// ── device auto-detection ─────────────────────────────────────────────────────

/// Find the first input device that looks like a real keyboard:
/// must support KEY_E, KEY_SPACE, and KEY_ENTER.
fn auto_detect_keyboard() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let must_have = [Key::KEY_E, Key::KEY_SPACE, Key::KEY_ENTER];

    for (path, device) in evdev::enumerate() {
        if let Some(keys) = device.supported_keys() {
            if must_have.iter().all(|k| keys.contains(*k)) {
                return Ok(path);
            }
        }
    }
    Err("No keyboard device found. Try passing the path manually (e.g. /dev/input/event4).".into())
}

// ── argument parsing ──────────────────────────────────────────────────────────

fn parse_args() -> Result<(PathBuf, u64), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();

    let mut device_path: Option<PathBuf> = None;
    let mut threshold_ms = DEFAULT_THRESHOLD_MS;

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
            "--help" | "-h" => {
                println!(
                    "Usage: kbd-debounce [DEVICE_PATH] [--threshold-ms N]\n\
                     \n\
                     DEVICE_PATH      path to keyboard, e.g. /dev/input/event4\n\
                     --threshold-ms N debounce window in ms (default: {DEFAULT_THRESHOLD_MS})\n\
                     \n\
                     Omit DEVICE_PATH to auto-detect the first keyboard."
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
            println!("No device specified — auto-detecting keyboard…");
            auto_detect_keyboard()?
        }
    };

    Ok((path, threshold_ms))
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn log_suppressed(event: &InputEvent) {
    let action = match event.value() {
        1 => "DN",
        0 => "UP",
        _ => "?",
    };
    println!(
        "  [suppressed] {action} {:?}  (chatter within threshold)",
        event.kind()
    );
}
