import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_COLUMNS, boxSize, columnY, fromFile, layout, toFile, type Link, type Table } from "./diagram-layout.ts";

const table = (name: string, columns = ["id"]): Table => ({
  name, view: false, columns: columns.map((c) => ({ name: c, data_type: "int", primary: c === "id", nullable: false })),
});
const link = (from: string, to: string): Link => ({ name: `${from}_${to}`, table: from, columns: [`${to}_id`], ref_table: to, ref_columns: ["id"] });

test("layout puts referenced tables left of the tables referencing them", () => {
  const p = layout([table("orders"), table("people"), table("items")], [link("orders", "people"), link("items", "orders")]);
  assert.ok(p.get("people")!.x < p.get("orders")!.x);
  assert.ok(p.get("orders")!.x < p.get("items")!.x);
});

test("layout survives a reference cycle and a self-reference, placing every table", () => {
  const p = layout([table("a"), table("b"), table("c")], [link("a", "b"), link("b", "a"), link("c", "c")]);
  assert.equal(p.size, 3);
});

test("layout keeps boxes from overlapping", () => {
  const tables = Array.from({ length: 60 }, (_, i) => table(`t${i}`, Array.from({ length: i % 9 + 1 }, (_, j) => `c${j}`)));
  const links = tables.slice(1, 30).map((t, i) => link(t.name, `t${i % 5}`));
  const p = layout(tables, links);
  const boxes = tables.map((t) => ({ ...p.get(t.name)!, ...boxSize(t) }));
  for (const [i, a] of boxes.entries())
    for (const b of boxes.slice(i + 1))
      assert.ok(a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y, "boxes overlap");
});

test("columnY points at the header for a column the box doesn't draw", () => {
  const wide = table("wide", Array.from({ length: MAX_COLUMNS + 5 }, (_, i) => `c${i}`));
  assert.ok(columnY(wide, "c1") > columnY(wide, "c0"));
  assert.equal(columnY(wide, `c${MAX_COLUMNS + 1}`), columnY(wide, "missing"));
});

test("a diagram file reads back what was written", () => {
  const text = toFile("shop", new Map([["shop.people", { x: 10.4, y: -3 }]]));
  assert.deepEqual(fromFile(text), { version: 1, schema: "shop", tables: [{ name: "shop.people", x: 10, y: -3 }] });
});

test("fromFile refuses malformed files with a reason", () => {
  assert.throws(() => fromFile("{"), /isn't JSON/);
  assert.throws(() => fromFile("[]"), /no version/);
  assert.throws(() => fromFile('{"version": 2, "tables": []}'), /version 2/);
  assert.throws(() => fromFile('{"version": 1}'), /no table list/);
  assert.throws(() => fromFile('{"version": 1, "tables": [{"name": "a", "x": "1", "y": 0}]}'), /without a name or position/);
  assert.throws(() => fromFile('{"version": 1, "tables": [{"name": "a", "x": 1, "y": 0}, {"name": "a", "x": 2, "y": 0}]}'), /twice/);
});
