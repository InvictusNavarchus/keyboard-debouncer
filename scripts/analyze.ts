import { Database } from "bun:sqlite";
import { parseArgs } from "node:util";
import {
  collectTimings,
  GAP_BUCKETS,
  type TimingEvent,
  type TimingReport,
  type TimingStats,
} from "./timing.ts";

const DAY_MS = 86_400_000;
const DEFAULT_DB = "/var/lib/keyboard-debouncer/keys.db";
const HELP = `Usage: bun scripts/analyze.ts [--db PATH] [--days N]

  --db PATH  Tracker database (default: ${DEFAULT_DB})
  --days N   Rolling window in whole 24-hour days (default: 7)
  --help     Show this help

Read-only report in local time, separated by keyboard.
Suppression rate = suppressed key-downs / all key-downs.
Releases and auto-repeats are excluded from press counts.
Gap/hold statistics use recorded monotonic durations where available.
Historical timings are reconstructed from millisecond wall-clock event pairs.`;

interface Counts {
  presses: number;
  suppressed: number;
}

interface Group extends Counts {
  label: string;
}

interface Keyboard {
  id: number | null;
  identity: string | null;
}

export interface KeyboardReport {
  keyboard: Keyboard;
  total: Counts;
  daily: Group[];
  hourly: Group[];
  keys: Group[];
  timing: TimingReport;
}

export function parseOptions(args: string[]) {
  const { values } = parseArgs({
    args,
    options: {
      db: { type: "string", default: DEFAULT_DB },
      days: { type: "string", default: "7" },
      help: { type: "boolean", default: false },
    },
    strict: true,
    allowPositionals: false,
  });
  const days = Number(values.days);
  if (!/^\d+$/.test(values.days) || !Number.isSafeInteger(days) || days < 1) {
    throw new Error("--days must be a positive whole number.");
  }
  const start = Date.now() - days * DAY_MS;
  if (!Number.isFinite(new Date(start).getTime())) {
    throw new Error("--days exceeds the supported date range.");
  }
  if (!values.db.trim()) throw new Error("--db requires a non-empty path.");
  return { db: values.db, days, help: values.help };
}

// The only dynamic SQL fragments are these internal expressions, never CLI input.
const GROUPS = {
  daily: "strftime('%Y-%m-%d', ts_ms / 1000.0, 'unixepoch', 'localtime')",
  hourly: "strftime('%H', ts_ms / 1000.0, 'unixepoch', 'localtime')",
  keys: "key",
} as const;

export function collectReports(
  db: Database,
  start: number,
  end: number,
): KeyboardReport[] {
  // A consistent snapshot across all aggregates, even while the daemon writes.
  return db.transaction(() => {
    const columns = db
      .query<{ name: string }, []>("PRAGMA table_info(key_events)")
      .all()
      .map((column) => column.name);
    const hasIdentity = columns.includes("keyboard_id");
    const timingColumns = ["gap_ms", "hold_ms", "timing_version"]
      .map((name) => (columns.includes(name) ? name : `NULL AS ${name}`))
      .join(", ");
    const keyboards = hasIdentity
      ? db
          .query<Keyboard, [number, number]>(
            `
          SELECT DISTINCT e.keyboard_id AS id, k.identity
          FROM key_events e LEFT JOIN keyboards k ON k.id = e.keyboard_id
          WHERE e.value = 1 AND e.ts_ms >= ? AND e.ts_ms < ?
          ORDER BY k.identity, e.keyboard_id
        `,
          )
          .all(start, end)
      : [{ id: null, identity: null }];

    return keyboards.flatMap((keyboard) => {
      const filter = `value = 1 AND ts_ms >= ? AND ts_ms < ?${hasIdentity ? " AND keyboard_id IS ?" : ""}`;
      const parameters = hasIdentity ? [start, end, keyboard.id] : [start, end];
      const total = db
        .query<Counts, (number | null)[]>(
          `
        SELECT COUNT(*) AS presses,
               COALESCE(SUM(CASE WHEN suppressed = 1 THEN 1 ELSE 0 END), 0) AS suppressed
        FROM key_events WHERE ${filter}
      `,
        )
        .get(...parameters);
      if (!total || total.presses === 0) return [];

      const group = (kind: keyof typeof GROUPS): Group[] =>
        db
          .query<Group, (number | null)[]>(
            `
          SELECT ${GROUPS[kind]} AS label, COUNT(*) AS presses,
                 SUM(CASE WHEN suppressed = 1 THEN 1 ELSE 0 END) AS suppressed
          FROM key_events WHERE ${filter}
          GROUP BY label
          ${kind === "keys" ? "HAVING SUM(CASE WHEN suppressed = 1 THEN 1 ELSE 0 END) > 0 ORDER BY suppressed DESC, presses DESC, label LIMIT 10" : "ORDER BY label"}
        `,
          )
          .all(...parameters);

      const keys = group("keys");
      const events = db
        .query<TimingEvent, (number | null)[]>(
          `
        SELECT ts_ms, key, value, suppressed, ${timingColumns} FROM key_events
        WHERE ts_ms >= ? AND ts_ms < ? AND value IN (0, 1)
        ${hasIdentity ? "AND keyboard_id IS ?" : ""}
        ORDER BY rowid
      `,
        )
        .iterate(...parameters);
      const timing = collectTimings(
        events,
        keys.map((key) => key.label),
      );
      return [
        {
          keyboard,
          total,
          daily: group("daily"),
          hourly: group("hourly"),
          keys,
          timing,
        },
      ];
    });
  })();
}

function localDate(date: Date): string {
  return `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(2, "0")}-${String(date.getDate()).padStart(2, "0")}`;
}

export function rate(counts: Counts): string {
  return counts.presses === 0
    ? "—"
    : `${((counts.suppressed / counts.presses) * 100).toFixed(2)}%`;
}

function table(title: string, rows: Group[]): string {
  return textTable([
    [title, "Presses", "Suppressed", "Rate"],
    ...rows.map((row) => [
      row.label,
      String(row.presses),
      String(row.suppressed),
      rate(row),
    ]),
  ]);
}

function textTable(lines: string[][]): string {
  const widths = (lines[0] ?? []).map((_, index) =>
    lines.reduce(
      (width, line) => Math.max(width, (line[index] ?? "").length),
      0,
    ),
  );
  return lines
    .map((line) =>
      line
        .map((cell, index) =>
          index === 0
            ? cell.padEnd(widths[index] ?? 0)
            : cell.padStart(widths[index] ?? 0),
        )
        .join("  "),
    )
    .join("\n");
}

function timingTable(title: string, stats: TimingStats[]): string {
  const ms = (value: number | null) =>
    value === null ? "—" : value.toFixed(1);
  return textTable([
    [title, "Samples", "Mono", "Approx", "Min", "p5", "Median", "p95", "Max"],
    ...stats.map((row) => [
      row.category,
      String(row.count),
      String(row.measured),
      String(row.reconstructed),
      ...[row.min, row.p5, row.median, row.p95, row.max].map(ms),
    ]),
  ]);
}

export function formatReport(
  reports: KeyboardReport[],
  start: number,
  end: number,
): string {
  const timezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const sections = [
    `Keyboard health | ${new Date(start).toLocaleString()} → ${new Date(end).toLocaleString()} (${timezone})`,
    "Suppression rate = suppressed / all key-downs; excludes releases and repeats.",
    "Recorded decisions, not confirmed hardware faults; unfiltered keys also count.",
    "Daily edge dates are partial. Hourly rows combine the same local hour across dates.",
    "No presses means no recorded activity, not proof the keyboard was idle.",
  ];
  if (reports.length === 0)
    return [...sections, "\nNo key-downs recorded in this window."].join("\n");

  for (const report of reports) {
    const identity =
      report.keyboard.id === null
        ? "Unidentified historical records"
        : `${JSON.stringify(report.keyboard.identity)} (id ${report.keyboard.id})`;
    sections.push(
      `\nKeyboard: ${identity}`,
      table("Overall", [{ label: "Total", ...report.total }]),
    );

    const dates: string[] = [];
    const cursor = new Date(start);
    cursor.setHours(0, 0, 0, 0);
    while (cursor.getTime() < end) {
      dates.push(localDate(cursor));
      cursor.setDate(cursor.getDate() + 1);
    }
    const fill = (labels: string[], groups: Group[]): Group[] => {
      const byLabel = new Map(groups.map((group) => [group.label, group]));
      return labels.map(
        (label) => byLabel.get(label) ?? { label, presses: 0, suppressed: 0 },
      );
    };
    sections.push("", table("Date", fill(dates, report.daily)));
    sections.push(
      "",
      table(
        "Hour",
        fill(
          Array.from({ length: 24 }, (_, hour) =>
            String(hour).padStart(2, "0"),
          ),
          report.hourly,
        ),
      ),
    );
    sections.push(
      "",
      report.keys.length > 0
        ? table("Top keys (by suppressed count)", report.keys)
        : "No suppressed key-downs.",
    );
    sections.push(
      "",
      timingTable("Timing overall (ms)", report.timing.overall),
    );
    sections.push(
      "",
      textTable([
        ["Gap buckets (ms)", ...GAP_BUCKETS],
        ...report.timing.gapHistogram.map((row) => [
          row.decision,
          ...row.counts.map(String),
        ]),
      ]),
    );
    for (const key of report.timing.keys) {
      sections.push("", timingTable(`${key.key} timing (ms)`, key.stats));
    }
    sections.push(
      `Timing samples skipped: ${report.timing.skippedGaps} gaps, ${report.timing.skippedHolds} holds (missing, incomplete, inconsistent, or negative pairs).`,
      "Gap = same-key UP → DN, including suppressed UPs; hold = matching DN → UP.",
      "Gap decision is the next press; hold decision is the paired press. Auto-repeats ignored.",
      "Mono = stored monotonic durations; Approx = historical wall-clock reconstruction. Zero-ms samples retained.",
      "Samples are selected by event wall time; stored durations may start before this window. Approx pairs stay within it.",
      "Restarts, recording gaps, and historical threshold changes are not identifiable; these statistics do not determine an ideal threshold.",
    );
  }
  return sections.join("\n");
}

if (import.meta.main) {
  try {
    const options = parseOptions(Bun.argv.slice(2));
    if (options.help) {
      console.log(HELP);
    } else {
      const end = Date.now();
      const start = end - options.days * DAY_MS;
      const db = new Database(options.db, { readonly: true, strict: true });
      try {
        console.log(formatReport(collectReports(db, start, end), start, end));
      } finally {
        db.close();
      }
    }
  } catch (error: unknown) {
    console.error(
      `Analytics failed: ${error instanceof Error ? error.message : String(error)}`,
    );
    process.exitCode = 1;
  }
}
