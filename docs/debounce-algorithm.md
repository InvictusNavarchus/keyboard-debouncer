# Tiered Debounce Algorithm

This document explains the full decision-making logic inside `src/debounce.rs`,
why it is structured the way it is, and how to reason about the five tunable
parameters.

---

## The Problem Space

Mechanical keyboard switches can produce "contact bounce" — a rapid series of
make/break events that look to the OS like the key was pressed many times in
quick succession. The debouncer's job is to distinguish hardware chatter from
legitimate re-presses.

The challenge is that these two things can look very similar at the event-stream
level. A micro-hold bounce is easy to distinguish:

```
Micro-hold bounce:               Intentional double-letter ("ee"):
  DN(E)                            DN(E)
  UP(E)  [hold: 7ms]               UP(E)  [hold: ~40-60ms]
  DN(E)  [gap: 82ms]               DN(E)  [gap: ~30-50ms UP→DN]
```

But the picture can be genuinely ambiguous for medium-hold bounces:

```
Medium-hold bounce:              Fast same-key re-press:
  DN(E)                            DN(E)
  UP(E)  [hold: 60ms]              UP(E)  [hold: ~40ms]
  DN(E)  [gap: 70ms]               DN(E)  [gap: ~30ms UP→DN]
```

A naive time-window filter ("suppress any re-press within X ms") has to thread a
needle: large enough to catch bounces, small enough not to swallow real typing.
Note that `gap` here always refers to the **UP→DN** interval (release-to-next-press),
not the DN→DN interval (press-to-press) that a typist naturally perceives. They
relate as: `UP→DN = DN→DN − hold`.

---

## Key Observation: Hold Duration Is a Signal

The gap between UP and the next DN is only half the picture. The **hold duration**
of the preceding press is equally informative:

| Prior hold | What it means |
|---|---|
| < 20 ms | Hardware ghost — no human holds a key that briefly. **Reliably** chatter. |
| 20–SHORT_HOLD ms | Suspicious range — could be partial contact *or* fast typing. Context-dependent. |
| ≥ SHORT_HOLD ms | Normal deliberate press. |

A 82 ms re-press gap in isolation is ambiguous. A 82 ms gap *after a 7 ms hold*
is almost certainly a bounce — the switch made ghost contact for 7 ms, broke, then
re-engaged 82 ms later. This is the **Micro** case, and it is unambiguous.

The **Short** case (hold in the 20–SHORT_HOLD range) is less clear-cut: fast typists
can produce holds in this range legitimately. Whether a particular SHORT_HOLD value
creates false positives depends on the typist's rhythm and must be calibrated
empirically using the logs.

---

## The Three Tiers

The algorithm classifies each *forwarded* UP event into one of three tiers based
on the hold duration of that press:

```
HoldTier::Micro   — hold < MICRO_HOLD_THRESHOLD_MS   (default: 20ms)
HoldTier::Short   — hold < SHORT_HOLD_THRESHOLD_MS   (default: 70ms)
HoldTier::Normal  — hold ≥ SHORT_HOLD_THRESHOLD_MS
```

The tier is stored on `PerKeyState.last_hold_tier` and determines which debounce
window will be applied to the **next** DN event for that key.

---

## Threshold Selection

When a DN arrives, the debouncer looks up the active threshold from the stored
tier:

```
Micro  → MICRO_EXTENDED_THRESHOLD_MS  (default: 150ms)
Short  → EXTENDED_THRESHOLD_MS        (default: 100ms)
Normal → THRESHOLD_MS                 (default: 30ms)
```

The DN is suppressed if the gap since the last UP is less than the active
threshold. Otherwise it is forwarded and a new tier will be computed when its
matching UP arrives.

### Visualised

```
                    ┌── prior hold < 20ms (Micro) ──► threshold = 150ms ─┐
                    │                                                      │
DN arrives ──► check last_hold_tier ─┤                                    ├──► suppress or forward
                    │                                                      │
                    ├── prior hold < 70ms (Short) ──► threshold = 100ms ─┤
                    │                                                      │
                    └── prior hold ≥ 70ms (Normal) ─► threshold =  30ms ─┘
```

---

## Full Event-by-Event Logic

### Key Down (value == 1)

1. If no prior UP has ever been recorded → **forward** (first press of the session).
2. Compute `gap = now − last_up`.
3. Select `active_threshold` based on `last_hold_tier` (see above).
4. If `gap < active_threshold` → **suppress** this DN *and* flag `suppressed = true`
   so the matching UP is also suppressed.
5. Otherwise → **forward**, record `last_dn_at = now`.

### Key Up (value == 0)

**Case A — suppressed pair**: if `suppressed == true`, suppress this UP too
(a stray key-release with no matching key-press would confuse applications).
Clear the `suppressed` flag. Update `last_up = now` so the gap for the *next*
DN is measured from this physical release, not from the original forwarded UP.

> **Why keep last_up updated on suppressed UPs?**
> If we only updated `last_up` on forwarded UPs, a chatter bounce's UP would be
> invisible to gap measurement. A subsequent press could then slip through because
> the gap appears to start from the old forwarded UP, making it seem larger than
> it really is.

**Case B — forwarded UP**: forward the event. Then classify the hold into a tier
and store it for the next cycle:

```rust
last_hold_tier = match hold {
    h if h < micro_hold_threshold => HoldTier::Micro,
    h if h < short_hold_threshold => HoldTier::Short,
    _                             => HoldTier::Normal,
};
last_up = now;
```

> **Why not clear last_hold_tier on a suppressed UP?**
> The tier is a property of the last *forwarded* hold, not of the chatter bounce.
> A suppressed bounce's duration is noise. Only a new legitimate press completing
> with a normal hold is a meaningful signal that the switch has recovered.
> Clearing the tier on a suppressed UP would disarm the extended threshold after
> one bounce pair, allowing subsequent bounces in the same chatter cluster to
> slip through.

### Auto-repeat (value == 2)

Forwarded unconditionally. Auto-repeat events are generated by the kernel at a
steady rate while a key is physically held — they are never bounce.

---

## Why These Default Values?

The defaults were derived from real hardware failure data logged from a degrading
mechanical switch:

### `THRESHOLD_MS = 30`

Each key's gap is measured independently — `gap = now − last_up` for that
specific key. This is the **UP→DN** interval: from when the key was released
to when it was next pressed. It is *not* the DN→DN interval a typist would
naturally perceive; those two relate as:

```
UP→DN gap = DN→DN interval − hold duration
```

Observed bounce gaps (UP→DN) after normal holds were 7–20 ms. The base
threshold of 30 ms sits safely above this range for Normal-tier presses.

### `SHORT_HOLD_THRESHOLD_MS = 70`

Two failure modes were observed in the hardware data:
- Ghost contacts with holds of 3–11 ms (clearly hardware chatter)
- Partial-contact presses with holds of 59–63 ms (switch partially engaging)

70 ms catches both. **However, this default is not universally safe.** Real-world
testing shows fast typists can produce deliberate holds of 38–62 ms — well within
the Short tier — leading to false positives on rapid same-key presses. The Short
tier threshold is the parameter most likely to need per-user calibration.

If you observe false positives on fast same-key repetition, lower
`SHORT_HOLD_THRESHOLD_MS` toward `MICRO_HOLD_THRESHOLD_MS`. At the extreme,
setting them equal effectively disables the Short tier, leaving only Micro
protection and the base threshold.

### `EXTENDED_THRESHOLD_MS = 100`

After a Short hold, bounces were observed at 67–70 ms — just above the old 60 ms
extended threshold. 100 ms catches this range.

> **Caveat**: for very fast same-key repetition, the DN→DN interval and the
> hold duration are tightly coupled — you cannot hold longer than the DN→DN.
> A DN→DN interval below ~100 ms forces hold below 100 ms (Short or Micro tier),
> which arms the extended threshold. The resulting UP→DN gap (= DN→DN − hold)
> is then likely to fall below 100 ms and be suppressed. In practice this
> means rapid same-key repetition with a DN→DN interval under ~100 ms may
> be blocked. If you observe false positives, lower `SHORT_HOLD_THRESHOLD_MS`
> so that more fast presses classify as Normal and use the base threshold.

### `MICRO_HOLD_THRESHOLD_MS = 20`

No human intentionally holds a key for less than 20 ms. All observed ghost contacts
in the hardware data had holds of 3–11 ms. 20 ms is a safe upper bound for this
class of chatter.

### `MICRO_EXTENDED_THRESHOLD_MS = 150`

After a Micro hold, bounces were observed at 79–99 ms. 150 ms catches this range.
This threshold is safe because it only arms after a **Micro hold** (< 20 ms) —
and no deliberate press, however fast, produces a hold under 20 ms. The 20 ms
boundary is the reliable dividing line between hardware ghost contacts and any
human keypress. Once a Micro hold is classified, the 150 ms lockout follows
regardless of the typist's speed.

---

## Worked Examples

### Example 1 — Micro-hold bounce (caught ✓)

```
05:45:14.766  DN  gap=4263ms          → forward (legitimate)
              last_dn_at = .766

05:45:14.773  UP  hold=7ms            → forward, classify tier
              hold(7ms) < micro(20ms) → last_hold_tier = Micro
              last_up = .773

05:45:14.855  DN  gap=82ms            → check threshold
              tier=Micro → threshold=150ms
              82ms < 150ms            → SUPPRESS ✓

05:45:14.855  UP  (paired)            → suppress (clears suppressed flag)
              last_up = .855

05:45:15.078  DN  gap=223ms           → check threshold
              tier still Micro → threshold=150ms
              223ms ≥ 150ms           → forward (legitimate re-press)
```

### Example 2 — Medium-hold bounce (caught ✓)

```
05:59:28.222  DN  gap=1946ms          → forward
              last_dn_at = .222

05:59:28.281  UP  hold=59ms           → forward, classify tier
              hold(59ms) ≥ micro(20ms), < short(70ms) → last_hold_tier = Short
              last_up = .281

05:59:28.351  DN  gap=70ms            → check threshold
              tier=Short → threshold=100ms
              70ms < 100ms            → SUPPRESS ✓
```

Under the old logic, 59 ms was above `SHORT_HOLD_THRESHOLD_MS=40`, so the tier
stayed Normal and only the 30 ms base threshold applied — the 70 ms bounce gap
sailed through.

### Example 3 — Intentional double letter ("ee" in "seen")

The outcome depends entirely on how fast the typist presses:

**Slow/normal hold (≥ SHORT_HOLD_THRESHOLD_MS):**
```
  DN(E)  hold=80ms                   → forward
  UP(E)  hold(80ms) ≥ short(70ms)   → last_hold_tier = Normal
  DN(E)  gap=40ms                    → threshold=30ms (Normal tier)
         40ms ≥ 30ms                 → forward ✓
```

**Fast hold (< SHORT_HOLD_THRESHOLD_MS, e.g. 40ms):**
```
  DN(E)  hold=40ms                   → forward
  UP(E)  hold(40ms) < short(70ms)   → last_hold_tier = Short
  DN(E)  gap=30ms                    → threshold=100ms (Extended tier)
         30ms < 100ms                → SUPPRESS ❌  (false positive)
```

Fast typists who produce holds below `SHORT_HOLD_THRESHOLD_MS` will see false
positives on rapid same-key repetition. Lowering `SHORT_HOLD_THRESHOLD_MS`
toward `MICRO_HOLD_THRESHOLD_MS` trades away medium-hold bounce protection
in exchange for eliminating these false positives. Which trade-off is correct
depends on whether your hardware actually exhibits the medium-hold bounce pattern.

---

## Threshold Decision Guide

When tuning for a specific keyboard, adjust these parameters:

| Symptom | Parameter to raise |
|---|---|
| Bounces still slipping through after normal presses | `THRESHOLD_MS` |
| Bounces slipping through after short holds | `EXTENDED_THRESHOLD_MS` |
| Short holds not triggering extended mode | `SHORT_HOLD_THRESHOLD_MS` |
| Bounces slipping through after sub-20ms ghost contacts | `MICRO_EXTENDED_THRESHOLD_MS` |
| Ghost contacts not triggering micro mode | `MICRO_HOLD_THRESHOLD_MS` |
| Intentional double-letters getting suppressed | Lower whichever threshold is arming |

A log entry like:

```
05:45:14.773  ↑ FORWARD  KEY_K  hold=7.12ms  ⚠ micro hold → next threshold=150ms (micro-extended)
05:45:14.855  ↓ SUPPRESS KEY_K  gap=82.00ms < 150ms (micro-extended threshold)  [chatter]
```

tells you exactly which tier armed and why the DN was suppressed, making
threshold tuning an empirical, log-driven process.

---

## State Machine Summary

```
                     ┌─────────────────────────────────┐
                     │           PerKeyState            │
                     │  last_up:        Option<Instant> │
                     │  last_dn_at:     Option<Instant> │
                     │  suppressed:     bool            │
                     │  last_hold_tier: HoldTier        │
                     └──────────────┬──────────────────┘
                                    │
              ┌─────────────────────┼──────────────────────┐
              ▼                     ▼                      ▼
           DN event             UP event              Repeat event
              │                     │                      │
     ┌────────┴────────┐   ┌────────┴──────────┐     always forward
     │ gap < threshold?│   │   suppressed?      │
     │                 │   │                    │
    yes               no  yes                  no
     │                 │   │                    │
  SUPPRESS          FORWARD │               FORWARD
  suppressed=true  update   │               classify hold → tier
  last_dn_at=now   last_dn  │               last_up = now
                            │
                        SUPPRESS (paired UP)
                        suppressed=false
                        last_up = now
```
