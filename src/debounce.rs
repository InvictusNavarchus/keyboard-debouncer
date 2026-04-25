//! Keyboard debounce logic: state management, decision making, and event filtering.

use evdev::{uinput::VirtualDevice, Device, InputEvent, Key};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

use colored::Colorize;

// ── configuration ────────────────────────────────────────────────────────────

/// Keys to debounce. Add or remove entries as needed.
pub const TARGET_KEYS: &[Key] = &[Key::KEY_K, Key::KEY_L];

/// Default debounce window. Any DN event for a TARGET_KEY arriving within this
/// duration after the *last forwarded* UP is treated as chatter and dropped
/// (together with its matching UP).
///
/// From the log analysis: bounce intervals were 7–20 ms, real inter-key gaps
/// are well above 30 ms even at 150 WPM. 15 ms is a safe middle ground.
/// personal test data shows that the bounce range between 6-17.9 ms. setting it to 30ms is much safer
pub const DEFAULT_THRESHOLD_MS: u64 = 30;

/// Extended debounce window used when the previous press was
/// abnormally short (< 20 ms). This catches the slower bounce mode where a
/// brief false contact is followed by re-engagement at 33–50 ms later.
pub const EXTENDED_THRESHOLD_MS: u64 = 100;

/// Hold duration threshold to detect a short/bouncy press that should trigger
/// extended debouncing for the next cycle.
const SHORT_HOLD_THRESHOLD_MS: u64 = 50;

// ── per-key debounce state ────────────────────────────────────────────────────

/// Represents a decision made by the debounce logic: whether to forward or suppress
/// an event, along with the reason for that decision. Separates decision logic from
/// logging/emission concerns, making the decision logic testable in isolation.
enum EventDecision {
    Forward { reason: String },
    Suppress { reason: String },
}

/// All mutable debounce state that must be tracked independently for each
/// target key.  One instance lives in the `HashMap<Key, PerKeyState>` that is
/// initialised in `run_filter_loop`.
pub struct PerKeyState {
    /// Instant of the last UP we actually let through (for gap measurement).
    last_forwarded_up: Option<Instant>,
    /// Instant of the last DN we forwarded (to measure hold duration).
    last_dn_at: Option<Instant>,
    /// true while we are inside a suppressed press/release pair (so we can
    /// also swallow the matching UP).
    suppressed: bool,
    /// Was the previous hold abnormally short?  If so, use the extended
    /// threshold for the next DN.
    last_hold_was_short: bool,
    /// Buffered forward log messages, emitted only when a subsequent suppress
    /// provides context.
    pending: Vec<String>,
}

impl PerKeyState {
    pub fn new() -> Self {
        Self {
            last_forwarded_up: None,
            last_dn_at: None,
            suppressed: false,
            last_hold_was_short: false,
            pending: Vec::new(),
        }
    }

    /// Select the active threshold (normal or extended) and return both the duration
    /// and a label for logging, based on whether the previous hold was abnormally short.
    fn active_threshold(&self, normal: Duration, extended: Duration) -> (Duration, &'static str) {
        if self.last_hold_was_short {
            (extended, "extended")
        } else {
            (normal, "normal")
        }
    }

    /// Flush all pending forward log messages to stderr.
    fn flush_pending(&mut self) {
        for msg in self.pending.drain(..) {
            eprintln!("{msg}");
        }
    }
}

// ── hold duration helper ──────────────────────────────────────────────────────

/// Compute the hold duration from `last_dn_at` to now, and return both the
/// `Duration` and a formatted string "XX.XXms" (or "?" if no DN timestamp).
/// Extracted to eliminate duplicated hold-duration calculation throughout the code.
fn fmt_hold(last_dn_at: Option<Instant>) -> (Option<Duration>, String) {
    let hold = last_dn_at.map(|t| Instant::now().duration_since(t));
    let s = hold
        .map(|h| format!("{:.2}ms", h.as_micros() as f64 / 1000.0))
        .unwrap_or_else(|| "?".to_string());
    (hold, s)
}

// ── filter loop ───────────────────────────────────────────────────────────────

/// Core event loop.
///
/// State tracked per-loop (only for TARGET_KEYS):
/// - `last_forwarded_up`  — Instant of the last UP we actually let through
/// - `suppressed`         — true while we are inside a suppressed press/release
///                          pair (so we can also swallow the matching UP)
/// - `pending`            — buffered forward log messages, emitted only when a
///                          subsequent suppress provides context
///
/// Each target key has its own independent `PerKeyState` stored in a HashMap.
pub fn run_filter_loop(
    real: &mut Device,
    virt: &mut VirtualDevice,
    threshold_ms: u64,
    log_forward: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let threshold = Duration::from_millis(threshold_ms);
    let extended_threshold = Duration::from_millis(EXTENDED_THRESHOLD_MS);

    // Initialise independent debounce state for every target key.
    let mut key_states: HashMap<Key, PerKeyState> = TARGET_KEYS
        .iter()
        .map(|&k| (k, PerKeyState::new()))
        .collect();

    loop {
        for event in real.fetch_events()? {
            // Determine whether this event belongs to one of the target keys.
            // Non-target keys are forwarded immediately without touching any state.
            let target_key = match event.kind() {
                evdev::InputEventKind::Key(k) if key_states.contains_key(&k) => k,
                _ => {
                    virt.emit(&[event])?;
                    continue;
                }
            };

            let state = key_states.get_mut(&target_key).unwrap();

            let decision = process_event(&event, target_key, threshold, extended_threshold, state);

            let ts = crate::fmt_ts();
            let forward = apply_decision(decision, &event, state, &ts, log_forward);

            if forward {
                virt.emit(&[event])?;
            }
        }
    }
}

/// Decide whether the event should be forwarded or suppressed, based on debounce logic.
/// Returns an `EventDecision` with the decision and reason, but **does not** handle logging
/// or state updates. This separation makes the decision logic testable in isolation.
///
/// State updates are applied by the caller (`apply_decision`) based on the returned decision.
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
///   • Otherwise forward normally and detect if the hold duration was short.
///
/// Logic for TARGET_KEY repeat (value == 2) and all other keys:
///   • Forward unconditionally.
fn process_event(
    event: &InputEvent,
    key: Key,
    threshold: Duration,
    extended_threshold: Duration,
    state: &PerKeyState,
) -> EventDecision {
    // Auto-repeat (value == 2): forward unconditionally, no decision logic needed.
    if event.value() == 2 {
        return EventDecision::Forward {
            reason: format!("{key:?}  (auto-repeat)"),
        };
    }

    let (active_threshold, threshold_label) = state.active_threshold(threshold, extended_threshold);
    let active_threshold_ms = active_threshold.as_millis();

    match event.value() {
        // ── Key Down ─────────────────────────────────────────────────────────
        1 => match state.last_forwarded_up {
            None => {
                // First press ever — no reference UP to compare against.
                EventDecision::Forward {
                    reason: format!("{key:?}  (first press, no prior UP recorded)"),
                }
            }
            Some(last_up) => {
                let now = Instant::now();
                let gap = now.duration_since(last_up);
                let gap_ms = gap.as_micros() as f64 / 1000.0;

                if gap < active_threshold {
                    EventDecision::Suppress {
                        reason: format!(
                            "{key:?}  gap={gap_ms:.2}ms < {active_threshold_ms}ms ({threshold_label} threshold)  [chatter]"
                        ),
                    }
                } else {
                    EventDecision::Forward {
                        reason: format!(
                            "{key:?}  gap={gap_ms:.2}ms ≥ {active_threshold_ms}ms ({threshold_label} threshold)"
                        ),
                    }
                }
            }
        },

        // ── Key Up ────────────────────────────────────────────────────────────
        0 => {
            if state.suppressed {
                let (_hold, hold_str) = fmt_hold(state.last_dn_at);
                EventDecision::Suppress {
                    reason: format!("{key:?}  hold={hold_str} (paired UP for suppressed DN)"),
                }
            } else {
                let (hold, hold_str) = fmt_hold(state.last_dn_at);
                let reason = if hold
                    .map(|h| h < Duration::from_millis(SHORT_HOLD_THRESHOLD_MS))
                    .unwrap_or(false)
                {
                    let next_ms = EXTENDED_THRESHOLD_MS;
                    format!(
                        "{key:?}  hold={hold_str}  ⚠ short hold → next threshold={next_ms}ms (extended)"
                    )
                } else {
                    format!("{key:?}  hold={hold_str}")
                };
                EventDecision::Forward { reason }
            }
        }

        _ => EventDecision::Forward {
            reason: format!("{key:?}"),
        },
    }
}

/// Apply the decision made by `process_event`, handling state updates and logging.
/// Returns whether to forward the event.
fn apply_decision(
    decision: EventDecision,
    event: &InputEvent,
    state: &mut PerKeyState,
    ts: &str,
    log_forward: bool,
) -> bool {
    match decision {
        EventDecision::Forward { reason } => {
            let msg = format!(
                "{} {} {}",
                ts.dimmed(),
                if event.value() == 1 {
                    "↓ FORWARD".green()
                } else {
                    "↑ FORWARD".green()
                },
                reason
            );

            // Update state BEFORE buffering the log message. This ensures that
            // when a new forwarded DN clears the pending context, the new DN's
            // log message is added *after* the clear, keeping it as context for
            // the subsequent UP and any following suppress.
            match event.value() {
                1 => {
                    // Key Down
                    state.last_dn_at = Some(Instant::now());
                    state.suppressed = false;
                    state.pending.clear(); // New press, clear old pending context
                }
                0 => {
                    // Key Up
                    let now = Instant::now();
                    let hold = state.last_dn_at.map(|t| now.duration_since(t));
                    state.last_hold_was_short = hold
                        .map(|h| h < Duration::from_millis(SHORT_HOLD_THRESHOLD_MS))
                        .unwrap_or(false);
                    state.last_forwarded_up = Some(now);
                }
                _ => {} // Auto-repeat: no state update needed
            }

            if log_forward {
                eprintln!("{msg}");
            } else {
                state.pending.push(msg);
            }

            true
        }

        EventDecision::Suppress { reason } => {
            // Flush pending forward logs to provide context.
            state.flush_pending();
            eprintln!(
                "{} {} {}",
                ts.dimmed(),
                if event.value() == 1 {
                    "↓ SUPPRESS".red().bold()
                } else {
                    "↑ SUPPRESS".red().bold()
                },
                reason
            );

            // Update state based on event type
            match event.value() {
                1 => {
                    // Key Down - mark as suppressed
                    state.suppressed = true;
                    state.last_dn_at = Some(Instant::now());
                }
                0 => {
                    // Key Up - end the suppressed pair
                    state.suppressed = false;
                    state.last_hold_was_short = false;
                }
                _ => {} // Auto-repeat: shouldn't happen
            }

            false
        }
    }
}
