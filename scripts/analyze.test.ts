import { Database } from "bun:sqlite";
import { afterEach, expect, test } from "bun:test";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { collectReports, formatReport, parseOptions, rate } from "./analyze.ts";

const databases: Database[] = [];
const directories: string[] = [];
afterEach(() => {
  for (const db of databases.splice(0)) db.close();
  for (const directory of directories.splice(0))
    rmSync(directory, { recursive: true });
});

function fixture(legacy = false, path = ":memory:") {
  const db = new Database(path);
  databases.push(db);
  db.exec(`
    CREATE TABLE key_events (
      ts_ms INTEGER NOT NULL, key TEXT NOT NULL,
      value INTEGER NOT NULL, suppressed INTEGER NOT NULL
      ${legacy ? "" : ", keyboard_id INTEGER REFERENCES keyboards(id)"}
    );
    ${legacy ? "" : "CREATE TABLE keyboards (id INTEGER PRIMARY KEY, identity TEXT NOT NULL UNIQUE); INSERT INTO keyboards VALUES (1, 'desk'), (2, 'travel');"}
  `);
  return db;
}

const start = new Date(2026, 9, 25, 12).getTime();
const end = start + 7 * 86_400_000;

test("counts only key-downs, respects both time boundaries, and separates histories", () => {
  const db = fixture();
  const insert = db.query("INSERT INTO key_events VALUES (?, ?, ?, ?, ?)");
  insert.run(start - 1, "KEY_A", 1, 1, 1);
  insert.run(start, "KEY_A", 1, 0, 1);
  insert.run(start + 1, "KEY_A", 1, 1, 1);
  insert.run(start + 2, "KEY_A", 0, 1, 1);
  insert.run(start + 3, "KEY_A", 2, 0, 1);
  insert.run(end - 1, "KEY_B", 1, 0, 1);
  insert.run(end, "KEY_B", 1, 1, 1);
  insert.run(start, "KEY_C", 1, 1, 2);
  insert.run(start, "KEY_D", 1, 1, null);
  const reports = collectReports(db, start, end);
  expect(reports).toHaveLength(3);
  const desk = reports.find((report) => report.keyboard.id === 1);
  expect(desk?.total).toEqual({ presses: 3, suppressed: 1 });
  expect(desk?.keys).toEqual([{ label: "KEY_A", presses: 2, suppressed: 1 }]);
  expect(reports.find((report) => report.keyboard.id === 2)?.total).toEqual({
    presses: 1,
    suppressed: 1,
  });
  expect(reports.find((report) => report.keyboard.id === null)?.total).toEqual({
    presses: 1,
    suppressed: 1,
  });
  expect(rate({ presses: 3, suppressed: 1 })).toBe("33.33%");
});

test("daily and hourly totals use local dates and combine hours across days", () => {
  const db = fixture();
  const insert = db.query(
    "INSERT INTO key_events VALUES (?, 'KEY_A', 1, ?, 1)",
  );
  const first = new Date(2026, 9, 26, 0, 30);
  insert.run(first.getTime(), 0);
  insert.run(new Date(2026, 9, 27, 0, 30).getTime(), 1);
  const report = collectReports(db, start, end)[0];
  expect(report?.daily).toEqual([
    { label: "2026-10-26", presses: 1, suppressed: 0 },
    { label: "2026-10-27", presses: 1, suppressed: 1 },
  ]);
  expect(report?.hourly).toEqual([{ label: "00", presses: 2, suppressed: 1 }]);
  const text = formatReport(report ? [report] : [], start, end);
  expect(text).toContain("2026-10-25");
  expect(text).toMatch(/23\s+0\s+0\s+—/);
  expect(text).toContain("50.00%");
});

test("ranks affected keys by count rather than rate, with deterministic top 10", () => {
  const db = fixture();
  const insert = db.query("INSERT INTO key_events VALUES (?, ?, 1, ?, 1)");
  for (let index = 0; index < 12; index++) {
    insert.run(start, `KEY_${String(index).padStart(2, "0")}`, 1);
  }
  insert.run(start, "KEY_BUSY", 0);
  insert.run(start, "KEY_BUSY", 1);
  insert.run(start, "KEY_BUSY", 1);
  const keys = collectReports(db, start, end)[0]?.keys;
  expect(keys).toHaveLength(10);
  expect(keys?.[0]).toEqual({ label: "KEY_BUSY", presses: 3, suppressed: 2 });
  expect(keys?.[1]?.label).toBe("KEY_00");
});

test("supports databases predating keyboard identity without migrating them", () => {
  const db = fixture(true);
  db.query("INSERT INTO key_events VALUES (?, 'KEY_A', 1, 0)").run(start);
  const reports = collectReports(db, start, end);
  expect(reports[0]?.keyboard).toEqual({ id: null, identity: null });
  expect(reports[0]?.keys).toEqual([]);
  expect(formatReport(reports, start, end)).toContain(
    "Unidentified historical records",
  );
  expect(formatReport(reports, start, end)).toContain(
    "No suppressed key-downs.",
  );
  expect(db.query("PRAGMA table_info(key_events)").all()).toHaveLength(4);
});

test("empty windows are explicit and zero denominators are not reported as 0%", () => {
  const db = fixture();
  expect(collectReports(db, start, end)).toEqual([]);
  expect(formatReport([], start, end)).toContain("No key-downs recorded");
  expect(rate({ presses: 0, suppressed: 0 })).toBe("—");
});

test("timing reconstruction stays within the window and keyboard, using recording order", () => {
  const db = fixture();
  const insert = db.query("INSERT INTO key_events VALUES (?, ?, ?, ?, ?)");
  insert.run(start - 20, "KEY_A", 1, 0, 1);
  insert.run(start - 10, "KEY_A", 0, 0, 1);
  insert.run(start, "KEY_A", 1, 0, 1);
  insert.run(start, "KEY_A", 0, 0, 1);
  insert.run(start + 20, "KEY_A", 1, 1, 1);
  insert.run(start + 10, "KEY_A", 0, 1, 1);
  insert.run(start + 30, "KEY_A", 1, 0, 2);
  insert.run(start + 50, "KEY_A", 0, 0, 2);
  insert.run(end - 1, "KEY_B", 1, 0, 1);
  insert.run(end, "KEY_B", 0, 0, 1);
  const reports = collectReports(db, start, end);
  const desk = reports.find((report) => report.keyboard.id === 1);
  const travel = reports.find((report) => report.keyboard.id === 2);
  expect(
    desk?.timing.overall.find((row) => row.category === "Forwarded hold")
      ?.median,
  ).toBe(0);
  expect(
    desk?.timing.overall.find((row) => row.category === "Suppressed gap")
      ?.median,
  ).toBe(20);
  expect(
    desk?.timing.overall.find((row) => row.category === "Suppressed hold")
      ?.count,
  ).toBe(0);
  expect(desk?.timing.skippedGaps).toBe(2);
  expect(desk?.timing.skippedHolds).toBe(2);
  expect(
    travel?.timing.overall.find((row) => row.category === "Forwarded gap")
      ?.count,
  ).toBe(0);
  expect(
    travel?.timing.overall.find((row) => row.category === "Forwarded hold")
      ?.median,
  ).toBe(20);
  const text = formatReport(reports, start, end);
  expect(text).toContain("KEY_A timing (ms)");
  expect(text).toContain("Gap buckets (ms)");
  expect(text).toContain("Timing samples skipped: 2 gaps, 2 holds");
});

test("CLI validates arguments and help works without opening a database", () => {
  expect(parseOptions([]).days).toBe(7);
  expect(parseOptions(["--days", "14"]).days).toBe(14);
  for (const value of [
    "0",
    "-1",
    "1.5",
    "Infinity",
    "1e3",
    "9007199254740992",
  ]) {
    expect(() => parseOptions(["--days", value])).toThrow();
  }
  expect(() => parseOptions(["--db", " "])).toThrow();
  expect(() => parseOptions(["--unknown"])).toThrow();
  const result = runCli(["--help"]);
  expect(result.exitCode).toBe(0);
  expect(result.stdout.toString()).toContain("Usage:");
});

function runCli(args: string[], timezone = "UTC") {
  return Bun.spawnSync(
    [process.execPath, join(import.meta.dir, "analyze.ts"), ...args],
    {
      env: { ...process.env, TZ: timezone },
    },
  );
}

test("CLI reads a live WAL database, honors timezone, and never creates missing files", () => {
  const directory = mkdtempSync(join(tmpdir(), "keyboard-analytics-"));
  directories.push(directory);
  const path = join(directory, "keys.db");
  const db = fixture(false, path);
  db.exec("PRAGMA journal_mode = WAL");
  const timestamp = Date.now() - 60_000;
  db.query("INSERT INTO key_events VALUES (?, 'KEY_A', 1, 1, 1)").run(
    timestamp,
  );
  for (const timezone of ["UTC", "Asia/Jakarta"]) {
    const result = runCli(["--db", path], timezone);
    expect(result.exitCode).toBe(0);
    expect(result.stdout.toString()).toContain(`(${timezone})`);
    const hour = new Intl.DateTimeFormat("en-GB", {
      timeZone: timezone,
      hour: "2-digit",
      hourCycle: "h23",
    }).format(timestamp);
    expect(result.stdout.toString()).toMatch(
      new RegExp(`${hour}\\s+1\\s+1\\s+100.00%`),
    );
  }
  const reader = new Database(path, { readonly: true });
  databases.push(reader);
  expect(() => reader.exec("DELETE FROM key_events")).toThrow();
  const missing = join(directory, "missing.db");
  const failed = runCli(["--db", missing]);
  expect(failed.exitCode).toBe(1);
  expect(failed.stderr.toString()).toContain("Analytics failed:");
  expect(existsSync(missing)).toBe(false);
  expect(db.query("SELECT * FROM key_events").all()).toHaveLength(1);
});
