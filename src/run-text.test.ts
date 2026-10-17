import { test } from "node:test";
import assert from "node:assert/strict";
import { rowsText, runSummary } from "./run-text.ts";

const rows = (n: number, truncated = false) => ({ columns: ["a"], rows: Array.from({ length: n }, () => ["x"]), affected: 0, truncated });
const run = { total: 1, done: 1, failed: 0, affected: 0, stopped: "", ms: 12 };

test("rowsText counts rows, or the rows a statement changed", () => {
  assert.equal(rowsText(rows(1)), "1 row");
  assert.equal(rowsText(rows(2, true)), "2 rows (more not loaded)");
  assert.equal(rowsText({ columns: [], rows: [], affected: 3 }), "3 rows affected");
});

test("runSummary of one statement: its rows, or the rows it changed", () => {
  assert.deepEqual(runSummary({ ...run, only: rows(2) }), ["2 rows · 12 ms", "done"]);
  assert.deepEqual(runSummary({ ...run, only: rows(2, true) }), ["2 rows (first 2; Load more, or raise the row limit) · 12 ms", "done"]);
  assert.deepEqual(runSummary({ ...run, affected: 5 }), ["5 rows affected · 12 ms", "done"]);
});

test("runSummary of several statements counts those that ran and failed", () => {
  assert.deepEqual(runSummary({ ...run, total: 3, done: 3, affected: 4 }), ["3 of 3 statements · 4 rows affected · 12 ms", "done"]);
  assert.deepEqual(runSummary({ ...run, total: 3, done: 2, failed: 1 }), ["2 of 3 statements · 1 failed · 0 rows affected · 12 ms", "error"]);
});

test("runSummary of a stopped run says why, an error only when a statement failed", () => {
  assert.deepEqual(runSummary({ ...run, stopped: "Cancelled", done: 0 }), ["Cancelled", ""]);
  assert.deepEqual(runSummary({ ...run, stopped: "syntax error", failed: 1 }), ["syntax error", "error"]);
});
