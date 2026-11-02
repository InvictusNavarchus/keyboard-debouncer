export interface TimingEvent {
  ts_ms: number;
  key: string;
  value: number;
  suppressed: number;
}

const CATEGORIES = [
  "Forwarded gap",
  "Suppressed gap",
  "Forwarded hold",
  "Suppressed hold",
] as const;
type Category = (typeof CATEGORIES)[number];
type Samples = Record<Category, number[]>;

export interface TimingStats {
  category: Category;
  count: number;
  min: number | null;
  p5: number | null;
  median: number | null;
  p95: number | null;
  max: number | null;
}

export interface TimingReport {
  overall: TimingStats[];
  keys: { key: string; stats: TimingStats[] }[];
  gapHistogram: { decision: string; counts: number[] }[];
  skippedGaps: number;
  skippedHolds: number;
}

export const GAP_BUCKETS = [
  "<20",
  "20–<30",
  "30–<70",
  "70–<100",
  "100–<150",
  "≥150",
];

function emptySamples(): Samples {
  return {
    "Forwarded gap": [],
    "Suppressed gap": [],
    "Forwarded hold": [],
    "Suppressed hold": [],
  };
}

function summarize(samples: Samples): TimingStats[] {
  return CATEGORIES.map((category) => {
    const values = samples[category].sort((a, b) => a - b);
    // Linear interpolation, including the conventional median for even counts.
    const percentile = (fraction: number): number | null => {
      if (values.length === 0) return null;
      const position = (values.length - 1) * fraction;
      const lower = values[Math.floor(position)] ?? 0;
      const upper = values[Math.ceil(position)] ?? lower;
      return lower + (upper - lower) * (position % 1);
    };
    return {
      category,
      count: values.length,
      min: values[0] ?? null,
      p5: percentile(0.05),
      median: percentile(0.5),
      p95: percentile(0.95),
      max: values.at(-1) ?? null,
    };
  });
}

// Events must arrive in recording order (rowid), not timestamp order: sorting by
// wall time would hide clock reversals and scramble events sharing a millisecond.
export function collectTimings(
  events: Iterable<TimingEvent>,
  affectedKeys: string[],
): TimingReport {
  const overall = emptySamples();
  const keys = new Map(affectedKeys.map((key) => [key, emptySamples()]));
  const states = new Map<
    string,
    { down: TimingEvent | null; up: number | null }
  >();
  let skippedGaps = 0;
  let skippedHolds = 0;
  const add = (key: string, category: Category, duration: number) => {
    overall[category].push(duration);
    keys.get(key)?.[category].push(duration);
  };

  for (const event of events) {
    if (event.value !== 0 && event.value !== 1) continue;
    let state = states.get(event.key);
    if (!state) {
      state = { down: null, up: null };
      states.set(event.key, state);
    }
    const decision = event.suppressed === 1 ? "Suppressed" : "Forwarded";
    if (event.value === 1) {
      if (state.down) {
        skippedHolds++;
        state.up = null;
      }
      if (state.up !== null && event.ts_ms >= state.up) {
        add(event.key, `${decision} gap`, event.ts_ms - state.up);
      } else {
        skippedGaps++;
      }
      state.down = event;
      state.up = null;
    } else {
      if (
        state.down &&
        event.ts_ms >= state.down.ts_ms &&
        event.suppressed === state.down.suppressed
      ) {
        add(event.key, `${decision} hold`, event.ts_ms - state.down.ts_ms);
      } else {
        skippedHolds++;
      }
      state.down = null;
      state.up = event.ts_ms;
    }
  }
  for (const state of states.values()) if (state.down) skippedHolds++;

  return {
    overall: summarize(overall),
    keys: [...keys].map(([key, samples]) => ({
      key,
      stats: summarize(samples),
    })),
    gapHistogram: (["Forwarded", "Suppressed"] as const).map((decision) => {
      const counts = Array<number>(6).fill(0);
      for (const value of overall[`${decision} gap`]) {
        const index = [20, 30, 70, 100, 150].findIndex(
          (limit) => value < limit,
        );
        const bucket = index === -1 ? 5 : index;
        counts[bucket] = (counts[bucket] ?? 0) + 1;
      }
      return { decision, counts };
    }),
    skippedGaps,
    skippedHolds,
  };
}
