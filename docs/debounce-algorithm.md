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

The challenge is that these two things look similar at the event-stream level:

```
Bounce:                          Intentional double-letter ("ee"):
  DN(E)                            DN(E)
  UP(E)  [hold: 7ms]               UP(E)  [hold: ~100ms]
  DN(E)  [gap: 82ms]               DN(E)  [gap: ~170ms]
```

A naive time-window filter ("suppress any re-press within X ms") has to thread a
needle: large enough to catch bounces, small enough not to swallow real typing.
This gets harder when hardware degrades and produces bounces at progressively
longer gaps.

---

## Key Observation: Hold Duration Is a Signal

The gap between UP and the next DN is only half the picture. The **hold duration**
of the preceding press is equally informative:

| Prior hold | What it means |
|---|---|
| < 20 ms | Hardware ghost — no human holds a key that briefly. Pure chatter. |
| 20–70 ms | Suspicious partial contact. Real presses rarely feel this short. |
| > 70 ms | Normal deliberate press. |

A 82 ms re-press gap in isolation is ambiguous. A 82 ms gap *after a 7 ms hold*
is almost certainly a bounce — the switch made ghost contact for 7 ms, broke, then
re-engaged 82 ms later. Combining both signals collapses the ambiguity.

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

Normal inter-key gap at 150 WPM is ~80 ms. Even at 200 WPM it's ~60 ms. Observed
bounce gaps (after normal holds) were 7–20 ms. 30 ms sits comfortably in the
clear zone between those ranges.

### `SHORT_HOLD_THRESHOLD_MS = 70`

Two failure modes were observed:
- Ghost contacts with holds of 3–11 ms (clearly hardware chatter)
- Partial-contact presses with holds of 59–63 ms (switch partially engaging)

70 ms catches both while keeping genuine fast presses (typically ≥ 80 ms) in the
Normal tier.

### `EXTENDED_THRESHOLD_MS = 100`

After a Short hold, bounces were observed at 67–70 ms — just above the old 60 ms
extended threshold. 100 ms catches this range while remaining well below the
~170 ms gap for intentional same-key re-presses at 120 WPM.

### `MICRO_HOLD_THRESHOLD_MS = 20`

No human intentionally holds a key for less than 20 ms. All observed ghost contacts
in the hardware data had holds of 3–11 ms. 20 ms is a safe upper bound for this
class of chatter.

### `MICRO_EXTENDED_THRESHOLD_MS = 150`

After a Micro hold, bounces were observed at 79–99 ms. 150 ms catches this range.
The safety margin for intentional double-letter typing is comfortable: a conscious
same-key re-press ("ee" in "seen") at 120 WPM has a gap of ~160–220 ms, and
crucially, the deliberate first press will have a *Normal* hold (≥ 70 ms), so the
150 ms threshold never even arms in normal typing. The Micro threshold only arms
after a ghost contact, not after a real press.

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

```
  DN(E)  hold=~100ms                  → forward
  UP(E)  hold=100ms ≥ short(70ms)    → last_hold_tier = Normal
  DN(E)  gap=~170ms                  → threshold=30ms (Normal tier)
         170ms ≥ 30ms                → forward ✓
```

The deliberate hold (100 ms) puts the tier in Normal, so the next press only
needs to clear the base 30 ms threshold — which a conscious re-press at any
sane typing speed will do with room to spare.

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
