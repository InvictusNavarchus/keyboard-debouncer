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
    Device, InputEvent, InputEventKind, Key,
};
use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// ── configuration ────────────────────────────────────────────────────────────

/// Key to debounce. Swap this out if a different key starts misbehaving.
const TARGET_KEY: Key = Key::KEY_K;

/// Default debounce window. Any DN event for TARGET_KEY arriving within this
/// duration after the *last forwarded* UP is treated as chatter and dropped
/// (together with its matching UP).
///
/// From the log analysis: bounce intervals were 7–20 ms, real inter-key gaps
/// are well above 30 ms even at 150 WPM. 15 ms is a safe middle ground.
/// personal test data shows that the bounce range between 6-17.9 ms. setting it to 30ms is much safer
const DEFAULT_THRESHOLD_MS: u64 = 30;

/// Extended debounce window (3x threshold) used when the previous press was
/// abnormally short (< 20 ms). This catches the slower bounce mode where a
/// brief false contact is followed by re-engagement at 33–50 ms later.
const EXTENDED_THRESHOLD_MULTIPLIER: u64 = 2;

/// Hold duration threshold to detect a short/bouncy press that should trigger
/// extended debouncing for the next cycle.
const SHORT_HOLD_THRESHOLD_MS: u64 = 20;

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
    println!("Device grabbed.");

    // Drain any events already buffered in the kernel queue before the grab
    // (e.g. the Enter keypress used to launch this program from a terminal).
    // Without this, those events are immediately re-injected through the virtual
    // device causing rapid-fire input on startup.
    print!("Waiting for all keys to be released…");
    loop {
        let keys_held = real
            .get_key_state()
            .map(|ks| ks.iter().next().is_some())
            .unwrap_or(false);
        if !keys_held {
            break;
        }
        // Consume and discard buffered events (DNs, auto-repeats, etc.)
        // fetch_events() returns immediately when events are queued.
        for _ in real.fetch_events()? {}
    }
    println!(" done.\nRunning… (Ctrl-C to stop)\n");
    run_filter_loop(&mut real, &mut virt, threshold)?;
    Ok(())
}

// ── filter loop ───────────────────────────────────────────────────────────────

/// Core event loop.
///
/// State tracked per-loop (only for TARGET_KEY):
/// - `last_forwarded_up`  — Instant of the last UP we actually let through
/// - `suppressed`         — true while we are inside a suppressed press/release
///                          pair (so we can also swallow the matching UP)
fn run_filter_loop(
    real: &mut Device,
    virt: &mut VirtualDevice,
    threshold_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let threshold = Duration::from_millis(threshold_ms);
    let extended_threshold = Duration::from_millis(threshold_ms * EXTENDED_THRESHOLD_MULTIPLIER);

    let mut last_forwarded_up: Option<Instant> = None;
    let mut last_dn_at: Option<Instant> = None;   // to measure hold duration
    let mut suppressed = false; // are we currently dropping a bounce press?
    let mut last_hold_was_short = false; // was the previous hold abnormally short?

    loop {
        for event in real.fetch_events()? {
            let forward = process_event(
                &event,
                threshold,
                extended_threshold,
                &mut last_forwarded_up,
                &mut last_dn_at,
                &mut suppressed,
                &mut last_hold_was_short,
            );

            if forward {
                virt.emit(&[event])?;
            }
        }
    }
}

/// Decide whether the event should be forwarded, log it with full context,
/// and update all relevant state.
///
/// Logic for TARGET_KEY DN (value == 1):
///   • If no previous UP was forwarded, let it through.
///   • If the gap since the last forwarded UP is ≥ active threshold → new
///     legitimate press: forward it. Uses extended threshold if the previous
///     hold was abnormally short (< 20 ms), indicating a chattering switch.
///   • If the gap is < active threshold → bounce: suppress this DN *and* its UP.
///
/// Logic for TARGET_KEY UP (value == 0):
///   • If we just suppressed the matching DN, suppress this UP too (prevents
///     a stray "key release" with no matching "key press").
///   • Otherwise forward normally, record the time, and detect if the hold
///     duration was short (< 20 ms) to trigger extended threshold next cycle.
///
/// Logic for TARGET_KEY repeat (value == 2) and all other keys:
///   • Forward unconditionally (no logging — would be extremely noisy).
fn process_event(
    event: &InputEvent,
    threshold: Duration,
    extended_threshold: Duration,
    last_forwarded_up: &mut Option<Instant>,
    last_dn_at: &mut Option<Instant>,
    suppressed: &mut bool,
    last_hold_was_short: &mut bool,
) -> bool {
    // Non-target key or repeat: pass through silently.
    if event.kind() != InputEventKind::Key(TARGET_KEY) {
        return true;
    }
    // Auto-repeat (value == 2): also forward silently — very different cadence
    // from bounce and would produce extremely noisy logs.
    if event.value() == 2 {
        return true;
    }

    let ts = fmt_ts();
    let active_threshold = if *last_hold_was_short {
        extended_threshold
    } else {
        threshold
    };
    let active_threshold_ms = active_threshold.as_millis();
    let threshold_label = if *last_hold_was_short { "extended" } else { "normal" };

    match event.value() {
        // ── Key Down ─────────────────────────────────────────────────────────
        1 => {
            let now = Instant::now();

            match *last_forwarded_up {
                None => {
                    // First press ever — no reference UP to compare against.
                    *last_dn_at = Some(now);
                    *suppressed = false;
                    eprintln!(
                        "[{ts}] ↓ {TARGET_KEY:?}  FORWARD   (first press, no prior UP recorded)"
                    );
                    true
                }
                Some(last_up) => {
                    let gap = now.duration_since(last_up);
                    let gap_ms = gap.as_micros() as f64 / 1000.0;

                    if gap < active_threshold {
                        *suppressed = true;
                        eprintln!(
                            "[{ts}] ↓ {TARGET_KEY:?}  SUPPRESS  gap={gap_ms:.2}ms < {active_threshold_ms}ms ({threshold_label} threshold)  [chatter]"
                        );
                        false
                    } else {
                        *last_dn_at = Some(now);
                        *suppressed = false;
                        eprintln!(
                            "[{ts}] ↓ {TARGET_KEY:?}  FORWARD   gap={gap_ms:.2}ms ≥ {active_threshold_ms}ms ({threshold_label} threshold)"
                        );
                        true
                    }
                }
            }
        }

        // ── Key Up ────────────────────────────────────────────────────────────
        0 => {
            if *suppressed {
                // Drop the UP that pairs with the suppressed DN.
                *suppressed = false;
                *last_hold_was_short = false;
                eprintln!(
                    "[{ts}] ↑ {TARGET_KEY:?}  SUPPRESS  (paired UP for suppressed DN)"
                );
                false
            } else {
                let now = Instant::now();
                let hold = last_dn_at.map(|t| now.duration_since(t));
                let hold_ms = hold.map(|h| h.as_micros() as f64 / 1000.0);
                let hold_str = hold_ms
                    .map(|ms| format!("{ms:.2}ms"))
                    .unwrap_or_else(|| "?".to_string());

                *last_hold_was_short = hold
                    .map(|h| h < Duration::from_millis(SHORT_HOLD_THRESHOLD_MS))
                    .unwrap_or(false);
                *last_forwarded_up = Some(now);

                if *last_hold_was_short {
                    let next_ms = active_threshold_ms * EXTENDED_THRESHOLD_MULTIPLIER as u128;
                    eprintln!(
                        "[{ts}] ↑ {TARGET_KEY:?}  FORWARD   hold={hold_str}  ⚠ short hold → next threshold={next_ms}ms (extended)"
                    );
                } else {
                    eprintln!(
                        "[{ts}] ↑ {TARGET_KEY:?}  FORWARD   hold={hold_str}"
                    );
                }
                true
            }
        }

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

    // Mirror misc events (some keyboards use them)
    if let Some(misc) = real.misc_properties() {
        builder = builder.with_msc(misc)?;
    }

    Ok(builder.build()?)
}

// ── device auto-detection ─────────────────────────────────────────────────────

/// Find the first input device that looks like a real keyboard:
/// must support KEY_E, KEY_SPACE, and KEY_ENTER.
fn auto_detect_keyboard() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let must_have = [Key::KEY_K, Key::KEY_SPACE, Key::KEY_ENTER];

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

/// Returns a UTC wall-clock timestamp string: `HH:MM:SS.mmm`
fn fmt_ts() -> String {
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
