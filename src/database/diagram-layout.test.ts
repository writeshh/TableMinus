import { test } from "node:test";
import assert from "node:assert/strict";
import { MAX_COLUMNS, boxSize, columnY, fromFile, layout, roundedPath, route, toFile, type Link, type Rect, type Table } from "./diagram-layout.ts";

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

test("route runs its vertical channel clear of a box sitting in the middle", () => {
  const blocker: Rect = { x: 180, y: 0, w: 40, h: 300 };
  const pts = route({ x: 100, y: 50 }, 1, { x: 400, y: 250 }, -1, [blocker]);
  const x = pts[2].x;
  assert.equal(pts[3].x, x);
  assert.ok(x < blocker.x - 8 || x > blocker.x + blocker.w + 8, `channel at ${x} runs through the box`);
});

test("route goes round outside both ends when the line comes back into the same side", () => {
  const pts = route({ x: 300, y: 40 }, 1, { x: 260, y: 140 }, 1, []);
  assert.ok(pts[2].x >= 316, "the channel is past both right edges");
});

test("roundedPath drops straight-through points and rounds each corner", () => {
  assert.equal(roundedPath([{ x: 0, y: 0 }, { x: 10, y: 0 }, { x: 20, y: 0 }]), "M0 0 L20 0");
  assert.equal(roundedPath([{ x: 0, y: 0 }, { x: 20, y: 0 }, { x: 20, y: 20 }], 6), "M0 0 L14 0 Q20 0 20 6 L20 20");
});

test("a diagram file keeps each table's colour and note", () => {
  const text = toFile(null, new Map([["a", { x: 0, y: 0 }], ["b", { x: 5, y: 5 }]]), new Map([["a", { color: "pink" as const, note: "Owned by billing" }]]));
  assert.deepEqual(fromFile(text).tables, [{ name: "a", x: 0, y: 0, color: "pink", note: "Owned by billing" }, { name: "b", x: 5, y: 5 }]);
});

test("fromFile refuses an unknown colour and a note too long", () => {
  const file = (extra: object) => JSON.stringify({ version: 1, schema: null, tables: [{ name: "a", x: 0, y: 0, ...extra }] });
  assert.throws(() => fromFile(file({ color: "chartreuse" })), /colour/);
  assert.throws(() => fromFile(file({ note: "x".repeat(501) })), /note/);
});
