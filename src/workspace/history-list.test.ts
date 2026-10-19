import { test } from "node:test";
import assert from "node:assert/strict";
import { byDay, keptSince, prepend } from "./history-list.ts";

const YEAR = 365.25 * 864e5;
const at = (at: number, sql = "select 1") => ({ sql, at, db: null });

test("keptSince drops entries older than the kept years and keeps all with 0", () => {
  const now = 10 * YEAR;
  const list = [at(now - 0.5 * YEAR), at(now - 2 * YEAR)];
  assert.deepEqual(keptSince(list, 1, now), [list[0]]);
  assert.deepEqual(keptSince(list, 0, now), list);
});

test("keptSince treats anything but a list as no history", () => {
  assert.deepEqual(keptSince({ sql: "x" }, 0, 0), []);
  assert.deepEqual(keptSince(undefined, 0, 0), []);
});

test("prepend puts the new entry first and caps the list at 5,000", () => {
  const list = Array.from({ length: 5000 }, (_, i) => at(i));
  const out = prepend(at(-1), list);
  assert.equal(out.length, 5000);
  assert.equal(out[0].at, -1);
  assert.equal(out.at(-1)!.at, 4998);
});

test("byDay filters on the SQL in any case and groups in order", () => {
  const list = [at(3, "SELECT a"), at(2, "update b"), at(1, "select c")];
  const groups = byDay(list, "select", (t) => (t > 1 ? "today" : "yesterday"));
  assert.deepEqual([...groups], [["today", [list[0]]], ["yesterday", [list[2]]]]);
});
