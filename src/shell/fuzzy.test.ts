// Run with `node --test src/` (Node strips the types itself). Part of `npm run check`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { fuzzy, rank } from "./fuzzy.ts";

test("fuzzy matches the query's characters in order, case-insensitively", () => {
  assert.deepEqual(fuzzy("OIT", "order_items")?.at, [0, 6, 7]);
  assert.equal(fuzzy("tio", "order_items"), null);
});

test("fuzzy matches everything with an empty query", () => {
  assert.deepEqual(fuzzy("", "anything"), { score: 0, at: [] });
});

test("rank puts runs and word starts above scattered matches", () => {
  const names = ["or_d_x", "orders", "customer_orders"];
  assert.deepEqual(rank("ord", names, (n) => n).map((r) => r.item), ["orders", "customer_orders", "or_d_x"]);
});

test("rank prefers word starts, so 'oi' finds order_items before a scattered match", () => {
  const names = ["position", "order_items"];
  assert.equal(rank("oi", names, (n) => n)[0].item, "order_items");
});

test("rank drops what doesn't match", () => {
  assert.deepEqual(rank("zz", ["orders"], (n) => n), []);
});
