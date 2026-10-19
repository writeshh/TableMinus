import { test } from "node:test";
import assert from "node:assert/strict";
import { lastPage, nextSort, pageLabel, totalLabel } from "./table-text.ts";

test("nextSort cycles a column ascending, descending, then off", () => {
  assert.deepEqual(nextSort([], "id", false), [{ column: "id", desc: false }]);
  assert.deepEqual(nextSort([{ column: "id", desc: false }], "id", false), [{ column: "id", desc: true }]);
  assert.deepEqual(nextSort([{ column: "id", desc: true }], "id", false), []);
});

test("nextSort without add replaces the sort on other columns", () => {
  assert.deepEqual(nextSort([{ column: "a", desc: true }], "b", false), [{ column: "b", desc: false }]);
});

test("nextSort with add appends, flips in place and drops a column", () => {
  const a = { column: "a", desc: false };
  assert.deepEqual(nextSort([a], "b", true), [a, { column: "b", desc: false }]);
  assert.deepEqual(nextSort([a, { column: "b", desc: false }], "b", true), [a, { column: "b", desc: true }]);
  assert.deepEqual(nextSort([{ column: "b", desc: true }, a], "b", true), [a]);
});

test("pageLabel marks an uncounted total and an estimate", () => {
  assert.equal(pageLabel({ offset: 0, shown: 0 }), "0–0 of ? rows");
  assert.equal(pageLabel({ offset: 300, shown: 2, total: 302, estimated: true }), `301–302 of ~${(302).toLocaleString()} rows`);
});

test("lastPage trusts an exact total but not an estimate", () => {
  assert.equal(lastPage({ offset: 0, shown: 10, total: 10 }, 10), true);
  assert.equal(lastPage({ offset: 0, shown: 10, total: 10, estimated: true }, 10), false);
  assert.equal(lastPage({ offset: 0, shown: 3 }, 10), true);
});

test("totalLabel says when the total is not counted or estimated", () => {
  assert.equal(totalLabel(undefined, false), "Not counted");
  assert.equal(totalLabel(5, true), "About 5 rows (estimated)");
  assert.equal(totalLabel(5, false), "5 rows");
});
