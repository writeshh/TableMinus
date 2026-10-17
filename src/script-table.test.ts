import { test } from "node:test";
import assert from "node:assert/strict";
import { SHOWN_ROWS, tableOf } from "./script-table.ts";

test("tableOf takes columns from every object's keys, in first-seen order", () => {
  assert.deepEqual(tableOf([{ a: 1 }, { b: "x", a: null }]), { columns: ["a", "b"], cells: [["1", "NULL"], ["NULL", "x"]], more: 0 });
});

test("tableOf numbers the columns of arrays", () => {
  assert.deepEqual(tableOf([[1, 2], [3]]), { columns: ["1", "2"], cells: [["1", "2"], ["3", "NULL"]], more: 0 });
});

test("tableOf shows plain values and nested objects as text", () => {
  assert.deepEqual(tableOf(["a", { o: { n: 1 } }]), { columns: ["value", "o"], cells: [["a", "NULL"], ["NULL", '{"n":1}']], more: 0 });
});

test("tableOf caps the rows it shows and counts the rest", () => {
  const t = tableOf(Array.from({ length: SHOWN_ROWS + 5 }, (_, i) => ({ i })));
  assert.equal(t.cells.length, SHOWN_ROWS);
  assert.equal(t.more, 5);
});
