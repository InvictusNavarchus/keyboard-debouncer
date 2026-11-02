import { expect, test } from "bun:test";
import { collectTimings, type TimingEvent } from "./timing.ts";

function event(
  ts_ms: number,
  value: number,
  suppressed = 0,
  key = "KEY_A",
): TimingEvent {
  return { ts_ms, value, suppressed, key };
}

test("measures same-key physical gaps and paired holds separately by press decision", () => {
  const report = collectTimings(
    [
      event(100, 1),
      event(105, 1, 0, "KEY_B"),
      event(110, 0),
      event(120, 1, 1),
      event(122, 2),
      event(125, 0, 1),
      event(140, 1),
      event(150, 0),
      event(160, 0, 0, "KEY_B"),
    ],
    ["KEY_A"],
  );
  expect(report.overall).toEqual([
    {
      category: "Forwarded gap",
      count: 1,
      min: 15,
      p5: 15,
      median: 15,
      p95: 15,
      max: 15,
    },
    {
      category: "Suppressed gap",
      count: 1,
      min: 10,
      p5: 10,
      median: 10,
      p95: 10,
      max: 10,
    },
    {
      category: "Forwarded hold",
      count: 3,
      min: 10,
      p5: 10,
      median: 10,
      p95: expect.closeTo(50.5, 8),
      max: 55,
    },
    {
      category: "Suppressed hold",
      count: 1,
      min: 5,
      p5: 5,
      median: 5,
      p95: 5,
      max: 5,
    },
  ]);
  expect(
    report.keys[0]?.stats.find((row) => row.category === "Forwarded hold")
      ?.count,
  ).toBe(2);
  expect(report.skippedGaps).toBe(2);
  expect(report.skippedHolds).toBe(0);
});

test("uses interpolated percentiles and preserves zero-ms holds in recording order", () => {
  const events: TimingEvent[] = [];
  let time = 0;
  for (const duration of [0, 10, 20, 30]) {
    events.push(event(time, 1), event(time + duration, 0));
    time += duration + 100;
  }
  const report = collectTimings(events, []);
  expect(
    report.overall.find((row) => row.category === "Forwarded hold"),
  ).toEqual({
    category: "Forwarded hold",
    count: 4,
    min: 0,
    p5: expect.closeTo(1.5, 8),
    median: 15,
    p95: expect.closeTo(28.5, 8),
    max: 30,
  });
  expect(
    report.overall.find((row) => row.category === "Suppressed hold")?.median,
  ).toBeNull();
});

test("histogram boundaries match strict less-than thresholds and retain long gaps", () => {
  const events: TimingEvent[] = [];
  for (const [index, gap] of [
    0, 19, 20, 29, 30, 69, 70, 99, 100, 149, 150, 10_000,
  ].entries()) {
    const key = `KEY_${index}`;
    events.push(
      event(0, 0, 0, key),
      event(gap, 1, 0, key),
      event(gap + 1, 0, 0, key),
    );
  }
  const report = collectTimings(events, []);
  expect(report.gapHistogram).toEqual([
    { decision: "Forwarded", counts: [2, 2, 2, 2, 2, 2] },
    { decision: "Suppressed", counts: [0, 0, 0, 0, 0, 0] },
  ]);
  expect(report.overall[0]?.max).toBe(10_000);
});

test("counts skipped samples for duplicate presses, orphan releases, clock reversals, mismatched decisions, and trailing holds", () => {
  const report = collectTimings(
    [
      event(100, 1),
      event(110, 0),
      event(120, 1),
      event(130, 1),
      event(140, 0),
      event(145, 0),
      event(144, 1),
      event(143, 0),
      event(150, 1, 1),
      event(160, 0),
      event(170, 1),
    ],
    [],
  );
  expect(report.skippedGaps).toBe(3);
  expect(report.skippedHolds).toBe(5);
  expect(
    report.overall.find((row) => row.category === "Forwarded hold")?.count,
  ).toBe(2);
  expect(
    report.overall.find((row) => row.category === "Suppressed hold")?.count,
  ).toBe(0);
});

test("empty timing sets have no invented samples", () => {
  const report = collectTimings([], ["KEY_A"]);
  expect(
    report.overall.every(
      (row) => row.count === 0 && row.min === null && row.max === null,
    ),
  ).toBe(true);
  expect(report.keys[0]?.stats).toEqual(report.overall);
  expect(report.skippedGaps).toBe(0);
  expect(report.skippedHolds).toBe(0);
});
