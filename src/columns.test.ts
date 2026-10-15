import { test } from "node:test";
import assert from "node:assert/strict";
import { arrange, moveColumn } from "./columns.ts";

const r = { columns: ["id", "name", "age"], rows: [["1", "Ada", null]] };

test("arrange puts the ordered columns first and moves each row's values with them", () => {
  assert.deepEqual(arrange(r, ["age", "id"]), { columns: ["age", "id", "name"], rows: [[null, "1", "Ada"]] });
});

test("arrange ignores names the table no longer has and keeps the object when nothing moves", () => {
  assert.equal(arrange(r, ["gone", "id"]), r);
});

test("moveColumn moves one column to a new position", () => {
  assert.deepEqual(moveColumn(["a", "b", "c", "d"], 0, 2), ["b", "c", "a", "d"]);
  assert.deepEqual(moveColumn(["a", "b", "c", "d"], 3, 0), ["d", "a", "b", "c"]);
});
