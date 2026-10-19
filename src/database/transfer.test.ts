// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { fileName, matchColumns } from "./transfer.ts";

const parts = { connection: "prod db", database: "shop", table: "public.orders", now: new Date(2026, 9, 3, 7, 5, 9) };

test("fileName fills in the pattern and adds the extension", () => {
  assert.equal(fileName("{connection}-{table}-{date}_{time}", parts, "csv.gz"), "prod db-public.orders-2026-10-03_07-05-09.csv.gz");
});

test("fileName keeps unknown placeholders and replaces characters files can't hold", () => {
  assert.equal(fileName("{nope}/{table}:x", { ...parts, table: "a/b" }, "sql"), "{nope}_a_b_x.sql");
});

test("fileName falls back to the table name for an empty pattern", () => {
  assert.equal(fileName("  ", parts, "json"), "public.orders.json");
});

test("matchColumns by name skips fields the table doesn't have", () => {
  assert.deepEqual(matchColumns(["id", "Name", "extra"], ["id", "name"], true, false), ["id", null, null]);
  assert.deepEqual(matchColumns(["id", "Name"], ["id", "name"], true, true), ["id", "name"]);
});

test("matchColumns by order pairs fields with columns position by position", () => {
  assert.deepEqual(matchColumns(["a", "b", "c"], ["x", "y"], false, false), ["x", "y", null]);
});
