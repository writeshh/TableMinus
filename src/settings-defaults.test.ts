import { test } from "node:test";
import assert from "node:assert/strict";
import { DEFAULT_SETTINGS, fromStored } from "./settings-defaults.ts";

test("fromStored keeps stored values over the defaults", () => {
  assert.deepEqual(fromStored(undefined), DEFAULT_SETTINGS);
  const s = fromStored({ fontSize: 16, resultsMode: "append" });
  assert.equal(s.fontSize, 16);
  assert.equal(s.resultsMode, "append");
  assert.equal(s.pageSize, DEFAULT_SETTINGS.pageSize);
});

test("fromStored turns the old safe mode switch into a level", () => {
  assert.equal(fromStored({ safeDefault: true }).safeLevel, "alert2");
  assert.equal(fromStored({ safeDefault: false }).safeLevel, "silent");
  assert.equal(fromStored({ safeDefault: false, safeLevel: "safe1" }).safeLevel, "safe1");
  assert.equal("safeDefault" in fromStored({ safeDefault: true }), false);
});

test("fromStored leaves the stored object alone", () => {
  const stored = { safeDefault: true };
  fromStored(stored);
  assert.deepEqual(stored, { safeDefault: true });
});
