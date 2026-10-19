// Run with `node --test 'src/**/*.test.ts'`. Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { chartData, fitSpec, guessSpec, niceTicks, numericColumns, pieSlices, xPositions } from "./chartdata.ts";

const result = (columns: string[], rows: (string | null)[][]) => ({ columns, rows, affected: 0 });
const sales = result(["month", "revenue", "orders", "note"], [["2024-01", "10.5", "3", "a"], ["2024-02", null, "4", "7"]]);

test("numericColumns keeps columns of numbers and nulls only", () => {
  assert.deepEqual(numericColumns(sales), ["revenue", "orders"]);
});

test("guessSpec takes the first text column as x and the numbers as y", () => {
  assert.deepEqual(guessSpec(sales), { kind: "bar", x: "month", y: ["revenue", "orders"] });
});

test("guessSpec gives a pie one series", () => {
  assert.deepEqual(guessSpec(sales, "pie").y, ["revenue"]);
});

test("fitSpec drops columns the result no longer has", () => {
  assert.deepEqual(fitSpec({ kind: "line", x: "gone", y: ["orders", "note", "gone"] }, sales), { kind: "line", x: "month", y: ["orders"] });
});

test("chartData reads nulls as gaps", () => {
  assert.deepEqual(chartData(sales, { kind: "line", x: "month", y: ["revenue"] }), {
    labels: ["2024-01", "2024-02"], series: [{ name: "revenue", values: [10.5, null] }],
  });
});

test("niceTicks steps by 1, 2 or 5 times a power of ten", () => {
  assert.deepEqual(niceTicks(0, 87), [0, 20, 40, 60, 80, 100]);
  assert.deepEqual(niceTicks(0.1, 0.3, 2), [0.1, 0.2, 0.3]); // no 0.30000000000000004
});

test("niceTicks widens a flat range to include zero", () => {
  assert.deepEqual(niceTicks(5, 5), [0, 1, 2, 3, 4, 5]);
});

test("xPositions spaces dates by time and text evenly", () => {
  assert.deepEqual(xPositions(["2024-01-01", "2024-01-02", "2024-01-05"]), [0, 0.25, 1]);
  assert.deepEqual(xPositions(["a", "b", "c"]), [0, 0.5, 1]);
});

test("pieSlices folds all but the six largest into Other", () => {
  const labels = ["a", "b", "c", "d", "e", "f", "g", "h"];
  const slices = pieSlices(labels, [8, 1, 7, 6, 5, 4, 3, 2]);
  assert.deepEqual(slices.map((s) => s.label), ["a", "c", "d", "e", "f", "g", "Other"]);
  assert.equal(slices.at(-1)!.value, 3);
  assert.equal(slices.at(-1)!.slot, null);
});

test("pieSlices leaves out zero and negative values", () => {
  assert.deepEqual(pieSlices(["a", "b", "c"], [0, -2, 5]), [{ label: "c", value: 5, slot: 0 }]);
});
