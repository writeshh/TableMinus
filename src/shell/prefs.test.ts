import assert from "node:assert/strict";
import test from "node:test";
import { isPref, same } from "./prefs.ts";

test("same compares keys and values, not key order", () => {
  assert.ok(same({ a: 1, b: { c: [1, 2] } }, { b: { c: [1, 2] }, a: 1 }));
  assert.ok(!same({ a: 1 }, { a: 2 }));
  assert.ok(!same({ a: 1 }, { a: 1, b: 2 }));
  assert.ok(!same({ a: undefined }, { b: undefined }));
});

test("isPref leaves out what moved to files of its own", () => {
  assert.ok(isPref("settings"));
  assert.ok(isPref("columns:pg:users"));
  assert.ok(!isPref("sql-tabs:pg"));
  assert.ok(!isPref("history:pg"));
});
