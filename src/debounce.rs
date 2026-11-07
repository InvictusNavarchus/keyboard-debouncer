//! Keyboard debounce logic: state management, decision making, and event filtering.

use crate::tracker::EventTiming;
use crate::watch::{LiveStream, LogLine};
use evdev::{uinput::VirtualDevice, Device, EventSummary, InputEvent, KeyCode};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant, SystemTime},
};

// ── configuration ────────────────────────────────────────────────────────────

/// Default debounce window. Any DN event for a TARGET_KEY arriving within this
/// duration after the *last forwarded* UP is treated as chatter and dropped
/// (together with its matching UP).
///
/// From the log analysis: bounce intervals were 7–20 ms, real inter-key gaps
/// are well above 30 ms even at 150 WPM. Personal test data shows bounce range
/// 6–17.9 ms; 30 ms is a safe baseline.
pub const DEFAULT_THRESHOLD_MS: u64 = 30;

/// Extended debounce window used after a short hold (hold in `[MICRO_HOLD, SHORT_HOLD)`).
/// Catches the slower bounce mode where partial contact is followed by
/// re-engagement at 33–100 ms later.
pub const DEFAULT_EXTENDED_THRESHOLD_MS: u64 = 100;

/// Hold duration threshold for a "short" press. Holds in `[MICRO_HOLD_THRESHOLD_MS,
/// SHORT_HOLD_THRESHOLD_MS)` arm the extended debounce window for the next cycle.
/// Set to 70 ms to also catch medium bounces (59–63 ms observed in hardware data).
pub const DEFAULT_SHORT_HOLD_THRESHOLD_MS: u64 = 70;

/// Hold duration below which a press is classified as hardware ghost contact.
/// No human holds a key for less than this; sub-threshold holds arm the
/// maximum lockout window for the next cycle.
pub const DEFAULT_MICRO_HOLD_THRESHOLD_MS: u64 = 20;

/// Debounce window after a micro hold. Since micro holds indicate pure hardware
/// chatter rather than a real press, a long lockout is safe and necessary.
/// Covers the observed 79–99 ms bounce gaps that follow 3–11 ms ghost contacts.
pub const DEFAULT_MICRO_EXTENDED_THRESHOLD_MS: u64 = 150;

// ── hold tier ────────────────────────────────────────────────────────────────

/// Classifies the duration of the last forwarded keypress to select how
/// aggressive the next debounce window should be.
///
/// - [`Micro`]: hold < `MICRO_HOLD_THRESHOLD_MS` — hardware ghost contact,
///   arms the maximum lockout window.
/// - [`Short`]: hold in `[MICRO_HOLD, SHORT_HOLD)` — suspicious partial contact,
///   arms the extended lockout window.
/// - [`Normal`]: hold ≥ `SHORT_HOLD_THRESHOLD_MS` — legitimate press, uses
///   the base threshold.
#[derive(Default)]
enum HoldTier {
    #[default]
    Normal,
    Short,
    Micro,
}

// ── per-key debounce state ────────────────────────────────────────────────────

/// Represents a decision made by the debounce logic: whether to forward or suppress
/// an event, along with the reason for that decision. Separates decision logic from
/// logging/emission concerns, making the decision logic testable in isolation.
enum EventDecision {
    Forward { reason: String },
    Suppress { reason: String },
}

/// Independent timing state for each key, with suppression/tier state used only
/// for debounce targets. Instances live in `run_filter_loop`'s key map.
pub struct PerKeyState {
    /// Instant of the last UP event (forwarded OR suppressed) for gap measurement.
    /// Updated on every UP — forwarded and suppressed — because physically the key
    /// released at that moment regardless of our forwarding decision.
    last_up: Option<Instant>,
    /// Instant of the last DN, forwarded or suppressed (to measure hold duration).
    last_dn_at: Option<Instant>,
    /// true while we are inside a suppressed press/release pair (so we can
    /// also swallow the matching UP).
    suppressed: bool,
    /// How the previous forwarded hold was classified. Determines which
    /// debounce threshold applies to the next DN event.
    last_hold_tier: HoldTier,
    /// Buffered forward log messages, emitted only when a subsequent suppress
    /// provides context.
    pending: Vec<LogLine>,
}

impl PerKeyState {
    fn timing(&self, value: i32, now: Instant) -> EventTiming {
        EventTiming {
            gap: if value == 1 {
                self.last_up.map(|up| now.duration_since(up))
            } else {
                None
            },
            hold: if value == 0 {
                self.last_dn_at.map(|down| now.duration_since(down))
            } else {
                None
            },
        }
    }

    pub fn new() -> Self {
        Self {
            last_up: None,
            last_dn_at: None,
            suppressed: false,
            last_hold_tier: HoldTier::Normal,
            pending: Vec::new(),
        }
    }

    /// Select the active threshold based on the previous hold's tier, returning
    /// both the duration and a label for logging.
    fn active_threshold(
        &self,
        normal: Duration,
        extended: Duration,
        micro_extended: Duration,
    ) -> (Duration, &'static str) {
        match self.last_hold_tier {
            HoldTier::Micro => (micro_extended, "micro-extended"),
            HoldTier::Short => (extended, "extended"),
            HoldTier::Normal => (normal, "normal"),
        }
    }

    /// Flush all pending forward log messages to stderr and default watch clients.
    fn flush_pending(&mut self, live_stream: &mut LiveStream) {
        for line in self.pending.drain(..) {
            live_stream.emit_stderr(&line);
            live_stream.broadcast(&line, Some(false));
        }
    }
}

// ── hold duration helper ──────────────────────────────────────────────────────

/// Format the already measured hold without reading the clock again.
fn fmt_hold(hold: Option<Duration>) -> String {
    hold.map(|h| format!("{:.2}ms", h.as_micros() as f64 / 1000.0))
        .unwrap_or_else(|| "?".to_string())
}

// ── modifier key detection ────────────────────────────────────────────────────

/// Keys that should never be debounced in `debounce_all` mode.
/// Modifiers, locks, and media controls don't chatter and have
/// different timing semantics (held-down combos, etc.).
fn is_modifier_key(key: KeyCode) -> bool {
    matches!(
        key,
        KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
            | KeyCode::KEY_CAPSLOCK
            | KeyCode::KEY_NUMLOCK
            | KeyCode::KEY_SCROLLLOCK
            | KeyCode::KEY_FN
    )
}

// ── filter loop ───────────────────────────────────────────────────────────────

/// Core event loop.
///
/// Timing state is tracked for every key; suppression/tier state is used only
/// for debounce targets:
/// - `last_up`             — Instant of the last UP (forwarded or suppressed)
/// - `suppressed`         — true while we are inside a suppressed press/release
///   pair (so we can also swallow the matching UP)
/// - `pending`            — buffered forward log messages, emitted only when a
///   subsequent suppress provides context
///
/// Each key has its own independent `PerKeyState` stored in a HashMap.
pub fn run_filter_loop(
    real: &mut Device,
    virt: &mut VirtualDevice,
    keys: &[KeyCode],
    cfg: &crate::config::DebounceConfig,
    tracker: &crate::tracker::Tracker,
    live_stream: &mut LiveStream,
    debounce_all: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let threshold = Duration::from_millis(cfg.threshold_ms);
    let extended_threshold = Duration::from_millis(cfg.extended_threshold_ms);
    let short_hold_threshold = Duration::from_millis(cfg.short_hold_threshold_ms);
    let micro_hold_threshold = Duration::from_millis(cfg.micro_hold_threshold_ms);
    let micro_extended_threshold = Duration::from_millis(cfg.micro_extended_threshold_ms);

    let target_keys: HashSet<KeyCode> = keys.iter().copied().collect();
    let mut key_states: HashMap<KeyCode, PerKeyState> = HashMap::new();

    loop {
        for event in real.fetch_events()? {
            // Only key events need timing and debounce state.
            let target_key = match event.destructure() {
                EventSummary::Key(_, k, _) => k,
                _ => {
                    virt.emit(&[event])?;
                    continue;
                }
            };

            // Capture each clock once, before decisions, formatting, and logging.
            let now = Instant::now();
            let wall_time = SystemTime::now();
            let is_target =
                target_keys.contains(&target_key) || (debounce_all && !is_modifier_key(target_key));
            // Keep timing state for non-target keys too; they are still tracked.
            let state = key_states
                .entry(target_key)
                .or_insert_with(PerKeyState::new);
            let timing = state.timing(event.value(), now);

            if !is_target {
                match event.value() {
                    1 => state.last_dn_at = Some(now),
                    0 => state.last_up = Some(now),
                    _ => {}
                }
                tracker.track(target_key, event.value(), false, wall_time, timing);
                virt.emit(&[event])?;
                continue;
            }

            let decision = process_event(
                &event,
                target_key,
                threshold,
                extended_threshold,
                micro_extended_threshold,
                short_hold_threshold,
                micro_hold_threshold,
                state,
                timing,
            );

            let ts = crate::fmt_ts_from(wall_time);
            let forward = apply_decision(
                decision,
                &event,
                state,
                &ts,
                cfg.log_forward,
                short_hold_threshold,
                micro_hold_threshold,
                now,
                timing,
                live_stream,
            );

            tracker.track(target_key, event.value(), !forward, wall_time, timing);

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
    key: KeyCode,
    threshold: Duration,
    extended_threshold: Duration,
    micro_extended_threshold: Duration,
    short_hold_threshold: Duration,
    micro_hold_threshold: Duration,
    state: &PerKeyState,
    timing: EventTiming,
) -> EventDecision {
    // Auto-repeat (value == 2): forward unconditionally, no decision logic needed.
    if event.value() == 2 {
        return EventDecision::Forward {
            reason: format!("{key:?}  (auto-repeat)"),
        };
    }

    let (active_threshold, threshold_label) =
        state.active_threshold(threshold, extended_threshold, micro_extended_threshold);
    let active_threshold_ms = active_threshold.as_millis();

    match event.value() {
        // ── Key Down ─────────────────────────────────────────────────────────
        1 => match timing.gap {
            None => {
                // First press ever — no reference UP to compare against.
                EventDecision::Forward {
                    reason: format!("{key:?}  (first press, no prior UP recorded)"),
                }
            }
            Some(gap) => {
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
                let hold_str = fmt_hold(timing.hold);
                EventDecision::Suppress {
                    reason: format!("{key:?}  hold={hold_str} (paired UP for suppressed DN)"),
                }
            } else {
                let hold_str = fmt_hold(timing.hold);
                let reason = match timing.hold {
                    Some(h) if h < micro_hold_threshold => {
                        let next_ms = micro_extended_threshold.as_millis();
                        format!("{key:?}  hold={hold_str}  ⚠ micro hold → next threshold={next_ms}ms (micro-extended)")
                    }
                    Some(h) if h < short_hold_threshold => {
                        let next_ms = extended_threshold.as_millis();
                        format!("{key:?}  hold={hold_str}  ⚠ short hold → next threshold={next_ms}ms (extended)")
                    }
                    _ => format!("{key:?}  hold={hold_str}"),
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
    short_hold_threshold: Duration,
    micro_hold_threshold: Duration,
    now: Instant,
    timing: EventTiming,
    live_stream: &mut LiveStream,
) -> bool {
    match decision {
        EventDecision::Forward { reason } => {
            let line = LogLine::forward(ts, event.value() == 1, &reason);

            // Update state BEFORE buffering the log message. This ensures that
            // when a new forwarded DN clears the pending context, the new DN's
            // log message is added *after* the clear, keeping it as context for
            // the subsequent UP and any following suppress.
            match event.value() {
                1 => {
                    // Key Down
                    state.last_dn_at = Some(now);
                    state.suppressed = false;
                    state.pending.clear(); // New press, clear old pending context
                }
                0 => {
                    // Key Up
                    state.last_hold_tier = match timing.hold {
                        Some(h) if h < micro_hold_threshold => HoldTier::Micro,
                        Some(h) if h < short_hold_threshold => HoldTier::Short,
                        _ => HoldTier::Normal,
                    };
                    state.last_up = Some(now);
                }
                _ => {} // Auto-repeat: no state update needed
            }

            if log_forward {
                live_stream.emit_stderr(&line);
                live_stream.broadcast(&line, None);
            } else {
                live_stream.broadcast(&line, Some(true));
                state.pending.push(line);
            }

            true
        }

        EventDecision::Suppress { reason } => {
            // Flush pending forward logs to provide context.
            state.flush_pending(live_stream);
            let line = LogLine::suppress(ts, event.value() == 1, &reason);
            live_stream.emit_stderr(&line);
            live_stream.broadcast(&line, None);

            // Update state based on event type
            match event.value() {
                1 => {
                    // Key Down - mark as suppressed
                    state.suppressed = true;
                    state.last_dn_at = Some(now);
                }
                0 => {
                    // Key Up - end the suppressed pair.
                    // NOTE: do NOT touch last_hold_was_short here. The flag is a
                    // property of the last *forwarded* hold being short, not of the
                    // chatter bounce. Clearing it here is incorrect because:
                    //   1. Suppressed events can never set it to true — only forwarded
                    //      UPs can — so clearing it here was a one-way ratchet that
                    //      always disarmed the extended threshold after one bounce pair.
                    //   2. The bounce duration is noise; it doesn't tell us whether the
                    //      hardware has recovered. Only a new legitimate press completing
                    //      with a normal hold duration is a meaningful signal.
                    // The flag is cleared only when a forwarded UP has a normal hold,
                    // keeping the extended threshold armed for as long as the switch
                    // keeps producing short legitimate holds.
                    state.suppressed = false;
                    // Update last_up so the next DN gap is measured from this
                    // physical release, not from the original forwarded UP. This
                    // prevents a chattery bounce's UP from being invisible to gap
                    // measurement, which could let a subsequent press slip through
                    // despite being within the threshold.
                    state.last_up = Some(now);
                }
                _ => {} // Auto-repeat: shouldn't happen
            }

            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(state: &mut PerKeyState, value: i32, now: Instant) -> (bool, EventTiming, String) {
        let event = InputEvent::new(evdev::EventType::KEY.0, KeyCode::KEY_A.code(), value);
        let timing = state.timing(value, now);
        let decision = process_event(
            &event,
            KeyCode::KEY_A,
            Duration::from_millis(30),
            Duration::from_millis(100),
            Duration::from_millis(150),
            Duration::from_millis(70),
            Duration::from_millis(20),
            state,
            timing,
        );
        let reason = match &decision {
            EventDecision::Forward { reason } | EventDecision::Suppress { reason } => {
                reason.clone()
            }
        };
        let mut live_stream = LiveStream::disabled();
        let forward = apply_decision(
            decision,
            &event,
            state,
            "12:00:00.000",
            false,
            Duration::from_millis(70),
            Duration::from_millis(20),
            now,
            timing,
            &mut live_stream,
        );
        (forward, timing, reason)
    }

    #[test]
    fn hold_log_and_tier_use_the_same_capture_at_boundaries() {
        let base = Instant::now();
        for (micros, expected_tier, label) in [
            (19_999, HoldTier::Micro, "micro hold"),
            (20_000, HoldTier::Short, "short hold"),
            (69_999, HoldTier::Short, "short hold"),
            (70_000, HoldTier::Normal, "hold=70.00ms"),
        ] {
            let mut state = PerKeyState::new();
            let (forward, timing, _) = step(&mut state, 1, base);
            assert!(forward);
            assert!(timing.gap.is_none());
            assert_eq!(state.last_dn_at, Some(base));
            let elapsed = Duration::from_micros(micros);
            let release = base + elapsed;
            let (forward, timing, reason) = step(&mut state, 0, release);
            assert!(forward);
            assert_eq!(timing.hold, Some(elapsed));
            assert_eq!(state.last_up, Some(release));
            assert_eq!(
                std::mem::discriminant(&state.last_hold_tier),
                std::mem::discriminant(&expected_tier)
            );
            assert!(reason.contains(label), "{reason}");
        }
    }

    #[test]
    fn gap_boundary_uses_the_captured_duration_for_every_tier() {
        let base = Instant::now();
        for (tier, threshold) in [
            (HoldTier::Normal, 30),
            (HoldTier::Short, 100),
            (HoldTier::Micro, 150),
        ] {
            let mut state = PerKeyState::new();
            state.last_hold_tier = tier;
            state.last_up = Some(base);
            let just_below = Duration::from_micros(threshold * 1000 - 1);
            let (forward, timing, _) = step(&mut state, 1, base + just_below);
            assert!(!forward);
            assert_eq!(timing.gap, Some(just_below));
            assert_eq!(state.last_dn_at, Some(base + just_below));
            // Reuse the same preceding UP to test exact equality independently.
            let exact = Duration::from_millis(threshold);
            let (forward, timing, _) = step(&mut state, 1, base + exact);
            assert!(forward);
            assert_eq!(timing.gap, Some(exact));
        }
    }

    #[test]
    fn suppressed_releases_update_gap_origin_without_disarming_the_hold_tier() {
        let base = Instant::now();
        let mut state = PerKeyState::new();
        step(&mut state, 1, base);
        step(&mut state, 0, base + Duration::from_millis(10));
        assert!(!step(&mut state, 1, base + Duration::from_millis(30)).0);
        let (forward, timing, _) = step(&mut state, 0, base + Duration::from_millis(35));
        assert!(!forward);
        assert_eq!(timing.hold, Some(Duration::from_millis(5)));
        assert!(matches!(state.last_hold_tier, HoldTier::Micro));
        let (forward, timing, _) = step(&mut state, 1, base + Duration::from_millis(184));
        assert!(!forward);
        assert_eq!(timing.gap, Some(Duration::from_millis(149)));
        step(&mut state, 0, base + Duration::from_millis(190));
        assert!(step(&mut state, 1, base + Duration::from_millis(340)).0);
    }

    #[test]
    fn repeats_have_no_durations_and_do_not_move_press_or_release_captures() {
        let base = Instant::now();
        let mut state = PerKeyState::new();
        step(&mut state, 1, base);
        let (forward, timing, _) = step(&mut state, 2, base + Duration::from_millis(50));
        assert!(forward);
        assert!(timing.gap.is_none() && timing.hold.is_none());
        assert_eq!(state.last_dn_at, Some(base));
        assert!(state.last_up.is_none());
        assert_eq!(
            step(&mut state, 0, base + Duration::from_millis(100))
                .1
                .hold,
            Some(Duration::from_millis(100))
        );
    }

    #[test]
    fn apply_decision_flushes_pending_context_only_to_default_watchers() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;
        use std::time::UNIX_EPOCH;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sock_path = std::env::temp_dir().join(format!(
            "kbd-debouncer-debounce-watch-{}-{nanos}.sock",
            std::process::id()
        ));
        let mut live_stream = LiveStream::bind_at(&sock_path).unwrap();

        let mut c_default = UnixStream::connect(&sock_path).unwrap();
        c_default.set_nonblocking(true).unwrap();
        c_default.write_all(&[0]).unwrap();

        let mut c_forward = UnixStream::connect(&sock_path).unwrap();
        c_forward.set_nonblocking(true).unwrap();
        c_forward.write_all(&[0x01]).unwrap(); // FLAG_LOG_FORWARD, plain text

        let base = Instant::now();
        let mut state = PerKeyState::new();

        for (value, offset_ms) in [(1, 0), (0, 80), (1, 90)] {
            let now = base + Duration::from_millis(offset_ms);
            let event = InputEvent::new(evdev::EventType::KEY.0, KeyCode::KEY_K.code(), value);
            let timing = state.timing(value, now);
            let decision = process_event(
                &event,
                KeyCode::KEY_K,
                Duration::from_millis(30),
                Duration::from_millis(100),
                Duration::from_millis(150),
                Duration::from_millis(70),
                Duration::from_millis(20),
                &state,
                timing,
            );
            apply_decision(
                decision,
                &event,
                &mut state,
                "12:00:00.000",
                false,
                Duration::from_millis(70),
                Duration::from_millis(20),
                now,
                timing,
                &mut live_stream,
            );
        }

        let mut buf_def = String::new();
        let _ = c_default.read_to_string(&mut buf_def);
        let mut buf_fwd = String::new();
        let _ = c_forward.read_to_string(&mut buf_fwd);

        // Both watchers see the exact same 3 lines (↓ FORWARD, ↑ FORWARD, ↓ SUPPRESS)
        // with zero duplicate FORWARD lines on the --log-forward watcher.
        assert_eq!(buf_def.lines().count(), 3, "{buf_def}");
        assert_eq!(buf_def, buf_fwd);

        let _ = std::fs::remove_file(&sock_path);
    }
}
